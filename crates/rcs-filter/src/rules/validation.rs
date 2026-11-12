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
    Action, CompiledDirectDelegatecall, CompiledEvent, CompiledInput, CompiledRule, EventAbi,
    RawCallstack, RawRule, RejectedRule, RuleSet, TimeoutAction, UnknownFields,
};

/// Fixed ERC20 `Transfer` quota shape (ordered input names).
const ERC20_TRANSFER_SHAPE: &[&str] = &["from", "to", "value"];
/// Fixed ERC1155 `TransferSingle` quota shape (ordered input names).
const ERC1155_TRANSFER_SINGLE_SHAPE: &[&str] = &["operator", "from", "to", "id", "value"];
/// ERC1155 `TransferBatch` quota shape advertised by the RCS rule ABI.
const ERC1155_TRANSFER_BATCH_SHAPE: &[&str] = &["operator", "from", "to", "ids", "values"];
/// The only audit type implemented today.
const QUOTA: &str = "quota";

/// The selected proxy's own address variable suffix bound per alias.
const PROXY_ADDRESS: &str = "proxyAddress";
/// The selected proxy's own executed-code-hash variable suffix bound per alias.
const PROXY_CODE_HASH: &str = "proxyCodeHash";
/// The direct-delegatecall target address variable suffix bound per alias.
const DELEGATE_TO: &str = "delegateTo";
/// The direct-delegatecall target's executed-code-hash variable suffix bound per alias.
const DELEGATE_CODE_HASH: &str = "delegateCodeHash";

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
/// order: the rule root, then events by name, then each event's inputs by index. The event map is
/// a `BTreeMap`, so "by name" is already sorted.
///
/// Running this as a single pass before every other per-rule check is what makes the reported
/// reason independent of which unrelated validation would otherwise have failed first.
///
/// The `callstack` section is deliberately absent from this traversal: it declares no
/// unknown-field map, because `RawCallstack` carries `deny_unknown_fields` and so an unknown key
/// inside it is already a deserialization failure that names the key. The rule root's own check
/// stays exactly as it was — it is what still catches a misspelled sibling such as `callStack`.
fn reject_unknown_rule_fields(raw: &RawRule) -> std::result::Result<(), String> {
    reject_unknown("rule", &raw.unknown)?;
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
///
/// There is no protocol-version-specific entry point: the whole rule schema, `callstack`
/// included, belongs to the single supported version, so compilation no longer varies with it.
/// `protocol_version` reaches [`RuleSet`] only through [`load_rules`], where its real remaining
/// job is the snapshot stamp.
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
    // First, and before every other per-rule check: an unknown wire key anywhere under this
    // rule means an author-visible constraint may be silently absent, so the reason that names
    // the offending key must win over any other rejection the same input would also trigger.
    reject_unknown_rule_fields(&raw)?;

    // A rule needs at least one selector family: with neither declared events nor a `callstack`
    // section it could never match. `event_abis` alone being empty is legitimate for a rule that
    // selects purely on call frames. An explicitly null `callstack` is NOT an absent one and is
    // reported by its own reason instead (see `compile_callstack`).
    if raw.event_abis.is_empty() && raw.callstack.is_none() {
        return Err("rule declares neither event_abis nor callstack".to_string());
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

    let callstack = compile_callstack(&raw)?;
    // Last, because it needs the compiled alias set: a condition may only read the four variables
    // an alias actually provides.
    reject_unknown_alias_variables(&raw.condition, &callstack)?;

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

/// Validates and compiles a rule's `callstack` section into its direct-delegatecall aliases.
///
/// Checks run in a fixed order so the reported reason is deterministic for a given rule: an
/// explicitly null section, an empty alias map, the per-rule alias bound, every alias address
/// encoding, an alias colliding with an event alias, a callstack-only `audit` rule (whose action
/// payload would have no content to carry), and finally an empty alias name.
///
/// An unknown key inside `callstack`, a wrong JSON type and a repeated alias never reach here:
/// they are rejected during deserialization, which is earlier and names the offending field.
fn compile_callstack(
    raw: &RawRule,
) -> std::result::Result<Vec<CompiledDirectDelegatecall>, String> {
    let section: &RawCallstack = match &raw.callstack {
        None => return Ok(Vec::new()),
        // An explicit null is not an absent field. Treating it as absent would let a rule the
        // author believes carries a call-frame constraint load as an unconstrained one.
        Some(None) => {
            return Err("callstack is explicitly null; omit the field instead".to_string());
        }
        Some(Some(section)) => section,
    };

    let aliases = &section.direct_delegatecalls;
    if aliases.is_empty() {
        return Err("direct_delegatecalls is empty".to_string());
    }
    if aliases.len() > super::MAX_DIRECT_DELEGATECALL_ALIASES {
        return Err(format!(
            "direct_delegatecalls declares {} aliases; at most {} are allowed",
            aliases.len(),
            super::MAX_DIRECT_DELEGATECALL_ALIASES
        ));
    }

    // Addresses first, for every alias, before any alias-name check: the encoding gate is the
    // one that protects the exact-match comparison the observation filter performs.
    let mut compiled = Vec::with_capacity(aliases.len());
    for (alias, address) in aliases {
        let address = parse_lower_hex_address(address).map_err(|_| {
            format!("direct_delegatecalls alias '{alias}' is not a lowercase 0x 20-byte address")
        })?;
        compiled.push(CompiledDirectDelegatecall { alias: alias.clone(), address });
    }

    for alias in aliases.keys() {
        if raw.event_abis.contains_key(alias) {
            return Err(format!(
                "direct_delegatecalls alias '{alias}' collides with an event_abis alias; condition variables share one flat namespace"
            ));
        }
    }

    if raw.action == Action::Audit && raw.event_abis.is_empty() {
        return Err("an audit rule selecting only on callstack declares no event payload content"
            .to_string());
    }

    // Only an EMPTY alias name is rejected. A non-empty non-ASCII name is a legitimate rule-local
    // identifier and must never be reported with this reason.
    if aliases.keys().any(|alias| alias.is_empty()) {
        return Err("direct_delegatecalls declares an empty alias name".to_string());
    }

    Ok(compiled)
}

/// Rejects a condition that reads a `<alias>.<field>` variable the rule's own aliases do not
/// provide, naming the variable.
///
/// This is the load-time guard for the one failure mode this contract cannot survive at runtime.
/// An unbound `var` resolves to `null`, and the evaluator treats a one-sided null as *not equal*,
/// so a retired or misspelled field name inside the negated membership test these rules are
/// written with inverts the rule into an unconditional match — a rule whose proxy was never
/// touched would object to unrelated transactions. Rejecting at load turns that class of
/// authoring mistake into a named, visible failure.
///
/// Scope is deliberately narrow: it fires only for a `<prefix>.<field>` variable whose `<prefix>`
/// is a declared alias of the SAME rule. Event variables, transaction-level variables
/// (`contract_address`, `origin`, `value`, `nonce`) and any unrelated name are untouched, so no
/// event-only rule can be affected.
fn reject_unknown_alias_variables(
    condition: &serde_json::Value,
    aliases: &[CompiledDirectDelegatecall],
) -> std::result::Result<(), String> {
    if aliases.is_empty() {
        return Ok(());
    }
    let mut names = std::collections::BTreeSet::new();
    super::jsonlogic::collect_var_names(condition, &mut names);
    for alias in aliases {
        let prefix = format!("{}.", alias.alias);
        for name in &names {
            let Some(field) = name.strip_prefix(prefix.as_str()) else { continue };
            if [PROXY_ADDRESS, PROXY_CODE_HASH, DELEGATE_TO, DELEGATE_CODE_HASH].contains(&field) {
                continue;
            }
            return Err(format!(
                "condition references unknown variable '{}.{field}'; direct_delegatecalls provides {PROXY_ADDRESS}, {PROXY_CODE_HASH}, {DELEGATE_TO} and {DELEGATE_CODE_HASH}",
                alias.alias
            ));
        }
    }
    Ok(())
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

#[path = "../../tests/rules__validation__tests.rs"]
#[cfg(test)]
mod tests;

#[path = "../../tests/rules__validation__strict_schema_tests.rs"]
#[cfg(test)]
mod strict_schema_tests;
