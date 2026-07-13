//! Per-transaction screening algorithm (FR-4, TD §4.6): filter-skip (zero-decode) →
//! topic0 candidate lookup → event decode → JSONLogic eval → action merge
//! (`deny > audit > allow`, both intra-log and cross-log).

use std::collections::{BTreeMap, BTreeSet};

use alloy_dyn_abi::{DynSolEvent, DynSolType, DynSolValue};
use alloy_primitives::{Address, Log};
use serde_json::Value;

use crate::client::ActionItem;
use crate::handle::ScreenInput;
use crate::rules::{truthy, Action, CompiledEvent, CompiledRule, RuleSet, TimeoutAction};

/// Outcome of matching one transaction against a rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchOutcome {
    /// No rule requires action → package normally.
    Allow,
    /// At least one `deny` rule matched → drop the whole tx immediately.
    Deny,
    /// One or more `audit` rules matched (no deny) → submit to RCS.
    Audit {
        /// `{ audit_type: [ActionItem] }` grouped for the submit payload.
        actions: BTreeMap<String, Vec<ActionItem>>,
        /// Merged timeout fallback across matched audit rules (deny is stricter).
        timeout_action: TimeoutAction,
    },
}

/// Runs the full screening algorithm for one transaction. Zero network IO.
pub fn evaluate(rules: &RuleSet, input: &ScreenInput) -> MatchOutcome {
    let mut actions: BTreeMap<String, Vec<ActionItem>> = BTreeMap::new();
    let mut has_audit = false;
    let mut timeout_action = TimeoutAction::Allow;

    for &idx in &candidate_rule_indices(rules, input.logs) {
        let rule = &rules.rules[idx];

        // Stage one: filter-skip (contract §3.1, zero decode).
        if let Some(ca) = rule.contract_address
            && input.tx_to != Some(ca)
        {
            continue;
        }
        if let Some(o) = rule.origin
            && input.origin != o
        {
            continue;
        }

        // Stage two: build variable bindings (decode matched logs; null for unmatched events).
        let (bindings, matched) = build_bindings(rule, input);
        if !truthy(&rule.condition, &bindings) {
            continue;
        }

        // Stage three: action merge with deny short-circuit.
        match rule.action {
            Action::Deny => return MatchOutcome::Deny,
            Action::Allow => { /* allow never overrides deny/audit; no state change */ }
            Action::Audit => {
                has_audit = true;
                timeout_action = timeout_action.stricter(rule.audit_timeout_action);
                let items = build_action_items(&matched);
                for audit_type in &rule.audit_types {
                    actions.entry(audit_type.clone()).or_default().extend(items.clone());
                }
            }
        }
    }

    if has_audit {
        MatchOutcome::Audit { actions, timeout_action }
    } else {
        MatchOutcome::Allow
    }
}

/// A matched named event: the triggering `log.address` and its decoded named params.
type MatchedEvent = (String, BTreeMap<String, String>);

/// Deduplicated candidate rule indices across all logs (topic0 index lookup), sorted for
/// deterministic deny short-circuit order.
fn candidate_rule_indices(rules: &RuleSet, logs: &[Log]) -> Vec<usize> {
    let mut set = BTreeSet::new();
    for log in logs {
        if let Some(topic0) = log.data.topics().first() {
            for &idx in rules.candidates_for_topic0(topic0) {
                set.insert(idx);
            }
        }
    }
    set.into_iter().collect()
}

/// Builds JSONLogic bindings for a rule and the per-event decoded match set. Tx-level vars
/// (`contract_address`, `origin`, `value`, `nonce`) plus `<name>.<param>` / `<name>.address`
/// per declared event; unmatched events bind all their variables to `null` (contract §3.2).
fn build_bindings(
    rule: &CompiledRule,
    input: &ScreenInput,
) -> (crate::rules::Bindings, BTreeMap<String, MatchedEvent>) {
    let mut b = crate::rules::Bindings::new();
    b.insert(
        "contract_address".to_string(),
        input.tx_to.map(addr_lower).map(Value::String).unwrap_or(Value::Null),
    );
    b.insert("origin".to_string(), Value::String(addr_lower(input.origin)));
    b.insert("value".to_string(), Value::String(input.value.to_string()));
    b.insert("nonce".to_string(), Value::Number(input.nonce.into()));

    let mut matched: BTreeMap<String, MatchedEvent> = BTreeMap::new();

    for event in &rule.events {
        match find_log_for_event(event, input.logs) {
            Some(log) => {
                let params = decode_event(event, log);
                let addr = addr_lower(log.address);
                for (pname, pval) in &params {
                    b.insert(format!("{}.{}", event.var_name, pname), Value::String(pval.clone()));
                }
                b.insert(format!("{}.address", event.var_name), Value::String(addr.clone()));
                matched.insert(event.var_name.clone(), (addr, params));
            }
            None => {
                for input_def in &event.inputs {
                    b.insert(format!("{}.{}", event.var_name, input_def.name), Value::Null);
                }
                b.insert(format!("{}.address", event.var_name), Value::Null);
            }
        }
    }

    (b, matched)
}

