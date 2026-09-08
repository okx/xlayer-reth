//! Rule loading, per-rule validation, and topic0 index construction.
//!
//! Validation is **per-rule**: an invalid rule is rejected whole (not field-ignored, not
//! whole-package-failed) and logged at `warn` with its id and reason; all other rules stay
//! in effect.

use std::str::FromStr;

use alloy_dyn_abi::DynSolType;
use alloy_primitives::{keccak256, Address};
use tracing::warn;

use super::jsonlogic::{compile, AddressSetInterner};
use super::model::{
    Action, CompiledEvent, CompiledInput, CompiledRule, EventAbi, RawRule, RejectedRule, RuleSet,
    TimeoutAction,
};

/// Fixed ERC20 `Transfer` quota shape (ordered input names).
const ERC20_TRANSFER_SHAPE: &[&str] = &["from", "to", "value"];
/// Fixed ERC1155 `TransferSingle` quota shape (ordered input names).
const ERC1155_TRANSFER_SINGLE_SHAPE: &[&str] = &["operator", "from", "to", "id", "value"];
/// ERC1155 `TransferBatch` quota shape advertised by the RCS rule ABI.
const ERC1155_TRANSFER_BATCH_SHAPE: &[&str] = &["operator", "from", "to", "ids", "values"];
/// The only audit type implemented today.
const QUOTA: &str = "quota";

/// Loads and validates a batch of raw rules into an immutable [`RuleSet`] snapshot with a
/// topic0 index. Rejected rules are dropped with a `warn`; the returned set contains only
/// rules that passed validation.
pub fn load_rules(protocol_version: u32, content_version: u64, raw: Vec<RawRule>) -> RuleSet {
    let mut id_counts = std::collections::HashMap::new();
    for rule in &raw {
        *id_counts.entry(rule.id.clone()).or_insert(0usize) += 1;
    }

    let mut rules = Vec::new();
    let mut rejected = Vec::new();
    // One interner per batch so identical normalized address lists across rules share one `Arc`.
    let mut interner = AddressSetInterner::new();
    for rule in raw {
        let id = rule.id.clone();
        if id_counts.get(&id).copied().unwrap_or_default() > 1 {
            let reason = "duplicate rule id".to_string();
            warn!(target: "rcs_filter", rule_id = %id, "duplicate rule id rejected");
            rejected.push(RejectedRule { id, reason });
            continue;
        }
        match compile_rule_with_interner(rule, &mut interner) {
            Ok(compiled) => rules.push(compiled),
            Err(reason) => {
                warn!(target: "rcs_filter", rule_id = %id, %reason, "rule rejected during load");
                rejected.push(RejectedRule { id, reason });
            }
        }
    }

    let mut index: std::collections::HashMap<_, Vec<usize>> = std::collections::HashMap::new();
    let mut anonymous_index: std::collections::HashMap<_, Vec<usize>> =
        std::collections::HashMap::new();
    for (idx, rule) in rules.iter().enumerate() {
        for event in &rule.events {
            if event.anonymous {
                let indexed_topics = event.inputs.iter().filter(|input| input.indexed).count();
                anonymous_index.entry(indexed_topics).or_default().push(idx);
            } else {
                index.entry(event.topic0).or_default().push(idx);
            }
        }
    }

    RuleSet { protocol_version, content_version, rules, index, anonymous_index, rejected }
}

/// Validates and compiles a single raw rule with a fresh single-use interner. This is the
/// preserved public entry point; batch callers share address sets via
/// [`compile_rule_with_interner`].
pub fn compile_rule(raw: RawRule) -> std::result::Result<CompiledRule, String> {
    compile_rule_with_interner(raw, &mut AddressSetInterner::new())
}

