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
    // Merge audit rules at the physical-log level. Multiple rules can match the same log, but
    // RCS must receive that event only once per audit type or it would account the same action
    // multiple times.
    let mut actions_by_log: BTreeMap<String, BTreeMap<usize, ActionItem>> = BTreeMap::new();
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
                    let by_log = actions_by_log.entry(audit_type.clone()).or_default();
                    for (log_index, item) in &items {
                        by_log.entry(*log_index).or_insert_with(|| item.clone());
                    }
                }
            }
        }
    }

    if has_audit {
        let actions = actions_by_log
            .into_iter()
            .map(|(audit_type, by_log)| (audit_type, by_log.into_values().collect()))
            .collect();
        MatchOutcome::Audit { actions, timeout_action }
    } else {
        MatchOutcome::Allow
    }
}

/// A matched named event and the physical transaction log that produced it.
struct MatchedEvent {
    log_index: usize,
    address: String,
    params: BTreeMap<String, String>,
}

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
            Some((log_index, log)) => {
                let params = decode_event(event, log);
                let addr = addr_lower(log.address);
                for (pname, pval) in &params {
                    b.insert(format!("{}.{}", event.var_name, pname), Value::String(pval.clone()));
                }
                b.insert(format!("{}.address", event.var_name), Value::String(addr.clone()));
                matched.insert(
                    event.var_name.clone(),
                    MatchedEvent { log_index, address: addr, params },
                );
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

/// Turns the matched event set into indexed submit items so cross-rule merging can identify
/// multiple matches of the same physical log.
fn build_action_items(matched: &BTreeMap<String, MatchedEvent>) -> Vec<(usize, ActionItem)> {
    matched
        .iter()
        .map(|(name, event)| {
            (
                event.log_index,
                ActionItem {
                    name: name.clone(),
                    address: event.address.clone(),
                    params: event.params.clone(),
                },
            )
        })
        .collect()
}

/// Finds the first log matching an event's `topic0` (contract §3.2: first log only, no
/// multi-log pairing).
fn find_log_for_event<'a>(event: &CompiledEvent, logs: &'a [Log]) -> Option<(usize, &'a Log)> {
    logs.iter().enumerate().find(|(_, log)| log.data.topics().first() == Some(&event.topic0))
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
    use crate::rules::{load_rules, RawRule};
    use crate::test_support::{golden, log_builder};
    use alloy_primitives::{keccak256, Bytes, LogData};
    use serde_json::json;

    fn ruleset_a() -> RuleSet {
        load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()])
    }

    fn audit_rule(
        id: &str,
        event_alias: &str,
        event_name: &str,
        inputs: Value,
        audit_type: &str,
    ) -> RawRule {
        let mut event_abis = serde_json::Map::new();
        event_abis.insert(
            event_alias.to_string(),
            json!({
                "type": "event",
                "name": event_name,
                "inputs": inputs,
                "anonymous": false
            }),
        );
        serde_json::from_value(json!({
            "id": id,
            "event_abis": event_abis,
            "audit_types": [audit_type],
            "condition": true,
            "action": "audit"
        }))
        .unwrap()
    }

    fn erc20_audit_rule(id: &str, event_alias: &str) -> RawRule {
        audit_rule(
            id,
            event_alias,
            "Transfer",
            json!([
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ]),
            "quota",
        )
    }

    fn empty_event_log(address: Address, event_name: &str) -> Log {
        let topic0 = keccak256(format!("{event_name}()").as_bytes());
        Log { address, data: LogData::new_unchecked(vec![topic0], Bytes::new()) }
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
    fn same_log_matching_multiple_audit_rules_is_submitted_once_per_type() {
        let rules = load_rules(
            1,
            1,
            vec![
                erc20_audit_rule("quota-a", "first_transfer"),
                erc20_audit_rule("quota-b", "second_transfer"),
            ],
        );
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
                assert_eq!(quota[0].name, "first_transfer");
                assert_eq!(quota[0].params.get("value").unwrap(), golden::ONE_TOKEN);
            }
            other => panic!("expected Audit, got {other:?}"),
        }
    }

    #[test]
    fn distinct_logs_in_the_same_audit_type_are_preserved() {
        let rules = load_rules(
            1,
            1,
            vec![
                audit_rule("custom-a", "event_a", "EventA", json!([]), "custom"),
                audit_rule("custom-b", "event_b", "EventB", json!([]), "custom"),
            ],
        );
        let logs = vec![
            empty_event_log(golden::token_x(), "EventA"),
            empty_event_log(golden::claim_contract(), "EventB"),
        ];
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
                let custom = actions.get("custom").expect("custom key");
                assert_eq!(custom.len(), 2);
                assert_eq!(custom[0].name, "event_a");
                assert_eq!(custom[1].name, "event_b");
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
