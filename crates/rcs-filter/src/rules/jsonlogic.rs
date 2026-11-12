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

    /// Interns a normalized address list to a shared `Arc<HashSet<Address>>` with a single keyed
    /// probe. The key is normalized with `sort_unstable()` + `dedup()` — only element identity
    /// matters for set membership, so a stable sort is unnecessary. `entry(..).or_insert_with_key`
    /// then performs one `HashMap` lookup and builds the set once, inside the closure, only on a
    /// miss (the closure receives a borrow of the just-inserted key). A repeated identical list
    /// therefore clones the shared `Arc` without constructing a throwaway set and without a
    /// `get`-then-`insert` double probe.
    fn intern(&mut self, mut addrs: Vec<Address>) -> Arc<HashSet<Address>> {
        addrs.sort_unstable();
        addrs.dedup();
        Arc::clone(
            self.by_addresses
                .entry(addrs)
                .or_insert_with_key(|k| Arc::new(k.iter().copied().collect())),
        )
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

/// Collects every variable name a condition can resolve through `var`, in sorted order.
///
/// The walk is over the raw condition, which is what `compile` is derived from, so it sees every
/// `var` the compiled condition can evaluate — including one retained inside an accelerated
/// membership node. `var` accepts `"name"`, `["name"]` and `["name", default]`
/// (see `eval_var`), and all three forms are collected.
///
/// Used by rule validation to reject a condition that reads a variable the rule's own
/// `direct_delegatecalls` aliases do not provide. Such a name resolves to `null` at evaluation
/// time, and a one-sided null is "not equal", so a negated allowlist over a misspelled variable
/// inverts into an unconditional match instead of failing — which has to be a load-time
/// rejection, not a runtime surprise.
pub(crate) fn collect_var_names(expr: &Value, out: &mut std::collections::BTreeSet<String>) {
    match expr {
        Value::Object(map) => {
            if map.len() == 1
                && let Some(arg) = map.get("var")
            {
                let name = match arg {
                    Value::String(name) => Some(name.as_str()),
                    Value::Array(parts) => parts.first().and_then(Value::as_str),
                    _ => None,
                };
                if let Some(name) = name {
                    out.insert(name.to_string());
                }
                // A `var` node's operands are a name and an optional default, never nested
                // expressions, so there is nothing further to walk under it.
                return;
            }
            for value in map.values() {
                collect_var_names(value, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_var_names(item, out);
            }
        }
        _ => {}
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

#[path = "../../tests/rules__jsonlogic__tests.rs"]
#[cfg(test)]
mod tests;
