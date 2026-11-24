//! Direct checks of the request-level adjudication helpers moved from the audit RPC.

use alloy_primitives::{Address, Bytes, Log, LogData, B256};
use rcs_filter::audit::{observations_from, screen_capture};
use rcs_filter::callstack::{ObservationUnavailable, Observations, SnapshotStamp};
use rcs_filter::capture::{
    CallstackCaptureConfig, CaptureInvariantError, CaptureOutcome, CapturedObservations,
    ObservationBudget,
};
use rcs_filter::rules::{load_rules, RawRule, RuleSet};

// ---- fixtures ----

fn log(byte: u8) -> Log {
    Log {
        address: Address::repeat_byte(byte),
        data: LogData::new_unchecked(Vec::new(), Bytes::new()),
    }
}

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

/// A request with no compiled rule at all.
fn empty_rules() -> RuleSet {
    load_rules(1, 0, vec![])
}

/// A `Complete` capture stamped with the evaluating snapshot, over `logs`.
fn complete(rules: &RuleSet, logs: Vec<Log>) -> CaptureOutcome {
    CaptureOutcome::Complete {
        logs,
        observations: CapturedObservations::new(rules.stamp(), Vec::new()),
    }
}

// ---- screen_capture ----

/// A completed capture always yields its OWN ordered logs — never the raw execution logs passed as
/// the fallback — and its own observations.
#[test]
fn screen_capture_complete_returns_the_capture_own_logs() {
    let rules = selector_free_rules();
    let captured = vec![log(0x11), log(0x12)];
    let capture = complete(&rules, captured.clone());
    let result_logs = vec![log(0x99)];

    let screen =
        screen_capture(&rules, &capture, &result_logs).expect("a completed capture is adjudicable");
    assert_eq!(screen.logs, captured.as_slice(), "the capture's logs, not the fallback");
    assert!(matches!(screen.observations, Observations::Available { .. }));
}

/// Native AND callstack budget overflows are unadjudicable regardless of which rule kinds the
/// request declared — including an event-only request that declares no callstack selector.
#[test]
fn screen_capture_rejects_every_budget_overflow_regardless_of_rule_kind() {
    for rules in [selector_bearing_rules(), selector_free_rules()] {
        for budget in [ObservationBudget::NativeTransfer, ObservationBudget::Callstack] {
            let capture = CaptureOutcome::LimitExceeded { attempted: 2, limit: 1, budget };
            assert!(
                matches!(
                    screen_capture(&rules, &capture, &[]),
                    Err(ObservationUnavailable::BudgetExceeded)
                ),
                "a {budget:?} overflow must be unadjudicable"
            );
        }
    }
}

/// An invariant violation is unadjudicable.
#[test]
fn screen_capture_rejects_invariant_violations() {
    let rules = selector_free_rules();
    let capture = CaptureOutcome::InvariantViolation {
        reason: CaptureInvariantError::FrameStackUnbalanced { remaining: 1 },
    };
    assert!(matches!(
        screen_capture(&rules, &capture, &[]),
        Err(ObservationUnavailable::Incomplete)
    ));
}

/// An observation set produced under a foreign stamp is unadjudicable.
#[test]
fn screen_capture_rejects_a_foreign_stamp() {
    let rules = selector_bearing_rules();
    let foreign = SnapshotStamp { selector_digest: B256::repeat_byte(0xEE), ..rules.stamp() };
    let capture = CaptureOutcome::Complete {
        logs: Vec::new(),
        observations: CapturedObservations::new(foreign, Vec::new()),
    };
    assert!(matches!(
        screen_capture(&rules, &capture, &[]),
        Err(ObservationUnavailable::SnapshotMismatch)
    ));
}

/// With an empty rule set, only `Passthrough` falls back to the raw execution logs; a completed
/// capture still supplies its own logs, and every non-`Complete` outcome is unadjudicable.
#[test]
fn screen_capture_empty_rules_only_passthrough_falls_back() {
    let rules = empty_rules();
    let result_logs = vec![log(0x99)];

    let passthrough =
        screen_capture(&rules, &CaptureOutcome::Passthrough, &result_logs).expect("plain EVM");
    assert_eq!(passthrough.logs, result_logs.as_slice(), "only no-capture screens raw logs");
    assert_eq!(passthrough.observations, Observations::NOT_CAPTURED);

    let captured = vec![log(0x11)];
    let binding = complete(&rules, captured.clone());
    let complete_screen = screen_capture(&rules, &binding, &result_logs)
        .expect("a completed capture is adjudicable even with no rules");
    assert_eq!(complete_screen.logs, captured.as_slice(), "no fallback for a real capture");

    assert!(matches!(
        screen_capture(
            &rules,
            &CaptureOutcome::LimitExceeded {
                attempted: 2,
                limit: 1,
                budget: ObservationBudget::NativeTransfer,
            },
            &result_logs,
        ),
        Err(ObservationUnavailable::BudgetExceeded)
    ));
    assert!(matches!(
        screen_capture(
            &rules,
            &CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::FrameStackUnbalanced { remaining: 1 },
            },
            &result_logs,
        ),
        Err(ObservationUnavailable::Incomplete)
    ));
}

/// A `Passthrough` with a rule-bearing request is a state-machine failure: fail closed instead of
/// silently screening the raw execution logs.
#[test]
fn screen_capture_passthrough_with_rules_fails_closed() {
    let rules = selector_free_rules();
    assert!(matches!(
        screen_capture(&rules, &CaptureOutcome::Passthrough, &[log(0x99)]),
        Err(ObservationUnavailable::NotCaptured)
    ));
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
    // Screen-time adjudicability rejects this outcome before the mapping, so only the test-only
    // recorder reaches it; it stays unidentifiable rather than ignorable.
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
