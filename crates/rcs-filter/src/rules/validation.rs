//! Rule loading, per-rule validation, and topic0 index construction.
//!
//! Validation is **per-rule**: an invalid rule is rejected whole (not field-ignored, not
//! whole-package-failed) and logged at `warn` with its id and reason; all other rules stay
//! in effect.

use std::str::FromStr;

use alloy_dyn_abi::DynSolType;
use alloy_primitives::{keccak256, Address, B256};
use tracing::warn;

use super::jsonlogic::{compile, AddressSetInterner};
use super::model::{
    Action, CompiledCallstackSelector, CompiledEvent, CompiledInput, CompiledRule, EventAbi,
    RawCallstackSelector, RawRule, RejectedRule, RuleSet, TimeoutAction, UnknownFields,
};

/// Fixed ERC20 `Transfer` quota shape (ordered input names).
const ERC20_TRANSFER_SHAPE: &[&str] = &["from", "to", "value"];
/// Fixed ERC1155 `TransferSingle` quota shape (ordered input names).
const ERC1155_TRANSFER_SINGLE_SHAPE: &[&str] = &["operator", "from", "to", "id", "value"];
/// ERC1155 `TransferBatch` quota shape advertised by the RCS rule ABI.
const ERC1155_TRANSFER_BATCH_SHAPE: &[&str] = &["operator", "from", "to", "ids", "values"];
/// The only audit type implemented today.
const QUOTA: &str = "quota";

/// At most this many offending keys are echoed in a rejection reason. A hostile or broken
/// snapshot must not be able to inflate an operator log line.
const MAX_UNKNOWN_KEYS_REPORTED: usize = 8;
/// At most this many bytes of each echoed key are reproduced, for the same reason.
const MAX_UNKNOWN_KEY_BYTES: usize = 64;
/// Location label used when the offending keys sit at the `GET /rules` response root.
pub(crate) const RULES_RESPONSE_LOCATION: &str = "rules response";

/// Rejects a level that carries any wire key it does not declare, naming the keys.
///
/// The reason is bounded on both axes ([`MAX_UNKNOWN_KEYS_REPORTED`],
/// [`MAX_UNKNOWN_KEY_BYTES`]) and keys are reported in sorted order, which a `BTreeMap`
/// already guarantees, so the reason is stable for a given input.
pub fn reject_unknown(location: &str, unknown: &UnknownFields) -> std::result::Result<(), String> {
    if unknown.is_empty() {
        return Ok(());
    }
    let echoed: Vec<String> =
        unknown.keys().take(MAX_UNKNOWN_KEYS_REPORTED).map(|key| bound_key(key)).collect();
    let mut reason = format!("unknown field(s) in {location}: {}", echoed.join(", "));
    if let Some(suppressed) = unknown.len().checked_sub(MAX_UNKNOWN_KEYS_REPORTED)
        && suppressed > 0
    {
        reason.push_str(&format!(" (+{suppressed} more)"));
    }
    Err(reason)
}

/// Truncates one echoed key to [`MAX_UNKNOWN_KEY_BYTES`] bytes on a char boundary, marking
/// that it was cut so a reader cannot mistake the prefix for the whole key.
fn bound_key(key: &str) -> String {
    if key.len() <= MAX_UNKNOWN_KEY_BYTES {
        return key.to_string();
    }
    const MARKER: &str = "...";
    let mut end = MAX_UNKNOWN_KEY_BYTES - MARKER.len();
    while end > 0 && !key.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARKER}", &key[..end])
}

