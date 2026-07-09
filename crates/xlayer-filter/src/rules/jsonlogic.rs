//! Minimal JSONLogic evaluator covering the contract §3.5 operator subset:
//! `var`, `==`, `!=`, `in`, `and`, `or`, `!`, `>`, `>=`, `<`, `<=`, and the literal `true`.
//!
//! Inputs: a JSONLogic expression as [`serde_json::Value`] plus a bindings map
//! (`variable name → value`). Numeric comparisons coerce both sides to `uint256`
//! (contract §3.5: uint256 carried as decimal string / JSON number). Variables absent from
//! the bindings (e.g. a named event with no matching log) resolve to `null` (contract §3.2).

use std::collections::HashMap;
use std::str::FromStr;

use alloy_primitives::U256;
use serde_json::Value;

/// Variable bindings for one candidate rule evaluation.
pub type Bindings = HashMap<String, Value>;

/// Evaluates `expr` against `bindings` and returns whether the result is truthy.
pub fn truthy(expr: &Value, bindings: &Bindings) -> bool {
    is_truthy(&eval(expr, bindings))
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
        "and" => Value::Bool(eval_all(arg, b)),
        "or" => Value::Bool(eval_any(arg, b)),
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

/// Equality with numeric coercion: if both sides parse as uint256, compare numerically;
/// otherwise compare normalized (lower-cased) string / structural values. `null == null`.
fn value_eq((l, r): &(Value, Value)) -> bool {
    if l.is_null() || r.is_null() {
        return l.is_null() && r.is_null();
    }
    if let (Some(lu), Some(ru)) = (to_u256(l), to_u256(r)) {
        return lu == ru;
    }
    normalize(l) == normalize(r)
}

/// `in` membership: `[needle, haystack]` where haystack is an array (blacklist/whitelist)
/// or a string (substring). Contract §3.5.
fn eval_in(arg: &Value, b: &Bindings) -> bool {
    let (needle, hay) = two(arg, b);
    match hay {
        Value::Array(items) => items.iter().any(|item| value_eq(&(needle.clone(), item.clone()))),
        Value::String(s) => needle.as_str().map(|n| s.contains(n)).unwrap_or(false),
        _ => false,
    }
}

/// `and`: all sub-expressions truthy.
fn eval_all(arg: &Value, b: &Bindings) -> bool {
    match arg {
        Value::Array(items) => items.iter().all(|e| is_truthy(&eval(e, b))),
        other => is_truthy(&eval(other, b)),
    }
}

/// `or`: any sub-expression truthy.
fn eval_any(arg: &Value, b: &Bindings) -> bool {
    match arg {
        Value::Array(items) => items.iter().any(|e| is_truthy(&eval(e, b))),
        other => is_truthy(&eval(other, b)),
    }
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

/// Lower-cased string form for non-numeric equality (addresses are compared case-insensitively).
fn normalize(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_ascii_lowercase(),
        other => other.to_string(),
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
    fn literal_true_is_truthy() {
        assert!(truthy(&json!(true), &Bindings::new()));
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
}
