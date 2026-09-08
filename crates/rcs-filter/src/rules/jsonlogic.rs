//! Minimal JSONLogic evaluator covering the supported operator subset:
//! `var`, `==`, `!=`, `in`, `and`, `or`, `!`, `>`, `>=`, `<`, `<=`, and the literal `true`.
//!
//! Inputs: a JSONLogic expression as [`serde_json::Value`] plus a bindings map
//! (`variable name → value`). Numeric comparisons coerce both sides to `uint256`
//! (uint256 carried as decimal string / JSON number). Variables absent from the bindings
//! (e.g. a named event with no matching log) resolve to `null`.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use serde_json::Value;

use crate::rules::model::CompiledCondition;

/// Variable bindings for one candidate rule evaluation.
pub type Bindings = HashMap<String, Value>;

/// Evaluates `expr` against `bindings` and returns whether the result is truthy.
pub fn truthy(expr: &Value, bindings: &Bindings) -> bool {
    is_truthy(&eval(expr, bindings))
}

/// Per-`load_rules` interner: identical normalized address lists across the rules of one batch
/// share a single `Arc<HashSet<Address>>`.
#[derive(Default)]
pub(crate) struct AddressSetInterner {
    by_addresses: HashMap<Vec<Address>, Arc<HashSet<Address>>>,
}

impl AddressSetInterner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Looks up by the cheap normalized key and builds the `HashSet` only on a miss, so repeated
    /// identical lists do no redundant set construction — they only clone the shared `Arc`.
    fn intern(&mut self, mut addrs: Vec<Address>) -> Arc<HashSet<Address>> {
        addrs.sort();
        addrs.dedup();
        if let Some(existing) = self.by_addresses.get(&addrs) {
            return Arc::clone(existing);
        }
        let set: Arc<HashSet<Address>> = Arc::new(addrs.iter().copied().collect());
        self.by_addresses.insert(addrs, Arc::clone(&set));
        set
    }
}

/// Returns `Some(Vec<Address>)` iff `haystack` is a non-empty array whose every element is a JSON
/// string parseable to a valid address; otherwise `None` (the node keeps the linear path).
fn static_address_list(haystack: &Value) -> Option<Vec<Address>> {
    let Value::Array(items) = haystack else { return None };
    if items.is_empty() {
        return None;
    }
    items.iter().map(|it| it.as_str().and_then(|s| Address::from_str(s).ok())).collect()
}

/// Compiles a validated JSONLogic expression into a [`CompiledCondition`]. A top-level
/// `{"in":[needle, [addr…]]}` whose right side is a static array of all-valid addresses becomes an
/// O(1) `AddressSetIn` (its set interned); every other node is stored verbatim as `Raw`.
pub(crate) fn compile(expr: &Value, interner: &mut AddressSetInterner) -> CompiledCondition {
    if let Value::Object(map) = expr
        && map.len() == 1
    {
        let (op, arg) = map.iter().next().expect("map has one entry");
        if op == "in"
            && let Value::Array(operands) = arg
            && operands.len() == 2
            && let Some(addrs) = static_address_list(&operands[1])
        {
            let set = interner.intern(addrs);
            return CompiledCondition::AddressSetIn {
                needle: operands[0].clone(),
                set,
                orig: expr.clone(),
            };
        }

        // Specialize a boolean combinator only when its compiled subtree can reach a fast
        // membership node; otherwise it stays a `Raw` leaf. An empty `and`/`or` therefore also
        // stays `Raw`, preserving the original falsy result rather than the `true` an empty
        // `all()` would produce.
        match op.as_str() {
            "and" | "or" => {
                if let Value::Array(items) = arg {
                    let children: Vec<CompiledCondition> =
                        items.iter().map(|it| compile(it, interner)).collect();
                    if children.iter().any(contains_address_set) {
                        return if op == "and" {
                            CompiledCondition::And(children)
                        } else {
                            CompiledCondition::Or(children)
                        };
                    }
                }
            }
            "!" => {
                // The `!` operand is `x` or `[x]`; reuse the shared single-operand extraction.
                let inner = first_operand(arg);
                let child = compile(&inner, interner);
                if contains_address_set(&child) {
                    return CompiledCondition::Not(Box::new(child));
                }
            }
            _ => {}
        }
    }
    CompiledCondition::Raw(expr.clone())
}