/// One dedicated traversal rejecting any unknown wire key reachable from `raw`, in a fixed
/// order: rule, then callstack selectors by name, then events by name, then each event's inputs
/// by index. Both maps are `BTreeMap`s, so "by name" is already sorted.
///
/// Running this as a single pass before every other per-rule check is what makes the reported
/// reason independent of which unrelated validation would otherwise have failed first.
fn reject_unknown_rule_fields(raw: &RawRule) -> std::result::Result<(), String> {
    reject_unknown("rule", &raw.unknown)?;
    for (var_name, selector) in &raw.callstack {
        reject_unknown(&format!("callstack selector '{var_name}'"), &selector.unknown)?;
    }
    for (var_name, abi) in &raw.event_abis {
        reject_unknown(&format!("event '{var_name}'"), &abi.unknown)?;
        for (index, input) in abi.inputs.iter().enumerate() {
            reject_unknown(&format!("event '{var_name}' input #{index}"), &input.unknown)?;
        }
    }
    Ok(())
}

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
        match compile_rule_with_interner(rule, protocol_version, &mut interner) {
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

    // The capture selector set: every address any active rule observes, sorted and deduplicated
    // so the snapshot stamp's digest depends only on set membership.
    let mut callstack_selector_addresses: Vec<Address> =
        rules.iter().flat_map(|rule| rule.callstack.iter().map(|s| s.address)).collect();
    callstack_selector_addresses.sort_unstable();
    callstack_selector_addresses.dedup();

    RuleSet {
        protocol_version,
        content_version,
        rules,
        index,
        anonymous_index,
        rejected,
        callstack_selector_addresses,
    }
}

/// Validates and compiles a single raw rule with a fresh single-use interner. This is the
/// preserved public entry point; batch callers share address sets via
/// [`compile_rule_with_interner`].
pub fn compile_rule(raw: RawRule) -> std::result::Result<CompiledRule, String> {
    compile_rule_with_interner(
        raw,
        crate::config::MIN_PROTOCOL_VERSION,
        &mut AddressSetInterner::new(),
    )
}

/// Validates and compiles a single raw rule against an explicit rule `protocol_version`. Needed
/// because the callstack section is only accepted from the protocol version that introduced it.
pub fn compile_rule_at_protocol(
    raw: RawRule,
    protocol_version: u32,
) -> std::result::Result<CompiledRule, String> {
    compile_rule_with_interner(raw, protocol_version, &mut AddressSetInterner::new())
}

/// Validates and compiles a single raw rule, interning any static address-membership set through
/// `interner`. Returns `Err(reason)` when the rule must be rejected, `Ok` otherwise.
/// `audit_timeout_action` defaults to `allow` for `audit` rules that omit it.
pub(crate) fn compile_rule_with_interner(
    raw: RawRule,
    protocol_version: u32,
    interner: &mut AddressSetInterner,
) -> std::result::Result<CompiledRule, String> {
    // First, and before every other per-rule check: an unknown wire key anywhere under this
    // rule means an author-visible constraint may be silently absent, so the reason that names
    // the offending key must win over any other rejection the same input would also trigger.
    reject_unknown_rule_fields(&raw)?;

    // A rule needs at least one selector family: with neither declared events nor callstack
    // selectors it could never match. `event_abis` alone being empty is legitimate for a rule
    // that selects purely on call frames.
    if raw.event_abis.is_empty() && raw.callstack.is_empty() {
        return Err("rule declares neither event_abis nor callstack selectors".to_string());
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

    let callstack = compile_callstack(&raw, protocol_version)?;

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
        callstack,
        audit_types: raw.audit_types,
        condition: raw.condition,
        action: raw.action,
        audit_timeout_action,
        compiled_condition,
    })
}

/// Validates and compiles a rule's `callstack` section.
///
/// Rejections (any one of which rejects the rule, and therefore the whole snapshot update):
/// a selector without an address; exactly one of `implAddress` / `implCodeHash`; a malformed
/// address or code-hash encoding; a callstack-only `audit` rule, whose action payload would have
/// no content to carry; the section appearing under a protocol version that does not define it;
/// more selectors than the per-rule bound; and a selector name colliding with an event alias,
/// which would otherwise silently shadow one of them in the flat condition-variable keyspace.
fn compile_callstack(
    raw: &RawRule,
    protocol_version: u32,
) -> std::result::Result<Vec<CompiledCallstackSelector>, String> {
    if raw.callstack.is_empty() {
        return Ok(Vec::new());
    }
    if !crate::config::protocol_supports_callstack(protocol_version) {
        return Err(format!(
            "callstack selectors require rule protocol_version {} or later; this set declares {protocol_version}",
            crate::config::CALLSTACK_MIN_PROTOCOL_VERSION
        ));
    }
    if raw.action == Action::Audit && raw.event_abis.is_empty() {
        return Err("an audit rule selecting only on callstack declares no event payload content"
            .to_string());
    }
    if raw.callstack.len() > super::MAX_CALLSTACK_SELECTORS_PER_RULE {
        return Err(format!(
            "callstack declares {} selectors; at most {} are allowed",
            raw.callstack.len(),
            super::MAX_CALLSTACK_SELECTORS_PER_RULE
        ));
    }

    let mut out = Vec::with_capacity(raw.callstack.len());
    for (var_name, selector) in &raw.callstack {
        if raw.event_abis.contains_key(var_name) {
            return Err(format!(
                "callstack selector '{var_name}' collides with an event_abis alias; condition variables share one flat namespace"
            ));
        }
        out.push(compile_callstack_selector(var_name, selector)?);
    }
    Ok(out)
}

