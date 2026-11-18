//! Direct checks of the request-level adjudication helpers moved from the audit RPC.

use alloy_primitives::{Address, B256};
use rcs_filter::audit::{observations_from, unadjudicable_observations};
use rcs_filter::callstack::{ObservationUnavailable, Observations, SnapshotStamp};
use rcs_filter::capture::{
    CallstackCaptureConfig, CaptureInvariantError, CaptureOutcome, CapturedObservations,
    ObservationBudget,
};
use rcs_filter::rules::{load_rules, RawRule, RuleSet};

// ---- fixtures ----

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

/// Every capture outcome maps to the observation state both callers read: a completed capture is
/// authoritative — even when it is empty — and every other outcome is an explicit unavailable
/// reason rather than an empty available set that would read as "captured, nothing matched".
#[test]
fn observations_from_maps_every_capture_outcome_variant() {
    let rules = selector_bearing_rules();
    let stamp = rules.stamp();

    let complete = CaptureOutcome::Complete {
        logs: Vec::new(),
        observations: CapturedObservations::new(stamp, Vec::new()),
    };
    assert_eq!(
        observations_from(&complete),
        Observations::available(stamp, &[]),
        "a completed empty capture is available, not unavailable"
    );

    assert_eq!(
        observations_from(&CaptureOutcome::LimitExceeded {
            attempted: 2,
            limit: 1,
            budget: ObservationBudget::Callstack,
        }),
        Observations::Unavailable { reason: ObservationUnavailable::BudgetExceeded }
    );
    // The native budget is never consulted in `CallstackOnly` mode, so reaching it is a
    // capture-state bug: unidentifiable, not ignorable.
    assert_eq!(
        observations_from(&CaptureOutcome::LimitExceeded {
            attempted: 2,
            limit: 1,
            budget: ObservationBudget::NativeTransfer,
        }),
        Observations::Unavailable { reason: ObservationUnavailable::Incomplete }
    );
    assert_eq!(
        observations_from(&CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::FrameStackUnbalanced { remaining: 1 },
        }),
        Observations::Unavailable { reason: ObservationUnavailable::Incomplete }
    );
    assert_eq!(observations_from(&CaptureOutcome::Passthrough), Observations::NOT_CAPTURED);
}

/// `from_rules` carries the snapshot's own stamp and its own selector set, so the two can never
/// come from different snapshots, and yields an inert configuration exactly when the snapshot
/// declares no selector.
#[test]
fn callstack_capture_config_from_rules_carries_the_snapshot_stamp_and_its_selectors() {
    let rules = selector_bearing_rules();
    let config = CallstackCaptureConfig::from_rules(&rules);
    assert_eq!(config.stamp_for_test(), Some(rules.stamp()));
    assert!(!config.is_inert(), "a selector-bearing snapshot observes");

    let selector_free = selector_free_rules();
    let inert = CallstackCaptureConfig::from_rules(&selector_free);
    assert_eq!(inert.stamp_for_test(), Some(selector_free.stamp()));
    assert!(inert.is_inert(), "a snapshot declaring no selector yields an inert configuration");
}