/// Whether a compiled subtree contains at least one accelerable membership node. A boolean
/// combinator is specialized only when doing so can actually reach such a node; otherwise it stays
/// a `Raw` leaf, which also preserves the falsy result of an empty `and`/`or`.
fn contains_address_set(c: &CompiledCondition) -> bool {
    match c {
        CompiledCondition::AddressSetIn { .. } => true,
        CompiledCondition::And(xs) | CompiledCondition::Or(xs) => {
            xs.iter().any(contains_address_set)
        }
        CompiledCondition::Not(x) => contains_address_set(x),
        CompiledCondition::Raw(_) => false,
    }
}

/// Evaluates a compiled condition to a boolean, mirroring [`truthy`] exactly. `Raw` defers to the
/// raw-value evaluator; the specialized arms combine truthiness the same way the raw evaluator does
/// for non-empty operand lists.
pub(crate) fn eval_compiled(node: &CompiledCondition, b: &Bindings) -> bool {
    match node {
        CompiledCondition::Raw(v) => truthy(v, b),
        CompiledCondition::AddressSetIn { needle, set, orig } => match eval(needle, b) {
            Value::String(s) => match Address::from_str(&s) {
                Ok(addr) => set.contains(&addr),
                Err(_) => truthy(orig, b),
            },
            _ => truthy(orig, b),
        },
        CompiledCondition::And(xs) => xs.iter().all(|x| eval_compiled(x, b)),
        CompiledCondition::Or(xs) => xs.iter().any(|x| eval_compiled(x, b)),
        CompiledCondition::Not(x) => !eval_compiled(x, b),
    }
}

