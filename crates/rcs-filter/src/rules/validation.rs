//! Rule loading + per-rule validation (FR-8) and topic0 index construction.
//!
//! Validation is **per-rule**: an invalid rule is rejected whole (not field-ignored, not
//! whole-package-failed) and logged at `warn` with its id and reason; all other rules stay
//! in effect (contract §3.1, TD §4.5).

use std::str::FromStr;

use alloy_dyn_abi::DynSolType;
use alloy_primitives::{keccak256, Address};
use tracing::warn;

use super::model::{
    Action, CompiledEvent, CompiledInput, CompiledRule, EventAbi, RawRule, RuleSet, TimeoutAction,
};

/// Fixed ERC20 `Transfer` quota shape (ordered input names) — contract §3.3.
const ERC20_TRANSFER_SHAPE: &[&str] = &["from", "to", "value"];
/// Fixed ERC1155 `TransferSingle` quota shape (ordered input names) — contract §3.3.
const ERC1155_TRANSFER_SINGLE_SHAPE: &[&str] = &["operator", "from", "to", "id", "value"];
/// The only audit type implemented today (contract §2.4).
const QUOTA: &str = "quota";

/// Loads and validates a batch of raw rules into an immutable [`RuleSet`] snapshot with a
/// topic0 index. Rejected rules are dropped with a `warn`; the returned set contains only
/// rules that passed validation.
pub fn load_rules(protocol_version: u32, content_version: u64, raw: Vec<RawRule>) -> RuleSet {
    let mut rules = Vec::new();
    for rule in raw {
        let id = rule.id.clone();
        match compile_rule(rule) {
            Ok(compiled) => rules.push(compiled),
            Err(reason) => {
                warn!(target: "rcs_filter", rule_id = %id, %reason, "rule rejected during load")
            }
        }
    }

    let mut index: std::collections::HashMap<_, Vec<usize>> = std::collections::HashMap::new();
    for (idx, rule) in rules.iter().enumerate() {
        for event in &rule.events {
            index.entry(event.topic0).or_default().push(idx);
        }
    }

    RuleSet { protocol_version, content_version, rules, index }
}

/// Validates and compiles a single raw rule. Returns `Err(reason)` when the rule must be
/// rejected (FR-8 conditions), `Ok` otherwise. `audit_timeout_action` is defaulted to
/// `allow` for `audit` rules that omit it (contract §3.6 disambiguation).
pub fn compile_rule(raw: RawRule) -> std::result::Result<CompiledRule, String> {
    // (3) event_abis empty → reject (dead rule).
    if raw.event_abis.is_empty() {
        return Err("event_abis is empty".to_string());
    }

    // `audit_types` is irrelevant to filter-only deny/allow rules. In particular, its wire
    // default is `["quota"]`, which must not make non-audit events subject to quota shapes.
    let is_quota_audit = raw.action == Action::Audit && raw.audit_types.iter().any(|t| t == QUOTA);

    let mut events = Vec::with_capacity(raw.event_abis.len());
    for (var_name, abi) in &raw.event_abis {
        let compiled = compile_event(var_name, abi)?;
        // (2) quota fixed-shape: for audit rules whose audit_types includes "quota", every
        // declared event must match one of the fixed shapes (contract §3.3).
        if is_quota_audit && !matches_quota_shape(&compiled) {
            return Err(format!(
                "event '{var_name}' does not match a fixed quota shape (ERC20 Transfer / ERC1155 TransferSingle)"
            ));
        }
        events.push(compiled);
    }

    let contract_address = parse_opt_address(&raw.contract_address, "contract_address")?;
    let origin = parse_opt_address(&raw.origin, "origin")?;

    // contract_address is a review-only warning (TD §4.6 stage-one note): it must be left
    // empty for events that can be produced via intermediate contracts (ERC20/1155
    // transfers), otherwise legitimate hits are silently skipped. Not an auto-reject.
    if contract_address.is_some() {
        warn!(
            target: "rcs_filter",
            rule_id = %raw.id,
            "rule declares contract_address; verify the event can only be triggered by a direct call (contract §3.1)"
        );
    }

    // (4) audit rule without audit_timeout_action → default allow.
    let audit_timeout_action = raw.audit_timeout_action.unwrap_or(TimeoutAction::Allow);

    Ok(CompiledRule {
        id: raw.id,
        contract_address,
        origin,
        events,
        audit_types: raw.audit_types,
        condition: raw.condition,
        action: raw.action,
        audit_timeout_action,
    })
}

/// Compiles one named event, resolving input types and precomputing `topic0`.
fn compile_event(var_name: &str, abi: &EventAbi) -> std::result::Result<CompiledEvent, String> {
    let mut inputs = Vec::with_capacity(abi.inputs.len());
    let mut seen = std::collections::HashSet::new();
    let mut type_names = Vec::with_capacity(abi.inputs.len());

    for input in &abi.inputs {
        // (1) inputs[].name must be present and unique within the event → else reject.
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

/// Whether a compiled event's ordered input names match a fixed quota shape (contract §3.3).
fn matches_quota_shape(event: &CompiledEvent) -> bool {
    let names: Vec<&str> = event.inputs.iter().map(|i| i.name.as_str()).collect();
    names == ERC20_TRANSFER_SHAPE || names == ERC1155_TRANSFER_SINGLE_SHAPE
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
    use crate::rules::Action;
    use crate::test_support::golden;

    fn parse(json: &str) -> RawRule {
        serde_json::from_str(json).expect("valid rule json")
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
        let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address"},{"name":"a","type":"uint256"}],"anonymous":false}},"condition":true,"action":"deny"}"#;
        assert!(compile_rule(parse(json)).is_err());
    }

    #[test]
    fn empty_event_abis_rejected() {
        let json = r#"{"id":"r","event_abis":{},"condition":true,"action":"deny"}"#;
        assert!(compile_rule(parse(json)).is_err());
    }

    #[test]
    fn quota_wrong_shape_rejected() {
        // audit_types quota but shape is not ERC20/ERC1155.
        let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address"}],"anonymous":false}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
        assert!(compile_rule(parse(json)).is_err());
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
        let json = r#"{"id":"r","event_abis":{"transfer":{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256"}],"anonymous":false}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
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
}