/// Validates and compiles a single raw rule, interning any static address-membership set through
/// `interner`. Returns `Err(reason)` when the rule must be rejected, `Ok` otherwise.
/// `audit_timeout_action` defaults to `allow` for `audit` rules that omit it.
pub(crate) fn compile_rule_with_interner(
    raw: RawRule,
    interner: &mut AddressSetInterner,
) -> std::result::Result<CompiledRule, String> {
    // Reject an empty event_abis map because the rule could never match.
    if raw.event_abis.is_empty() {
        return Err("event_abis is empty".to_string());
    }
    if raw.event_abis.len() > super::MAX_EVENTS_PER_RULE {
        return Err(format!(
            "event_abis declares {} aliases; at most {} are allowed",
            raw.event_abis.len(),
            super::MAX_EVENTS_PER_RULE
        ));
    }
    if raw.action == Action::Audit && raw.audit_types.is_empty() {
        return Err("audit rule has no audit_types".to_string());
    }
    super::jsonlogic::validate(&raw.condition)?;

    // `audit_types` is irrelevant to filter-only deny/allow rules. In particular, its wire
    // default is `["quota"]`, which must not make non-audit events subject to quota shapes.
    let is_quota_audit = raw.action == Action::Audit && raw.audit_types.iter().any(|t| t == QUOTA);

    let mut events = Vec::with_capacity(raw.event_abis.len());
    for (var_name, abi) in &raw.event_abis {
        let compiled = compile_event(var_name, abi)?;
        // For quota audit rules, every declared event must match one of the fixed shapes.
        if is_quota_audit && !matches_quota_shape(&compiled) {
            return Err(format!(
                "event '{var_name}' does not match a fixed quota shape (ERC20 Transfer / ERC1155 TransferSingle / ERC1155 TransferBatch)"
            ));
        }
        events.push(compiled);
    }

    let contract_address = parse_opt_address(&raw.contract_address, "contract_address")?;
    let origin = parse_opt_address(&raw.origin, "origin")?;

    // contract_address is a review-only warning: it must be left
    // empty for events that can be produced via intermediate contracts (ERC20/1155
    // transfers), otherwise legitimate hits are silently skipped. Not an auto-reject.
    if contract_address.is_some() {
        warn!(
            target: "rcs_filter",
            rule_id = %raw.id,
            "rule declares contract_address; verify the event can only be triggered by a direct call"
        );
    }

    // An audit rule without audit_timeout_action defaults to allow.
    let audit_timeout_action = raw.audit_timeout_action.unwrap_or(TimeoutAction::Allow);

    // Compile the condition before `raw.condition` is moved into the struct literal below.
    let compiled_condition = compile(&raw.condition, interner);

    Ok(CompiledRule {
        id: raw.id,
        contract_address,
        origin,
        events,
        audit_types: raw.audit_types,
        condition: raw.condition,
        action: raw.action,
        audit_timeout_action,
        compiled_condition,
    })
}

/// Compiles one named event, resolving input types and precomputing `topic0`.
fn compile_event(var_name: &str, abi: &EventAbi) -> std::result::Result<CompiledEvent, String> {
    if abi.abi_type != "event" {
        return Err(format!("event '{var_name}' has unsupported ABI type '{}'", abi.abi_type));
    }
    let mut inputs = Vec::with_capacity(abi.inputs.len());
    let mut seen = std::collections::HashSet::new();
    let mut type_names = Vec::with_capacity(abi.inputs.len());
    let indexed_count = abi.inputs.iter().filter(|input| input.indexed).count();
    let max_indexed = if abi.anonymous { 4 } else { 3 };
    if indexed_count > max_indexed {
        return Err(format!(
            "event '{var_name}' declares {indexed_count} indexed inputs; maximum is {max_indexed}"
        ));
    }

    for input in &abi.inputs {
        // inputs[].name must be present and unique within the event.
        let name = input
            .name
            .clone()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| format!("event '{var_name}' has an input with a missing name"))?;
        if !seen.insert(name.clone()) {
            return Err(format!("event '{var_name}' has a duplicate input name '{name}'"));
        }
        let sol_type = DynSolType::parse(&input.param_type)
            .map_err(|e| format!("event '{var_name}' input '{name}' has invalid type: {e}"))?;
        type_names.push(sol_type.sol_type_name().to_string());
        inputs.push(CompiledInput { name, sol_type, indexed: input.indexed });
    }

    // topic0 = keccak256("EventName(canonicalType1,canonicalType2,...)").
    let signature = format!("{}({})", abi.name, type_names.join(","));
    let topic0 = keccak256(signature.as_bytes());

    Ok(CompiledEvent {
        var_name: var_name.to_string(),
        abi_name: abi.name.clone(),
        inputs,
        anonymous: abi.anonymous,
        topic0,
    })
}