/// Validates the supported JSONLogic subset and operator arity at rule-load time.
pub(crate) fn validate(expr: &Value) -> Result<(), String> {
    match expr {
        Value::Object(map) => {
            if map.len() != 1 {
                return Err("JSONLogic expression objects must contain exactly one operator".into());
            }
            let (op, arg) = map.iter().next().expect("validated non-empty map");
            let operands = match arg {
                Value::Array(items) => items.as_slice(),
                other => std::slice::from_ref(other),
            };
            match op.as_str() {
                "var" => {
                    if !(1..=2).contains(&operands.len()) || !operands[0].is_string() {
                        return Err("JSONLogic 'var' expects a name and optional default".into());
                    }
                }
                "==" | "!=" | "in" | ">" | ">=" | "<" | "<=" => {
                    if operands.len() != 2 {
                        return Err(format!("JSONLogic '{op}' expects exactly two operands"));
                    }
                }
                "!" => {
                    if operands.len() != 1 {
                        return Err("JSONLogic '!' expects exactly one operand".into());
                    }
                }
                "and" | "or" => {
                    if !arg.is_array() {
                        return Err(format!("JSONLogic '{op}' expects an operand array"));
                    }
                }
                _ => return Err(format!("unsupported JSONLogic operator '{op}'")),
            }
            for operand in operands {
                validate(operand)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                validate(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// JSONLogic truthiness: `false`/`null`/`0`/empty-string/empty-array are falsy.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

/// Recursively evaluates a JSONLogic expression.
fn eval(expr: &Value, b: &Bindings) -> Value {
    match expr {
        Value::Object(map) if map.len() == 1 => {
            // Safe: len == 1 guarantees a first entry.
            let (op, arg) = map.iter().next().expect("map has one entry");
            eval_op(op, arg, b)
        }
        Value::Array(items) => Value::Array(items.iter().map(|e| eval(e, b)).collect()),
        other => other.clone(),
    }
}

/// Dispatches a single JSONLogic operator.
fn eval_op(op: &str, arg: &Value, b: &Bindings) -> Value {
    match op {
        "var" => eval_var(arg, b),
        "==" => Value::Bool(value_eq(&two(arg, b))),
        "!=" => Value::Bool(!value_eq(&two(arg, b))),
        "in" => Value::Bool(eval_in(arg, b)),
        "and" => eval_and(arg, b),
        "or" => eval_or(arg, b),
        "!" => Value::Bool(!is_truthy(&eval(&first_operand(arg), b))),
        ">" => cmp(arg, b, |o| o == std::cmp::Ordering::Greater),
        ">=" => cmp(arg, b, |o| o != std::cmp::Ordering::Less),
        "<" => cmp(arg, b, |o| o == std::cmp::Ordering::Less),
        "<=" => cmp(arg, b, |o| o != std::cmp::Ordering::Greater),
        _ => Value::Null,
    }
}

/// Resolves a `var` reference: `"name"`, `["name"]`, or `["name", default]`.
fn eval_var(arg: &Value, b: &Bindings) -> Value {
    match arg {
        Value::String(name) => b.get(name).cloned().unwrap_or(Value::Null),
        Value::Array(parts) => {
            let name = parts.first().and_then(Value::as_str);
            match name.and_then(|n| b.get(n)) {
                Some(v) => v.clone(),
                None => parts.get(1).cloned().unwrap_or(Value::Null),
            }
        }
        _ => Value::Null,
    }
}

/// Evaluates the two operands of a binary operator (`[a, b]`).
fn two(arg: &Value, b: &Bindings) -> (Value, Value) {
    if let Value::Array(items) = arg {
        let l = items.first().map(|e| eval(e, b)).unwrap_or(Value::Null);
        let r = items.get(1).map(|e| eval(e, b)).unwrap_or(Value::Null);
        (l, r)
    } else {
        (eval(arg, b), Value::Null)
    }
}

/// The single operand for unary operators like `!` (`x` or `[x]`).
fn first_operand(arg: &Value) -> Value {
    match arg {
        Value::Array(items) => items.first().cloned().unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// Equality with numeric coercion and case-insensitive comparison only for valid addresses.
fn value_eq((l, r): &(Value, Value)) -> bool {
    if l.is_null() || r.is_null() {
        return l.is_null() && r.is_null();
    }
    if let (Some(lu), Some(ru)) = (to_u256(l), to_u256(r)) {
        return lu == ru;
    }
    if let (Some(ls), Some(rs)) = (l.as_str(), r.as_str())
        && let (Ok(la), Ok(ra)) = (Address::from_str(ls), Address::from_str(rs))
    {
        return la == ra;
    }
    l == r
}

/// `in` membership: `[needle, haystack]` where haystack is an array (blacklist/whitelist)
/// or a string (substring).
fn eval_in(arg: &Value, b: &Bindings) -> bool {
    let (needle, hay) = two(arg, b);
    match hay {
        Value::Array(items) => items.iter().any(|item| value_eq(&(needle.clone(), item.clone()))),
        Value::String(s) => needle.as_str().map(|n| s.contains(n)).unwrap_or(false),
        _ => false,
    }
}

/// JSONLogic `and` returns the first falsy operand, or the last operand.
fn eval_and(arg: &Value, b: &Bindings) -> Value {
    let Value::Array(items) = arg else { return Value::Null };
    let mut last = Value::Null;
    for item in items {
        last = eval(item, b);
        if !is_truthy(&last) {
            return last;
        }
    }
    last
}

/// JSONLogic `or` returns the first truthy operand, or the last operand.
fn eval_or(arg: &Value, b: &Bindings) -> Value {
    let Value::Array(items) = arg else { return Value::Null };
    let mut last = Value::Null;
    for item in items {
        last = eval(item, b);
        if is_truthy(&last) {
            return last;
        }
    }
    last
}

/// Numeric comparison. Returns `false` when either side is not uint256-coercible
/// (e.g. a `null` variable), never panicking.
fn cmp(arg: &Value, b: &Bindings, pass: impl Fn(std::cmp::Ordering) -> bool) -> Value {
    let (l, r) = two(arg, b);
    match (to_u256(&l), to_u256(&r)) {
        (Some(lu), Some(ru)) => Value::Bool(pass(lu.cmp(&ru))),
        _ => Value::Bool(false),
    }
}

/// Best-effort uint256 parse from a JSON number or decimal/hex string. `None` when the
/// value cannot represent an integer (so numeric operators treat it as "no comparison").
fn to_u256(v: &Value) -> Option<U256> {
    match v {
        Value::Number(n) => U256::from_str(&n.to_string()).ok(),
        Value::String(s) => {
            let s = s.trim();
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                U256::from_str_radix(hex, 16).ok()
            } else {
                U256::from_str(s).ok()
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binds(pairs: &[(&str, Value)]) -> Bindings {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn compiled_matches_truthy_for_all_shapes() {
        let mut interner = AddressSetInterner::new();
        let a = "0x0101010101010101010101010101010101010101";
        let b = "0x0202020202020202020202020202020202020202";
        let conds = [
            json!({"in": [{"var": "origin"}, [a, b]]}),
            json!({"in": [{"var": "origin"}, "0xdead"]}), // substring
            json!({"==": [{"var": "origin"}, a]}),
            json!({"and": [{"in": [{"var": "origin"}, [a]]}, {"==": [{"var": "x"}, 1]}]}),
            json!({"or":  [{"in": [{"var": "origin"}, [a]]}, {"==": [{"var": "x"}, 1]}]}),
            json!({"!": {"in": [{"var": "origin"}, [a]]}}),
            json!({"and": []}),
            json!({"or":  []}),
        ];
        let binding_sets = [
            binds(&[("origin", json!(a))]),
            binds(&[("origin", json!(b))]),
            binds(&[("origin", json!("0xdeadbeef")), ("x", json!(1))]),
            Bindings::new(),
        ];
        for cond in &conds {
            assert!(validate(cond).is_ok());
            let compiled = compile(cond, &mut interner);
            for bset in &binding_sets {
                assert_eq!(eval_compiled(&compiled, bset), truthy(cond, bset), "cond={cond}");
            }
        }
    }

    const A: &str = "0x0101010101010101010101010101010101010101";
    const B: &str = "0x0202020202020202020202020202020202020202";

    #[test]
    fn recognizes_only_static_all_address_arrays() {
        let mut i = AddressSetInterner::new();
        assert!(matches!(
            compile(&json!({"in": [{"var":"origin"}, [A, B]]}), &mut i),
            CompiledCondition::AddressSetIn { .. }
        ));
        // non-accelerable → Raw
        for cond in [
            json!({"in": [{"var":"origin"}, [A, 1]]}), // mixed
            json!({"in": [{"var":"origin"}, [A, "not-an-address"]]}), // bad element
            json!({"in": [{"var":"origin"}, []]}),     // empty
            json!({"in": [{"var":"origin"}, {"var":"list"}]}), // dynamic RHS
            json!({"in": [{"var":"origin"}, "0xdead"]}), // substring
            json!({"in": [1, [1, 2]]}),                // numeric array
        ] {
            assert!(matches!(compile(&cond, &mut i), CompiledCondition::Raw(_)), "cond={cond}");
        }
    }

    #[test]
    fn fast_path_hit_miss_case_and_duplicates_match_linear() {
        let mut i = AddressSetInterner::new();
        // checksummed + lowercase + duplicate entries in the list
        let cond = json!({"in": [{"var":"origin"}, ["0x52908400098527886E0F7030069857D2E4169EE7",
                                                     "0x52908400098527886e0f7030069857d2e4169ee7", B]]});
        let compiled = compile(&cond, &mut i);
        for probe in [json!("0x52908400098527886e0f7030069857d2e4169ee7"), json!(B), json!(A)] {
            let bset = binds(&[("origin", probe.clone())]);
            assert_eq!(eval_compiled(&compiled, &bset), truthy(&cond, &bset), "probe={probe}");
        }
    }

    #[test]
    fn non_address_needle_falls_back_to_linear() {
        let mut i = AddressSetInterner::new();
        let cond = json!({"in": [{"var":"origin"}, [A, B]]});
        let compiled = compile(&cond, &mut i);
        for probe in [Value::Null, json!(123), json!("not-an-address")] {
            let bset = binds(&[("origin", probe.clone())]);
            assert_eq!(eval_compiled(&compiled, &bset), truthy(&cond, &bset), "probe={probe}");
        }
        // missing var → null needle
        assert_eq!(eval_compiled(&compiled, &Bindings::new()), truthy(&cond, &Bindings::new()));
    }

    #[test]
    fn identical_normalized_lists_share_one_arc() {
        let mut i = AddressSetInterner::new();
        // same set, different order + case + a duplicate
        let c1 = compile(&json!({"in": [{"var":"origin"}, [A, B]]}), &mut i);
        let c2 = compile(&json!({"in": [{"var":"origin"}, [B, A, A]]}), &mut i);
        let c3 = compile(&json!({"in": [{"var":"origin"}, [A]]}), &mut i); // different set
        let get = |c: &CompiledCondition| match c {
            CompiledCondition::AddressSetIn { set, .. } => set.clone(),
            _ => panic!("expected AddressSetIn"),
        };
        assert!(Arc::ptr_eq(&get(&c1), &get(&c2)));
        assert!(!Arc::ptr_eq(&get(&c1), &get(&c3)));
    }

    #[test]
    fn combinators_with_accelerable_child_are_specialized_and_exact() {
        let mut i = AddressSetInterner::new();
        let a = A;
        for cond in [
            json!({"and": [{"in": [{"var":"origin"}, [a]]}, {"==": [{"var":"x"}, 1]}]}),
            json!({"or":  [{"==": [{"var":"x"}, 1]}, {"in": [{"var":"origin"}, [a]]}]}),
            json!({"!": {"in": [{"var":"origin"}, [a]]}}),
        ] {
            let compiled = compile(&cond, &mut i);
            assert!(!matches!(compiled, CompiledCondition::Raw(_)), "should specialize: {cond}");
            assert!(contains_address_set(&compiled));
            for bset in [
                binds(&[("origin", json!(a)), ("x", json!(1))]),
                binds(&[("origin", json!(B)), ("x", json!(2))]),
                Bindings::new(),
            ] {
                assert_eq!(eval_compiled(&compiled, &bset), truthy(&cond, &bset), "cond={cond}");
            }
        }
    }

    #[test]
    fn empty_and_or_and_non_accelerable_combinators_stay_raw_and_false() {
        let mut i = AddressSetInterner::new();
        for cond in [json!({"and": []}), json!({"or": []})] {
            let compiled = compile(&cond, &mut i);
            assert!(
                matches!(compiled, CompiledCondition::Raw(_)),
                "empty combinator must stay Raw: {cond}"
            );
            assert!(!eval_compiled(&compiled, &Bindings::new())); // false, matches eval→Null
        }
        // combinator with no accelerable child stays Raw
        let cond = json!({"and": [{"==": [{"var":"x"}, 1]}, {"==": [{"var":"y"}, 2]}]});
        assert!(matches!(compile(&cond, &mut i), CompiledCondition::Raw(_)));
    }

    #[test]
    fn literal_true_is_truthy() {
        assert!(truthy(&json!(true), &Bindings::new()));
    }

    #[test]
    fn empty_and_or_are_valid_and_falsy() {
        for condition in [json!({"and": []}), json!({"or": []})] {
            assert!(validate(&condition).is_ok());
            assert_eq!(eval(&condition, &Bindings::new()), Value::Null);
            assert!(!truthy(&condition, &Bindings::new()));
        }
    }

    #[test]
    fn eq_and_and_match_scenario_a() {
        // condition a: transfer.address == TOKEN_X AND transfer.from == BRIDGE
        let cond = json!({"and": [
            {"==": [{"var": "transfer.address"}, "0x0202020202020202020202020202020202020202"]},
            {"==": [{"var": "transfer.from"},    "0x0101010101010101010101010101010101010101"]}
        ]});
        let b = binds(&[
            ("transfer.address", json!("0x0202020202020202020202020202020202020202")),
            ("transfer.from", json!("0x0101010101010101010101010101010101010101")),
        ]);
        assert!(truthy(&cond, &b));
    }

    #[test]
    fn in_list_blacklist() {
        let cond = json!({"in": [{"var": "transfer.from"}, ["0x0606060606060606060606060606060606060606"]]});
        let hit = binds(&[("transfer.from", json!("0x0606060606060606060606060606060606060606"))]);
        let miss = binds(&[("transfer.from", json!("0x0101010101010101010101010101010101010101"))]);
        assert!(truthy(&cond, &hit));
        assert!(!truthy(&cond, &miss));
    }

    #[test]
    fn numeric_gt_18_digit_precision() {
        // one token = 1e18; ensure big-int comparison is exact (no f64 loss).
        let cond = json!({">": [{"var": "transfer.value"}, 1000000000000000000u64]});
        let over = binds(&[("transfer.value", json!("2000000000000000000"))]);
        let equal = binds(&[("transfer.value", json!("1000000000000000000"))]);
        assert!(truthy(&cond, &over));
        assert!(!truthy(&cond, &equal)); // strictly greater
    }

    #[test]
    fn missing_var_is_null_and_comparisons_are_false() {
        let cond = json!({"==": [{"var": "transfer.from"}, "0x01"]});
        assert!(!truthy(&cond, &Bindings::new())); // null != literal
        let gt = json!({">": [{"var": "transfer.value"}, 1]});
        assert!(!truthy(&gt, &Bindings::new())); // null comparison → false
    }

    #[test]
    fn negation_and_neq() {
        let neq = json!({"!=": [{"var": "origin"}, "0x01"]});
        assert!(truthy(&neq, &binds(&[("origin", json!("0x02"))])));
        let not = json!({"!": {"==": [{"var": "origin"}, "0x01"]}});
        assert!(truthy(&not, &binds(&[("origin", json!("0x02"))])));
    }

    #[test]
    fn nested_and_or_return_operands() {
        let cond = json!({"==": [
            {"and": [true, {"or": [false, "selected"]}]},
            "selected"
        ]});
        assert!(truthy(&cond, &Bindings::new()));
    }

    #[test]
    fn ordinary_strings_are_case_sensitive_but_addresses_are_not() {
        assert!(!truthy(&json!({"==": ["Quota", "quota"]}), &Bindings::new()));
        assert!(truthy(
            &json!({"==": [
                "0x52908400098527886E0F7030069857D2E4169EE7",
                "0x52908400098527886e0f7030069857d2e4169ee7"
            ]}),
            &Bindings::new()
        ));
    }

    #[test]
    fn validates_all_supported_operators_and_rejects_bad_shapes() {
        for expression in [
            json!({"var": "x"}),
            json!({"==": [1, 1]}),
            json!({"!=": [1, 2]}),
            json!({"in": [1, [1]]}),
            json!({"and": [true, true]}),
            json!({"or": [false, true]}),
            json!({"!": true}),
            json!({">": [2, 1]}),
            json!({">=": [2, 1]}),
            json!({"<": [1, 2]}),
            json!({"<=": [1, 2]}),
        ] {
            validate(&expression).expect("supported expression");
        }
        for expression in [
            json!({"unknown": [1]}),
            json!({"==": [1]}),
            json!({"!": [true, false]}),
            json!({"and": true}),
            json!({"or": false}),
        ] {
            assert!(validate(&expression).is_err(), "accepted {expression}");
        }
    }

    #[test]
    fn u256_boundaries_compare_exactly() {
        let max = U256::MAX.to_string();
        let below = (U256::MAX - U256::from(1)).to_string();
        assert!(truthy(&json!({">": [max, below]}), &Bindings::new()));
        assert!(truthy(&json!({"<=": ["0", max]}), &Bindings::new()));
    }
}