/// Compiles one named selector, enforcing the encoding rules and the impl field pairing.
fn compile_callstack_selector(
    var_name: &str,
    selector: &RawCallstackSelector,
) -> std::result::Result<CompiledCallstackSelector, String> {
    let address = match selector.address.as_deref() {
        Some(value) => parse_lower_hex_address(value)
            .map_err(|e| format!("callstack selector '{var_name}' address {e}"))?,
        None => {
            return Err(format!("callstack selector '{var_name}' has no address"));
        }
    };
    if selector.impl_address.is_some() != selector.impl_code_hash.is_some() {
        return Err(format!(
            "callstack selector '{var_name}' must supply implAddress and implCodeHash together or not at all"
        ));
    }
    let code_hash = match selector.code_hash.as_deref() {
        Some(value) => Some(
            parse_lower_hex_b256(value)
                .map_err(|e| format!("callstack selector '{var_name}' codeHash {e}"))?,
        ),
        None => None,
    };
    let impl_address = match selector.impl_address.as_deref() {
        Some(value) => Some(
            parse_lower_hex_address(value)
                .map_err(|e| format!("callstack selector '{var_name}' implAddress {e}"))?,
        ),
        None => None,
    };
    let impl_code_hash = match selector.impl_code_hash.as_deref() {
        Some(value) => Some(
            parse_lower_hex_b256(value)
                .map_err(|e| format!("callstack selector '{var_name}' implCodeHash {e}"))?,
        ),
        None => None,
    };
    Ok(CompiledCallstackSelector {
        var_name: var_name.to_string(),
        address,
        code_hash,
        impl_address,
        impl_code_hash,
    })
}

/// Rejects anything that is not a lowercase `0x`-prefixed 20-byte address.
///
/// Case is enforced rather than normalized: observed values are emitted lowercase, so accepting a
/// mixed-case selector would make an author believe a constraint is active while the exact-match
/// comparison it performs is against a different string form.
fn parse_lower_hex_address(value: &str) -> std::result::Result<Address, String> {
    check_lower_hex(value, Address::len_bytes())?;
    Address::from_str(value).map_err(|e| format!("is not a valid address: {e}"))
}

/// Rejects anything that is not a lowercase `0x`-prefixed 32-byte hash.
fn parse_lower_hex_b256(value: &str) -> std::result::Result<B256, String> {
    check_lower_hex(value, B256::len_bytes())?;
    B256::from_str(value).map_err(|e| format!("is not a valid 32-byte hash: {e}"))
}