/// Turns the matched event set into submit `ActionItem`s (one per matched named event).
fn build_action_items(matched: &BTreeMap<String, MatchedEvent>) -> Vec<ActionItem> {
    matched
        .iter()
        .map(|(name, (address, params))| ActionItem {
            name: name.clone(),
            address: address.clone(),
            params: params.clone(),
        })
        .collect()
}

/// Finds the first log matching an event's `topic0` (contract §3.2: first log only, no
/// multi-log pairing).
fn find_log_for_event<'a>(event: &CompiledEvent, logs: &'a [Log]) -> Option<&'a Log> {
    logs.iter().find(|log| log.data.topics().first() == Some(&event.topic0))
}

/// ABI-decodes a matched log into `{ param_name: string_value }` following the event's
/// declared input order (indexed params from topics, the rest from data).
fn decode_event(event: &CompiledEvent, log: &Log) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();

    let indexed_types: Vec<DynSolType> =
        event.inputs.iter().filter(|i| i.indexed).map(|i| i.sol_type.clone()).collect();
    let body_types: Vec<DynSolType> =
        event.inputs.iter().filter(|i| !i.indexed).map(|i| i.sol_type.clone()).collect();

    let topic0 = if event.anonymous { None } else { Some(event.topic0) };
    let decoder = DynSolEvent::new_unchecked(topic0, indexed_types, DynSolType::Tuple(body_types));

    let decoded = match decoder.decode_log_parts(log.data.topics().iter().copied(), &log.data.data)
    {
        Ok(d) => d,
        Err(_) => return out,
    };

    // Reassemble in declared input order.
    let mut idx_indexed = 0usize;
    let mut idx_body = 0usize;
    for input_def in &event.inputs {
        let value = if input_def.indexed {
            let v = decoded.indexed.get(idx_indexed);
            idx_indexed += 1;
            v
        } else {
            let v = decoded.body.get(idx_body);
            idx_body += 1;
            v
        };
        if let Some(v) = value {
            out.insert(input_def.name.clone(), dyn_value_to_string(v));
        }
    }
    out
}

/// Renders a decoded ABI value as a canonical string: addresses lower-cased hex, uint/int
/// decimal, bool literal. These are the types quota shapes and conditions use (contract §3.3/§3.5).
fn dyn_value_to_string(v: &DynSolValue) -> String {
    match v {
        DynSolValue::Address(a) => addr_lower(*a),
        DynSolValue::Uint(u, _) => u.to_string(),
        DynSolValue::Int(i, _) => i.to_string(),
        DynSolValue::Bool(b) => b.to_string(),
        DynSolValue::String(s) => s.clone(),
        DynSolValue::FixedBytes(w, _) => format!("{w:#x}"),
        DynSolValue::Bytes(b) => format!("0x{}", alloy_primitives::hex::encode(b)),
        other => format!("{other:?}"),
    }
}

/// Lower-cased `0x`-prefixed hex form of an address (no EIP-55 checksum, contract §4.8).
fn addr_lower(a: Address) -> String {
    format!("{a:#x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::load_rules;
    use crate::test_support::{golden, log_builder};

    fn ruleset_a() -> RuleSet {
        load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()])
    }

    #[test]
    fn scenario_a_matches_audit_with_quota_item() {
        let rules = ruleset_a();
        let logs = vec![log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
        };
        match evaluate(&rules, &input) {
            MatchOutcome::Audit { actions, .. } => {
                let quota = actions.get("quota").expect("quota key");
                assert_eq!(quota.len(), 1);
                assert_eq!(quota[0].name, "transfer");
                assert_eq!(quota[0].address, golden::TOKEN_X);
                assert_eq!(quota[0].params.get("value").unwrap(), golden::ONE_TOKEN);
                assert_eq!(quota[0].params.get("from").unwrap(), golden::BRIDGE_ERC20);
            }
            other => panic!("expected Audit, got {other:?}"),
        }
    }

    #[test]
    fn scenario_b_deny_short_circuits() {
        let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_B).unwrap()]);
        let logs = vec![log_builder::erc20_transfer(
            golden::token_x(),
            golden::blacklisted_from(),
            golden::recipient(),
            golden::one_token(),
        )];
        let input = ScreenInput {
            tx_hash: golden::tx_b(),
            origin: golden::origin_b(),
            tx_to: Some(golden::claim_contract()),
            nonce: 0,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
        };
        assert_eq!(evaluate(&rules, &input), MatchOutcome::Deny);
    }

    #[test]
    fn no_matching_log_allows() {
        let rules = ruleset_a();
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &[],
        };
        assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
    }
}