/// Whether a compiled event's ordered input names match a fixed quota shape.
fn matches_quota_shape(event: &CompiledEvent) -> bool {
    let names: Vec<&str> = event.inputs.iter().map(|i| i.name.as_str()).collect();
    names == ERC20_TRANSFER_SHAPE
        || names == ERC1155_TRANSFER_SINGLE_SHAPE
        || names == ERC1155_TRANSFER_BATCH_SHAPE
}

/// Parses an optional `0x`-prefixed address; a present-but-malformed address rejects the rule.
fn parse_opt_address(
    value: &Option<String>,
    field: &str,
) -> std::result::Result<Option<Address>, String> {
    match value {
        None => Ok(None),
        Some(s) => Address::from_str(s)
            .map(Some)
            .map_err(|e| format!("{field} is not a valid address: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Action, CompiledCondition};
    use crate::test_support::golden;
    use serde_json::{json, Value};

    fn parse(json: &str) -> RawRule {
        serde_json::from_str(json).expect("valid rule json")
    }

    /// Minimal single-event `deny` rule carrying the given condition, for exercising
    /// condition compilation and cross-rule set interning.
    fn deny_rule_with_condition(id: &str, condition: Value) -> RawRule {
        serde_json::from_value(json!({
            "id": id,
            "event_abis": {
                "e": {
                    "type": "event",
                    "name": "E",
                    "inputs": [{"name": "a", "type": "address", "indexed": false}],
                    "anonymous": false
                }
            },
            "condition": condition,
            "action": "deny"
        }))
        .expect("valid rule")
    }

    #[test]
    fn load_rules_shares_arc_across_rules_and_standalone_compiles() {
        // two rules, same static address list (different order) → one shared Arc
        let a = "0x0101010101010101010101010101010101010101";
        let b = "0x0202020202020202020202020202020202020202";
        let raw = vec![
            deny_rule_with_condition("r1", json!({"in": [{"var":"origin"}, [a, b]]})),
            deny_rule_with_condition("r2", json!({"in": [{"var":"origin"}, [b, a]]})),
        ];
        let set = load_rules(1, 1, raw);
        assert_eq!(set.rules.len(), 2);
        let arc_of = |r: &CompiledRule| match &r.compiled_condition {
            CompiledCondition::AddressSetIn { set, .. } => set.clone(),
            other => panic!("expected AddressSetIn, got {other:?}"),
        };
        assert!(std::sync::Arc::ptr_eq(&arc_of(&set.rules[0]), &arc_of(&set.rules[1])));

        // standalone compile_rule (fresh interner) still succeeds
        let standalone =
            compile_rule(deny_rule_with_condition("r3", json!({"in": [{"var":"origin"}, [a]]})))
                .unwrap();
        assert!(matches!(standalone.compiled_condition, CompiledCondition::AddressSetIn { .. }));
    }

    #[test]
    fn scenario_a_rule_compiles_and_indexes() {
        let set = load_rules(1, 1, vec![parse(golden::RULE_SCENARIO_A)]);
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "risk-check-erc20-tokenx-bridge");
        assert_eq!(set.rules[0].action, Action::Audit);
        // Transfer(address,address,uint256) topic0 is indexed.
        assert_eq!(set.index.len(), 1);
    }

    #[test]
    fn missing_input_name_rejected() {
        let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"type":"address","indexed":true}],"anonymous":false}},"condition":true,"action":"deny"}"#;
        assert!(compile_rule(parse(json)).is_err());
    }

    #[test]
    fn duplicate_input_name_rejected() {
        let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":false},{"name":"a","type":"uint256","indexed":false}],"anonymous":false}},"condition":true,"action":"deny"}"#;
        assert!(compile_rule(parse(json)).is_err());
    }

    #[test]
    fn empty_event_abis_rejected() {
        let json = r#"{"id":"r","event_abis":{},"condition":true,"action":"deny"}"#;
        assert!(compile_rule(parse(json)).is_err());
    }

    #[test]
    fn event_alias_depth_above_eight_is_rejected() {
        let mut raw = parse(
            r#"{"id":"deep","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":true,"action":"deny"}"#,
        );
        let event = raw.event_abis.remove("e").unwrap();
        for index in 0..8 {
            raw.event_abis.insert(format!("e{index}"), event.clone());
        }
        assert!(compile_rule(raw.clone()).is_ok(), "eight aliases must remain valid");
        raw.event_abis.insert("e8".to_string(), event);

        let set = load_rules(1, 1, vec![raw]);
        assert!(set.rules.is_empty());
        assert_eq!(set.rejected.len(), 1);
        assert!(set.rejected[0].reason.contains("at most 8"));
    }

    #[test]
    fn audit_rule_with_empty_audit_types_is_rejected() {
        let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"audit_types":[],"condition":true,"action":"audit"}"#;
        assert!(compile_rule(parse(json)).is_err());
    }

    #[test]
    fn quota_wrong_shape_rejected() {
        // audit_types quota but shape is not ERC20/ERC1155.
        let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":false}],"anonymous":false}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
        assert!(compile_rule(parse(json)).is_err());
    }

    #[test]
    fn rendered_quota_rule_accepts_transfer_batch_shape() {
        let json = r#"{
          "id":"rendered-quota-rule",
          "event_abis":{
            "transfer":{"type":"event","name":"Transfer","inputs":[
              {"name":"from","type":"address","indexed":true},
              {"name":"to","type":"address","indexed":true},
              {"name":"value","type":"uint256","indexed":false}],"anonymous":false},
            "transferSingle":{"type":"event","name":"TransferSingle","inputs":[
              {"name":"operator","type":"address","indexed":true},
              {"name":"from","type":"address","indexed":true},
              {"name":"to","type":"address","indexed":true},
              {"name":"id","type":"uint256","indexed":false},
              {"name":"value","type":"uint256","indexed":false}],"anonymous":false},
            "transferBatch":{"type":"event","name":"TransferBatch","inputs":[
              {"name":"operator","type":"address","indexed":true},
              {"name":"from","type":"address","indexed":true},
              {"name":"to","type":"address","indexed":true},
              {"name":"ids","type":"uint256[]","indexed":false},
              {"name":"values","type":"uint256[]","indexed":false}],"anonymous":false}
          },
          "audit_types":["quota"],
          "condition":true,
          "action":"audit",
          "audit_timeout_action":"allow"
        }"#;

        let compiled = compile_rule(parse(json)).expect("RCS-rendered quota rule must compile");
        assert_eq!(compiled.events.len(), 3);
    }

    #[test]
    fn non_audit_rules_do_not_require_quota_event_shapes() {
        let deny = r#"{"id":"deny-owner-change","event_abis":{"owner_change":{"type":"event","name":"OwnershipTransferred","inputs":[{"name":"previousOwner","type":"address","indexed":true},{"name":"newOwner","type":"address","indexed":true}],"anonymous":false}},"condition":true,"action":"deny"}"#;
        let allow = r#"{"id":"allow-owner-change","event_abis":{"owner_change":{"type":"event","name":"OwnershipTransferred","inputs":[{"name":"previousOwner","type":"address","indexed":true},{"name":"newOwner","type":"address","indexed":true}],"anonymous":false}},"condition":true,"action":"allow"}"#;

        for (json, expected_action) in [(deny, Action::Deny), (allow, Action::Allow)] {
            let rule = compile_rule(parse(json)).expect("non-audit rule is not quota-shaped");
            assert_eq!(rule.action, expected_action);
            assert_eq!(rule.audit_types, [QUOTA]);
        }
    }

    #[test]
    fn audit_without_timeout_action_defaults_allow() {
        let json = r#"{"id":"r","event_abis":{"transfer":{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":false}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
        let r = compile_rule(parse(json)).expect("valid");
        assert_eq!(r.audit_timeout_action, TimeoutAction::Allow);
    }

    #[test]
    fn invalid_rules_do_not_drop_valid_ones() {
        let bad = r#"{"id":"bad","event_abis":{},"condition":true,"action":"deny"}"#;
        let set = load_rules(1, 5, vec![parse(bad), parse(golden::RULE_SCENARIO_A)]);
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.content_version, 5);
    }

    #[test]
    fn missing_required_rule_and_event_fields_fail_decoding() {
        let missing_event_abis = r#"{"id":"r","condition":true,"action":"deny"}"#;
        assert!(serde_json::from_str::<RawRule>(missing_event_abis).is_err());

        for event in [
            r#"{"name":"E","inputs":[],"anonymous":false}"#,
            r#"{"type":"event","inputs":[],"anonymous":false}"#,
            r#"{"type":"event","name":"E","anonymous":false}"#,
            r#"{"type":"event","name":"E","inputs":[]}"#,
        ] {
            let json = format!(
                r#"{{"id":"r","event_abis":{{"e":{event}}},"condition":true,"action":"deny"}}"#
            );
            assert!(serde_json::from_str::<RawRule>(&json).is_err(), "accepted {event}");
        }

        let missing_indexed = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address"}],"anonymous":false}},"condition":true,"action":"deny"}"#;
        assert!(serde_json::from_str::<RawRule>(missing_indexed).is_err());
    }

    #[test]
    fn event_topic_limits_are_enforced() {
        let non_anonymous = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":true},{"name":"b","type":"address","indexed":true},{"name":"c","type":"address","indexed":true},{"name":"d","type":"address","indexed":true}],"anonymous":false}},"condition":true,"action":"deny"}"#;
        assert!(compile_rule(parse(non_anonymous)).is_err());

        let anonymous = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":true},{"name":"b","type":"address","indexed":true},{"name":"c","type":"address","indexed":true},{"name":"d","type":"address","indexed":true}],"anonymous":true}},"condition":true,"action":"deny"}"#;
        assert!(compile_rule(parse(anonymous)).is_ok());
    }

    #[test]
    fn duplicate_rule_ids_are_all_rejected_without_dropping_unique_rules() {
        let duplicate = r#"{"id":"duplicate","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":true,"action":"deny"}"#;
        let unique = r#"{"id":"unique","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":true,"action":"deny"}"#;
        let set = load_rules(1, 1, vec![parse(duplicate), parse(duplicate), parse(unique)]);
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "unique");
    }

    #[test]
    fn only_exact_event_abi_type_is_accepted() {
        let rule = |abi_type: &str| {
            format!(
                r#"{{"id":"r","event_abis":{{"e":{{"type":"{abi_type}","name":"E","inputs":[],"anonymous":false}}}},"condition":true,"action":"deny"}}"#
            )
        };
        assert!(compile_rule(parse(&rule("event"))).is_ok());
        assert!(compile_rule(parse(&rule("function"))).is_err());
        assert!(compile_rule(parse(&rule("Event"))).is_err());
    }

    #[test]
    fn invalid_jsonlogic_rejects_only_affected_rules() {
        let invalid_operator = r#"{"id":"bad-op","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":{"cat":[1,2]},"action":"deny"}"#;
        let invalid_arity = r#"{"id":"bad-arity","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":{"==":[1]},"action":"deny"}"#;
        let valid = r#"{"id":"valid","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":{"==":[1,1]},"action":"deny"}"#;
        let set =
            load_rules(1, 1, vec![parse(invalid_operator), parse(valid), parse(invalid_arity)]);
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "valid");
    }
}
