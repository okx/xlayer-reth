//! Direct checks of the request-level adjudication helpers moved from the audit RPC.

use alloy_primitives::{keccak256, Address, B256};
use rcs_filter::audit::{callstack_only_projection, unadjudicable_observations};
use rcs_filter::callstack::{ObservationUnavailable, Observations, SnapshotStamp};
use rcs_filter::rules::{load_rules, RawRule, RuleSet};

// ---- fixtures, copied verbatim from xlayer-rpc capture_path_tests.rs ----

const PROXY: Address = Address::repeat_byte(0xAA);

/// `SSTORE(0, marker); STOP`. Under a `DELEGATECALL` this writes the PROXY's storage, which
/// makes "the state was committed" a direct observation rather than an inference. The marker
/// also gives the two implementations distinct code, hence distinct code hashes.
fn impl_code(marker: u8) -> Vec<u8> {
    vec![0x60, marker, 0x60, 0x00, 0x55, 0x00]
}

fn approved_hash() -> B256 {
    keccak256(impl_code(0x01))
}

/// The normative rule pattern: declare one alias on the proxy address, then deny when the
/// code it delegated into is NOT in the approved set. No guard variable is needed or
/// available: an alias that produced no observation suppresses the whole rule, and only a
/// direct delegatecall is ever a candidate, so the bound hash is always a real value.
fn proxy_impl_allowlist_rule() -> RawRule {
    proxy_impl_allowlist_rule_approving(&[approved_hash()])
}

/// The same pattern over an explicit approved set, for a fixture whose implementation is not
/// the default one. The shape is identical — only the allowlist contents differ.
fn proxy_impl_allowlist_rule_approving(approved: &[B256]) -> RawRule {
    let approved: Vec<String> = approved.iter().map(|h| format!("{h:#x}")).collect();
    serde_json::from_value(serde_json::json!({
        "id": "proxy-impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "in": [ { "var": "entry.delegateCodeHash" }, approved ] } },
        "action": "deny"
    }))
    .expect("the normative sample must deserialize")
}

/// An event rule declaring no `callstack` section at all.
fn event_only_rule() -> RawRule {
    serde_json::from_str(
        r#"{"id":"evt","event_abis":{"transfer":{"type":"event","name":"Transfer",
               "inputs":[{"name":"from","type":"address","indexed":true},
                         {"name":"to","type":"address","indexed":true},
                         {"name":"value","type":"uint256","indexed":false}],
               "anonymous":false}},"condition":true,"action":"deny"}"#,
    )
    .expect("valid event-only rule")
}

// ---- fixtures, moved from xlayer-rpc identification_reporting_tests.rs ----

/// An alias-bearing rule set, compiled exactly as the handler compiles a request's own.
fn selector_bearing_rules() -> RuleSet {
    let rule: RawRule = serde_json::from_value(serde_json::json!({
        "id": "proxy-impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": {
            "entry": format!("{:#x}", Address::repeat_byte(0xAA))
        } },
        "condition": { "!=": [ { "var": "entry.delegateTo" }, null ] },
        "action": "deny"
    }))
    .expect("the fixture rule deserializes");
    let rules = load_rules(1, 0, vec![rule]);
    assert!(rules.rejected.is_empty(), "the fixture rule must load clean");
    rules
}

/// An event rule declaring no call-frame alias.
fn selector_free_rules() -> RuleSet {
    let rule: RawRule = serde_json::from_str(
        r#"{"id":"evt","event_abis":{"transfer":{"type":"event","name":"Transfer",
               "inputs":[{"name":"from","type":"address","indexed":true}],
               "anonymous":false}},"condition":true,"action":"deny"}"#,
    )
    .expect("valid event-only rule");
    load_rules(1, 0, vec![rule])
}

/// The callstack-only projection used for `known_allowed` adjudication carries the SAME
/// snapshot stamp as the full set, which is what keeps the existing stamp check meaningful
/// instead of bypassed.
#[test]
fn callstack_only_projection_preserves_the_snapshot_stamp() {
    let raw = vec![event_only_rule(), proxy_impl_allowlist_rule()];
    let full = load_rules(1, 0, raw.clone());
    assert!(full.rejected.is_empty());
    let projected = callstack_only_projection(&full, &raw).expect("aliases are declared");
    assert_eq!(projected.stamp(), full.stamp());
    assert_eq!(projected.rules.len(), 1, "only the alias-bearing rule survives");
    assert!(callstack_only_projection(
        &load_rules(1, 0, vec![event_only_rule()]),
        &[event_only_rule()]
    )
    .is_none());
}

/// the gate refuses an observation set produced under a different rule snapshot, accepts a
/// genuinely empty set stamped by the evaluating one, and leaves a request that declares no
/// selector completely untouched.
#[test]
fn the_request_level_gate_separates_unavailable_from_empty() {
    let rules = selector_bearing_rules();
    let foreign = SnapshotStamp { selector_digest: B256::repeat_byte(0xEE), ..rules.stamp() };

    assert_eq!(
        unadjudicable_observations(&rules, Observations::available(foreign, &[])),
        Some(ObservationUnavailable::SnapshotMismatch),
        "a foreign stamp cannot be read as captured-and-empty"
    );
    assert_eq!(
        unadjudicable_observations(&rules, Observations::available(rules.stamp(), &[])),
        None,
        "capture ran and genuinely observed nothing: adjudicable"
    );
    assert_eq!(
        unadjudicable_observations(&rules, Observations::NOT_CAPTURED),
        Some(ObservationUnavailable::NotCaptured)
    );
    assert_eq!(
        unadjudicable_observations(
            &rules,
            Observations::Unavailable { reason: ObservationUnavailable::BudgetExceeded }
        ),
        Some(ObservationUnavailable::BudgetExceeded),
        "the reason is preserved, not collapsed"
    );
    assert_eq!(
        unadjudicable_observations(&selector_free_rules(), Observations::NOT_CAPTURED),
        None,
        "a request declaring no selector keeps its pre-existing path byte for byte"
    );
}