/// Shared encoding gate: `0x` prefix, exact byte length, lowercase hex digits only.
fn check_lower_hex(value: &str, len_bytes: usize) -> std::result::Result<(), String> {
    let Some(body) = value.strip_prefix("0x") else {
        return Err("must be 0x-prefixed".to_string());
    };
    if body.len() != len_bytes * 2 {
        return Err(format!(
            "must be exactly {len_bytes} bytes ({} hex digits), got {}",
            len_bytes * 2,
            body.len()
        ));
    }
    if body.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("must be lowercase hex".to_string());
    }
    if !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("must be hex digits only".to_string());
    }
    Ok(())
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
    fn a_rule_with_no_selector_of_either_kind_is_rejected() {
        // An empty `event_abis` is no longer rejected on its own — a rule may select purely on
        // call frames. What must stay rejected is a rule with NEITHER selector family, which
        // could never match anything.
        let json = r#"{"id":"r","event_abis":{},"condition":true,"action":"deny"}"#;
        let reason = compile_rule(parse(json)).expect_err("a rule that can never match");
        assert!(reason.contains("neither event_abis nor callstack"), "{reason}");

        // The same rule with a callstack selector is valid at the version that defines it.
        let with_callstack = r#"{"id":"r","event_abis":{},"callstack":{"entry":{"address":"0x1111111111111111111111111111111111111111"}},"condition":true,"action":"deny"}"#;
        assert!(super::compile_rule_at_protocol(
            parse(with_callstack),
            crate::config::CALLSTACK_MIN_PROTOCOL_VERSION
        )
        .is_ok());
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
        // `event_abis` is optional on the wire now that a rule may select purely on call frames,
        // so an omitted map decodes to an empty one. The rule is still refused — one layer later,
        // at validation, with a reason naming both selector families.
        let missing_event_abis = r#"{"id":"r","condition":true,"action":"deny"}"#;
        let decoded = serde_json::from_str::<RawRule>(missing_event_abis)
            .expect("an omitted event_abis decodes to an empty map");
        assert!(decoded.event_abis.is_empty());
        assert!(decoded.callstack.is_empty());
        let reason = compile_rule(decoded).expect_err("a rule that can never match");
        assert!(reason.contains("neither event_abis nor callstack"), "{reason}");

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

/// Strict wire-schema tests: an unrecognized key anywhere in the rule wire format must reject
/// the rule, and therefore the whole snapshot update, with a reason that names the key.
///
/// Before this behaviour existed serde discarded the key, every selector constraint field being
/// `Option<String>` deserialized to `None`, and the rule installed with the protection silently
/// removed.
#[cfg(test)]
mod strict_schema_tests {
    use super::*;
    use crate::test_support::golden;
    use serde_json::{json, Value};

    const PROXY: &str = "0x00000000000000000000000000000000000000aa";
    const IMPL: &str = "0x00000000000000000000000000000000000000bb";
    const CODE_HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

    fn transfer_event() -> Value {
        json!({
            "type": "event", "name": "Transfer",
            "inputs": [
                { "name": "from", "type": "address", "indexed": true },
                { "name": "to", "type": "address", "indexed": true },
                { "name": "value", "type": "uint256", "indexed": false }
            ],
            "anonymous": false
        })
    }

    /// Loads one raw rule at the callstack-capable protocol version and returns the rejection
    /// reason, asserting that exactly one rule was rejected and none installed.
    fn rejection_reason(rule: Value) -> String {
        let raw: RawRule = serde_json::from_value(rule).expect("rule must still deserialize");
        let set = load_rules(crate::config::CALLSTACK_MIN_PROTOCOL_VERSION, 1, vec![raw]);
        assert!(set.rules.is_empty(), "no rule may install: {:?}", set.rules.len());
        assert_eq!(set.rejected.len(), 1, "exactly one rejection expected");
        set.rejected[0].reason.clone()
    }

    fn accepts(rule: Value, protocol_version: u32) -> RuleSet {
        let raw: RawRule = serde_json::from_value(rule).expect("rule must deserialize");
        let set = load_rules(protocol_version, 1, vec![raw]);
        assert!(set.rejected.is_empty(), "unexpected rejection: {:?}", set.rejected);
        set
    }

    /// a top-level `callStack` typo on a rule that carries a valid `event_abis` (so it
    /// survives the "declares neither" boundary) rejects the whole snapshot, naming `callStack`.
    #[test]
    fn top_level_callstack_typo_is_rejected_by_name() {
        let reason = rejection_reason(json!({
            "id": "typo-callstack",
            "event_abis": { "transfer": transfer_event() },
            "callStack": { "entry": { "address": PROXY } },
            "condition": true,
            "action": "deny"
        }));
        assert_eq!(reason, "unknown field(s) in rule: callStack");
    }

    /// a `codehash` typo inside a selector rejects the snapshot, naming `codehash`.
    /// Previously the selector installed with its code-hash constraint gone, matching any
    /// executed code.
    #[test]
    fn selector_codehash_typo_is_rejected_by_name() {
        let reason = rejection_reason(json!({
            "id": "typo-selector-codehash",
            "callstack": { "entry": { "address": PROXY, "codehash": CODE_HASH } },
            "condition": { "==": [ { "var": "entry.matched" }, true ] },
            "action": "deny"
        }));
        assert_eq!(reason, "unknown field(s) in callstack selector 'entry': codehash");
    }

    /// both `implAddress` and `implCodeHash` misspelled. Previously the pairing check
    /// (the implAddress/implCodeHash pairing check) saw `None != None` and passed, so the
    /// implementation allowlist was simply gone.
    #[test]
    fn both_impl_fields_misspelled_are_rejected() {
        let reason = rejection_reason(json!({
            "id": "typo-both-impl",
            "callstack": { "entry": {
                "address": PROXY,
                "impladdress": IMPL,
                "implcodehash": CODE_HASH
            } },
            "condition": { "==": [ { "var": "entry.matched" }, true ] },
            "action": "deny"
        }));
        assert_eq!(
            reason,
            "unknown field(s) in callstack selector 'entry': impladdress, implcodehash"
        );
    }

    /// any unrecognised safety key at rule level is rejected with the key named.
    #[test]
    fn unrecognised_rule_level_key_is_rejected() {
        let reason = rejection_reason(json!({
            "id": "speculative-field",
            "event_abis": { "transfer": transfer_event() },
            "require_attestation": true,
            "condition": true,
            "action": "deny"
        }));
        assert_eq!(reason, "unknown field(s) in rule: require_attestation");
    }

    /// an unknown key inside `event_abis.<name>` is rejected.
    #[test]
    fn unknown_key_inside_event_is_rejected() {
        let mut event = transfer_event();
        event["Anonymous"] = json!(true);
        let reason = rejection_reason(json!({
            "id": "typo-event",
            "event_abis": { "transfer": event },
            "condition": true,
            "action": "deny"
        }));
        assert_eq!(reason, "unknown field(s) in event 'transfer': Anonymous");
    }

    /// an unknown key inside an ABI input is rejected, naming the input index.
    #[test]
    fn unknown_key_inside_abi_input_is_rejected() {
        let mut event = transfer_event();
        event["inputs"][1]["Indexed"] = json!(false);
        let reason = rejection_reason(json!({
            "id": "typo-abi-input",
            "event_abis": { "transfer": event },
            "condition": true,
            "action": "deny"
        }));
        assert_eq!(reason, "unknown field(s) in event 'transfer' input #1: Indexed");
    }

    /// boundary preserved: a `callStack` typo on a rule declaring **no** `event_abis` is
    /// still rejected. The "declares neither event_abis nor callstack selectors" boundary already
    /// caught this input; the unknown-field
    /// traversal runs first, so the **reason** now names `callStack` instead. The rejection does
    /// not change; only the diagnostic improves.
    #[test]
    fn callstack_typo_without_events_is_rejected_with_unknown_field_reason() {
        let reason = rejection_reason(json!({
            "id": "typo-callstack-no-events",
            "callStack": { "entry": { "address": PROXY } },
            "condition": true,
            "action": "deny"
        }));
        assert_eq!(reason, "unknown field(s) in rule: callStack");
    }

    /// boundary preserved: a correct `implAddress` with only `implCodehash` misspelled is
    /// still rejected (the pairing check already caught it), now with the unknown-field reason.
    #[test]
    fn misspelled_impl_code_hash_is_rejected_with_unknown_field_reason() {
        let reason = rejection_reason(json!({
            "id": "typo-impl-code-hash-only",
            "callstack": { "entry": {
                "address": PROXY,
                "implAddress": IMPL,
                "implCodehash": CODE_HASH
            } },
            "condition": { "==": [ { "var": "entry.matched" }, true ] },
            "action": "deny"
        }));
        assert_eq!(reason, "unknown field(s) in callstack selector 'entry': implCodehash");
    }

    /// positive control: a fully correct v2 `callstack` rule, using every selector field,
    /// loads unchanged.
    #[test]
    fn correct_v2_callstack_rule_loads() {
        let set = accepts(
            json!({
                "id": "correct-v2",
                "callstack": { "entry": {
                    "address": PROXY,
                    "codeHash": CODE_HASH,
                    "implAddress": IMPL,
                    "implCodeHash": CODE_HASH
                } },
                "condition": { "==": [ { "var": "entry.matched" }, true ] },
                "action": "deny"
            }),
            crate::config::CALLSTACK_MIN_PROTOCOL_VERSION,
        );
        assert_eq!(set.rules.len(), 1);
        let selector = &set.rules[0].callstack[0];
        assert_eq!(selector.var_name, "entry");
        assert!(selector.code_hash.is_some());
        assert!(selector.impl_address.is_some());
        assert!(selector.impl_code_hash.is_some());
    }

    /// positive control: a correct v1 rule with no `callstack` loads unchanged.
    #[test]
    fn correct_v1_event_rule_loads() {
        let set = accepts(
            json!({
                "id": "correct-v1",
                "contract_address": null,
                "origin": null,
                "event_abis": { "transfer": transfer_event() },
                "condition": true,
                "action": "deny"
            }),
            crate::config::MIN_PROTOCOL_VERSION,
        );
        assert_eq!(set.rules.len(), 1);
        assert!(set.rules[0].callstack.is_empty());
    }

    /// positive control: the existing golden rule fixtures and the official RCS snapshot
    /// still load with `rejected` empty.
    #[test]
    fn golden_fixtures_still_load_clean() {
        let raw: Vec<RawRule> = [golden::RULE_SCENARIO_A, golden::RULE_SCENARIO_B]
            .iter()
            .map(|text| serde_json::from_str(text).expect("golden fixture must deserialize"))
            .collect();
        let set = load_rules(1, 1, raw);
        assert!(set.rejected.is_empty(), "golden fixtures rejected: {:?}", set.rejected);
        assert_eq!(set.rules.len(), 2);

        let snapshot: Value =
            serde_json::from_str(include_str!("../../testdata/rules-snapshot.json"))
                .expect("official snapshot must deserialize");
        let raw: Vec<RawRule> = snapshot["rules"]
            .as_array()
            .expect("rules array")
            .iter()
            .map(|rule| serde_json::from_value(rule.clone()).expect("snapshot rule"))
            .collect();
        let set = load_rules(
            snapshot["protocol_version"].as_u64().unwrap() as u32,
            snapshot["content_version"].as_u64().unwrap(),
            raw,
        );
        assert!(set.rejected.is_empty(), "official snapshot rejected: {:?}", set.rejected);
    }

    /// the key echo is bounded on both axes: at most 8 keys are named, each at most 64
    /// bytes, and the remainder is summarised as `(+N more)`. A hostile snapshot must not be
    /// able to inflate an operator log line.
    #[test]
    fn key_echo_is_bounded() {
        let mut rule = json!({
            "id": "many-unknown-keys",
            "event_abis": { "transfer": transfer_event() },
            "condition": true,
            "action": "deny"
        });
        // Sorted order is `k00..k49`, so the first 8 reported keys are deterministic. Each name
        // is far longer than the per-key byte bound.
        for index in 0..50 {
            let key = format!("k{index:02}{}", "x".repeat(200));
            rule[key] = json!(1);
        }
        let reason = rejection_reason(rule);

        assert!(reason.starts_with("unknown field(s) in rule: "), "{reason}");
        assert!(reason.ends_with(" (+42 more)"), "{reason}");
        let listed =
            reason.trim_start_matches("unknown field(s) in rule: ").trim_end_matches(" (+42 more)");
        let keys: Vec<&str> = listed.split(", ").collect();
        assert_eq!(keys.len(), MAX_UNKNOWN_KEYS_REPORTED);
        for key in &keys {
            assert!(key.len() <= MAX_UNKNOWN_KEY_BYTES, "key too long: {} bytes", key.len());
            assert!(key.ends_with("..."), "a truncated key must be marked: {key}");
        }
        assert!(keys[0].starts_with("k00"), "keys are reported in sorted order: {keys:?}");
        assert!(keys[7].starts_with("k07"), "keys are reported in sorted order: {keys:?}");
    }

    /// control: a rule genuinely declaring neither `event_abis` nor `callstack`, with no
    /// unknown keys, is still rejected with the **original** "declares neither" reason.
    #[test]
    fn declares_neither_keeps_original_reason() {
        let reason = rejection_reason(json!({
            "id": "dead-rule",
            "event_abis": {},
            "condition": true,
            "action": "deny"
        }));
        assert_eq!(reason, "rule declares neither event_abis nor callstack selectors");
    }

    /// control: a selector with a correct `implAddress` and `implCodeHash` genuinely
    /// absent, with no unknown keys, is still rejected with the pairing check's **original**
    /// reason.
    #[test]
    fn impl_pairing_keeps_original_reason() {
        let reason = rejection_reason(json!({
            "id": "unpaired-impl",
            "callstack": { "entry": { "address": PROXY, "implAddress": IMPL } },
            "condition": { "==": [ { "var": "entry.matched" }, true ] },
            "action": "deny"
        }));
        assert_eq!(
            reason,
            "callstack selector 'entry' must supply implAddress and implCodeHash together or not at all"
        );
    }

    /// The traversal order is fixed (rule → selectors → events → inputs), so an input carrying
    /// unknown keys at several levels always reports the outermost one. Without that the reported
    /// reason would depend on which unrelated check happened to run first.
    #[test]
    fn traversal_order_is_fixed_outermost_first() {
        let mut event = transfer_event();
        event["Anonymous"] = json!(true);
        let reason = rejection_reason(json!({
            "id": "unknown-at-every-level",
            "event_abis": { "transfer": event },
            "callstack": { "entry": { "address": PROXY, "codehash": CODE_HASH } },
            "speculative": 1,
            "condition": { "==": [ { "var": "entry.matched" }, true ] },
            "action": "deny"
        }));
        assert_eq!(reason, "unknown field(s) in rule: speculative");
    }

    /// Unknown-field rejection is an ordinary per-rule rejection, so one typo'd rule among
    /// valid siblings still lands in `rejected` and leaves the batch incomplete — which is what
    /// `load_and_install` turns into "snapshot refused, last-known-good retained".
    #[test]
    fn one_typod_rule_among_valid_siblings_is_rejected_individually() {
        let valid: RawRule = serde_json::from_str(golden::RULE_SCENARIO_A).unwrap();
        let typod: RawRule = serde_json::from_value(json!({
            "id": "typo-sibling",
            "event_abis": { "transfer": transfer_event() },
            "callStack": {},
            "condition": true,
            "action": "deny"
        }))
        .unwrap();
        let set = load_rules(1, 1, vec![valid, typod]);
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rejected.len(), 1);
        assert_eq!(set.rejected[0].id, "typo-sibling");
        assert_eq!(set.rejected[0].reason, "unknown field(s) in rule: callStack");
    }

    /// A key echoed at exactly the byte bound is reproduced whole; one byte over is truncated
    /// and marked. Pins the boundary rather than only the obviously-too-long case.
    #[test]
    fn key_echo_boundary_is_exact() {
        let exact = "e".repeat(MAX_UNKNOWN_KEY_BYTES);
        assert_eq!(bound_key(&exact), exact);
        let over = "o".repeat(MAX_UNKNOWN_KEY_BYTES + 1);
        let bounded = bound_key(&over);
        assert_eq!(bounded.len(), MAX_UNKNOWN_KEY_BYTES);
        assert!(bounded.ends_with("..."));
    }

    /// Truncation never splits a multi-byte character.
    #[test]
    fn key_echo_truncation_respects_char_boundaries() {
        let key = "ä".repeat(100);
        let bounded = bound_key(&key);
        assert!(bounded.len() <= MAX_UNKNOWN_KEY_BYTES);
        assert!(bounded.ends_with("..."));
        assert!(bounded.trim_end_matches("...").chars().all(|c| c == 'ä'));
    }
}
