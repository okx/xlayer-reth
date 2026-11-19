//! Capture-outcome post-processing: the fail-closed decision for every non-`Complete` outcome,
//! the no-snapshot branch, and `screen_captured` agreeing with the direct snapshot screening call.

use std::sync::Arc;

use alloy_primitives::{Address, Log, B256, U256};
use serde_json::json;

use rcs_filter::callstack::{CallObservation, Observations, SnapshotStamp};
use rcs_filter::capture::{
    CaptureInvariantError, CaptureOutcome, CapturedObservations, NativeTransferKind,
    ObservationBudget, RcsCaptureControl, RcsInspector,
};
use rcs_filter::handle::{FilterHandle, Screen, ScreenInput, ScreenTx};
use rcs_filter::rules::RuleSet;
use rcs_filter::test_support::log_builder::erc20_transfer;
use rcs_filter::{BufferStatus, FilterConfig, SystemClock};

use crate::callstack_screening::{
    approved_impl_code, delegated, deny_on_alias, hex_addr, implementation, proxy, rogue_code,
    rule_set, transfer_abi,
};

fn test_filter() -> FilterHandle {
    FilterHandle::for_test(FilterConfig::default(), RuleSet::default(), Arc::new(SystemClock))
}

/// An empty, available observation set: capture ran and genuinely saw no frame.
fn test_observations() -> CapturedObservations {
    CapturedObservations::new(
        SnapshotStamp { protocol_version: 1, content_version: 0, selector_digest: B256::ZERO },
        Vec::new(),
    )
}

/// Runs `f` under a capturing `tracing` subscriber and returns its result plus everything it
/// logged, so a test can assert the level, target, and exact bounded fields of a diagnostic and
/// prove no content leaks.
fn capture_tracing<R>(f: impl FnOnce() -> R) -> (R, String) {
    use std::io::Write;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl tracing_subscriber::fmt::MakeWriter<'_> for BufWriter {
        type Writer = BufWriter;
        fn make_writer(&self) -> BufWriter {
            self.clone()
        }
    }

    let buf = BufWriter(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_target(true)
        .with_writer(buf.clone())
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (out, logged)
}

/// An audit rule gated on a call-frame observation: the event supplies the action payload while
/// the observed frame acts as a condition.
fn audit_rule_set() -> RuleSet {
    rule_set(vec![json!({
        "id": "gated-audit",
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": { "==": [ { "var": "p.delegateTo" }, hex_addr(implementation()) ] },
        "action": "audit",
        "audit_types": ["delegatecall_review"]
    })])
}

fn transfer_log() -> Log {
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();
    erc20_transfer(token, from, to, U256::from(3u64))
}

fn capture_tx(tx_hash: B256) -> ScreenTx {
    ScreenTx {
        tx_hash,
        origin: "0x9999999999999999999999999999999999999999".parse().unwrap(),
        tx_to: Some(proxy()),
        nonce: 1,
        value: U256::ZERO,
        block_height: 100,
    }
}

// ---------------------------------------------------------------------------------------------
// M1–M6: moved from the payload builder's capture-decision tests
// ---------------------------------------------------------------------------------------------

#[test]
fn complete_is_not_fail_closed() {
    let filter = test_filter();
    assert_eq!(
        filter.capture_failure_decision(
            B256::ZERO,
            &CaptureOutcome::Complete { logs: vec![], observations: test_observations() }
        ),
        None
    );
}

#[test]
fn limit_exceeded_fails_closed() {
    let filter = test_filter();
    assert_eq!(
        filter.capture_failure_decision(
            B256::ZERO,
            &CaptureOutcome::LimitExceeded {
                attempted: 3,
                limit: 2,
                budget: ObservationBudget::NativeTransfer,
            }
        ),
        Some(Screen::Deny)
    );
}

#[test]
fn invariant_violation_fails_closed() {
    let filter = test_filter();
    let outcome = CaptureOutcome::InvariantViolation {
        reason: CaptureInvariantError::RealLogMismatch {
            first_bad_index: 0,
            observed: 1,
            expected: 2,
        },
    };
    assert_eq!(filter.capture_failure_decision(B256::ZERO, &outcome), Some(Screen::Deny));
}

#[test]
fn enabled_filter_passthrough_fails_closed() {
    // An enabled filter must never reuse raw logs on a `Passthrough`.
    let filter = test_filter();
    assert_eq!(
        filter.capture_failure_decision(B256::ZERO, &CaptureOutcome::Passthrough),
        Some(Screen::Deny)
    );
}

#[test]
fn native_overflow_emits_error_with_bounded_fields_only() {
    let filter = test_filter();
    let (decision, out) = capture_tracing(|| {
        filter.capture_failure_decision(
            B256::repeat_byte(0xCD),
            &CaptureOutcome::LimitExceeded {
                attempted: 10_001,
                limit: 10_000,
                budget: ObservationBudget::NativeTransfer,
            },
        )
    });
    assert_eq!(decision, Some(Screen::Deny));
    assert!(out.contains("ERROR"), "expected error-level diagnostic, got: {out}");
    assert!(out.contains("rcs_filter::capture"), "expected target, got: {out}");
    assert!(out.contains("observed_count"), "expected observed_count field, got: {out}");
    assert!(out.contains("10001"), "expected attempted count value, got: {out}");
    assert!(out.contains("limit"), "expected limit field, got: {out}");
}

#[test]
fn realogmismatch_emits_own_arm_four_fields_no_content() {
    // The `RealLogMismatch` arm must be DISTINCT from `Passthrough`: it emits EXACTLY tx_hash +
    // first_bad_index + observed + expected at error level, while `Passthrough` (an enabled filter
    // seeing a no-op outcome) stays a tx_hash-only fail-closed log. Neither may leak real-log or
    // transfer content — there is none to leak, because the decision fn takes only the tx hash and
    // bounded counts.
    let filter = test_filter();

    let (mismatch_decision, mismatch_out) = capture_tracing(|| {
        filter.capture_failure_decision(
            B256::repeat_byte(0xAB),
            &CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::RealLogMismatch {
                    first_bad_index: 7,
                    observed: 9,
                    expected: 11,
                },
            },
        )
    });
    assert_eq!(mismatch_decision, Some(Screen::Deny));
    assert!(mismatch_out.contains("ERROR"), "expected error level, got: {mismatch_out}");
    assert!(mismatch_out.contains("rcs_filter::capture"), "expected target, got: {mismatch_out}");
    assert!(mismatch_out.contains("tx_hash"), "expected tx_hash field, got: {mismatch_out}");
    // The four bounded fields, by exact `field=value`, so a timestamp digit cannot spoof them.
    assert!(
        mismatch_out.contains("first_bad_index=7"),
        "expected first_bad_index=7, got: {mismatch_out}"
    );
    assert!(mismatch_out.contains("observed=9"), "expected observed=9, got: {mismatch_out}");
    assert!(mismatch_out.contains("expected=11"), "expected expected=11, got: {mismatch_out}");
    // It is NOT the LimitExceeded arm (which emits `observed_count`, not `first_bad_index`).
    assert!(
        !mismatch_out.contains("observed_count"),
        "RealLogMismatch must not emit the LimitExceeded `observed_count` field: {mismatch_out}"
    );

    let (passthrough_decision, passthrough_out) = capture_tracing(|| {
        filter.capture_failure_decision(B256::repeat_byte(0xCD), &CaptureOutcome::Passthrough)
    });
    assert_eq!(passthrough_decision, Some(Screen::Deny));
    assert!(passthrough_out.contains("ERROR"), "expected error level, got: {passthrough_out}");
    assert!(
        passthrough_out.contains("tx_hash"),
        "Passthrough log must carry tx_hash, got: {passthrough_out}"
    );
    // Passthrough is tx_hash-only: it must NOT be merged with the RealLogMismatch arm and must
    // never carry the mismatch fields.
    assert!(
        !passthrough_out.contains("first_bad_index"),
        "Passthrough must not carry RealLogMismatch fields (distinct arms): {passthrough_out}"
    );
    assert!(
        !passthrough_out.contains("expected="),
        "Passthrough must stay tx_hash-only: {passthrough_out}"
    );
}

// ---------------------------------------------------------------------------------------------
// N1–N3: the remaining failure arms, each with its own bounded label
// ---------------------------------------------------------------------------------------------

#[test]
fn callstack_overflow_fails_closed_with_its_own_budget_label() {
    let filter = test_filter();
    let (decision, out) = capture_tracing(|| {
        filter.capture_failure_decision(
            B256::repeat_byte(0xCD),
            &CaptureOutcome::LimitExceeded {
                attempted: 1_001,
                limit: 1_000,
                budget: ObservationBudget::Callstack,
            },
        )
    });
    assert_eq!(decision, Some(Screen::Deny));
    assert!(out.contains("ERROR"), "expected error level, got: {out}");
    assert!(out.contains("rcs_filter::capture"), "expected target, got: {out}");
    assert!(out.contains("budget=\"callstack\""), "expected the callstack label, got: {out}");
    assert!(out.contains("observed_count"), "expected observed_count field, got: {out}");
    assert!(out.contains("limit=1000"), "expected the limit value, got: {out}");
    assert!(
        !out.contains("native_transfer"),
        "the callstack overflow must not be labelled as a native one: {out}"
    );
}

#[test]
fn frame_stack_invariants_fail_closed_with_bounded_fields() {
    let filter = test_filter();

    let (decision, out) = capture_tracing(|| {
        filter.capture_failure_decision(
            B256::repeat_byte(0x01),
            &CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::FrameStackOverflow { depth: 7 },
            },
        )
    });
    assert_eq!(decision, Some(Screen::Deny));
    assert!(out.contains("depth=7"), "expected the depth field, got: {out}");
    assert!(!out.contains("observed_count"), "bounded fields only: {out}");
    assert!(!out.contains("first_bad_index"), "bounded fields only: {out}");

    let (decision, out) = capture_tracing(|| {
        filter.capture_failure_decision(
            B256::repeat_byte(0x02),
            &CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::FrameStackUnbalanced { remaining: 3 },
            },
        )
    });
    assert_eq!(decision, Some(Screen::Deny));
    assert!(out.contains("remaining=3"), "expected the remaining field, got: {out}");
    assert!(!out.contains("observed_count"), "bounded fields only: {out}");
    assert!(!out.contains("first_bad_index"), "bounded fields only: {out}");
}

#[test]
fn create_address_mismatch_fails_closed_with_create_index_only() {
    let filter = test_filter();
    let (decision, out) = capture_tracing(|| {
        filter.capture_failure_decision(
            B256::repeat_byte(0x03),
            &CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::CreateAddressMismatch { create_index: 5 },
            },
        )
    });
    assert_eq!(decision, Some(Screen::Deny));
    assert!(out.contains("create_index=5"), "expected the create_index field, got: {out}");
    assert!(!out.contains("observed_count"), "bounded fields only: {out}");
    assert!(!out.contains("first_bad_index"), "bounded fields only: {out}");
    assert!(!out.contains("depth="), "bounded fields only: {out}");
    assert!(!out.contains("remaining="), "bounded fields only: {out}");
}

// ---------------------------------------------------------------------------------------------
// N4–N7: `screen_captured` / `screen_captured_without_snapshot`
// ---------------------------------------------------------------------------------------------

/// Runs the same inputs through `screen_captured` and the hand-built `screen_tx_with_snapshot`
/// call, on a fresh handle each side, and asserts both the decision and the resulting buffer state
/// agree. Returns the decision.
fn complete_paths_decision(rules: RuleSet, logs: Vec<Log>, items: Vec<CallObservation>) -> Screen {
    let tx_hash = B256::repeat_byte(0x42);
    let tx = capture_tx(tx_hash);

    let handle =
        FilterHandle::for_test(FilterConfig::default(), rules.clone(), Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();
    let captured = handle.screen_captured(
        &pinned,
        tx,
        &CaptureOutcome::Complete {
            logs: logs.clone(),
            observations: CapturedObservations::new(pinned.stamp(), items.clone()),
        },
    );
    let captured_status = handle.buffer_status(&tx_hash);

    let direct_handle =
        FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let direct_pinned = direct_handle.rule_snapshot();
    let input = ScreenInput {
        tx_hash,
        origin: tx.origin,
        tx_to: tx.tx_to,
        nonce: tx.nonce,
        value: tx.value,
        block_height: tx.block_height,
        logs: &logs,
        observations: Observations::available(direct_pinned.stamp(), &items),
    };
    let direct = direct_handle.screen_tx_with_snapshot(&input, &direct_pinned);
    let direct_status = direct_handle.buffer_status(&tx_hash);

    assert_eq!(captured, direct, "screen_captured must decide exactly as the direct call does");
    assert_eq!(captured_status, direct_status, "and must leave the same buffer state");
    captured
}

#[test]
fn screen_captured_complete_matches_screen_tx_with_snapshot() {
    // A matching deny rule.
    assert_eq!(
        complete_paths_decision(
            rule_set(vec![deny_on_alias("guard", "p", proxy(), json!(true))]),
            vec![],
            vec![delegated(implementation(), rogue_code())],
        ),
        Screen::Deny
    );
    // An audit rule whose event and observed frame both match.
    assert_eq!(
        complete_paths_decision(
            audit_rule_set(),
            vec![transfer_log()],
            vec![delegated(implementation(), approved_impl_code())],
        ),
        Screen::AuditPending
    );
    // A rule with selectors but no observed frame: the rule is suppressed.
    assert_eq!(
        complete_paths_decision(audit_rule_set(), vec![transfer_log()], Vec::new(),),
        Screen::Allow
    );
}

#[test]
fn screen_captured_complete_with_a_foreign_stamp_fails_closed() {
    let rules = rule_set(vec![deny_on_alias("guard", "p", proxy(), json!(true))]);
    let handle = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();

    let foreign = SnapshotStamp {
        protocol_version: 1,
        content_version: 99,
        selector_digest: B256::repeat_byte(0xEE),
    };
    let tx_hash = B256::repeat_byte(0x51);
    let decision = handle.screen_captured(
        &pinned,
        capture_tx(tx_hash),
        &CaptureOutcome::Complete {
            logs: vec![],
            observations: CapturedObservations::new(
                foreign,
                vec![delegated(implementation(), rogue_code())],
            ),
        },
    );
    assert_eq!(decision, Screen::Deny, "the foreign stamp must be forwarded, not replaced");
    assert_eq!(handle.buffer_status(&tx_hash), None);
}

#[test]
fn screen_captured_failure_skips_rule_evaluation_and_buffering() {
    let handle =
        FilterHandle::for_test(FilterConfig::default(), audit_rule_set(), Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();

    // Control: the same rule buffers a matching transaction as `AuditPending`.
    let control_hash = B256::repeat_byte(0x60);
    let control = handle.screen_captured(
        &pinned,
        capture_tx(control_hash),
        &CaptureOutcome::Complete {
            logs: vec![transfer_log()],
            observations: CapturedObservations::new(
                pinned.stamp(),
                vec![delegated(implementation(), approved_impl_code())],
            ),
        },
    );
    assert_eq!(control, Screen::AuditPending);
    assert_eq!(handle.buffer_status(&control_hash), Some(BufferStatus::NotSubmitted));

    let failures = [
        CaptureOutcome::LimitExceeded {
            attempted: 2,
            limit: 1,
            budget: ObservationBudget::NativeTransfer,
        },
        CaptureOutcome::LimitExceeded {
            attempted: 2,
            limit: 1,
            budget: ObservationBudget::Callstack,
        },
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::FrameStackUnbalanced { remaining: 1 },
        },
        CaptureOutcome::Passthrough,
    ];
    for (index, failure) in failures.iter().enumerate() {
        let tx_hash = B256::repeat_byte(0x70 + u8::try_from(index).unwrap());
        assert_eq!(
            handle.screen_captured(&pinned, capture_tx(tx_hash), failure),
            Screen::Deny,
            "a failed capture never reaches rule evaluation"
        );
        assert_eq!(
            handle.buffer_status(&tx_hash),
            None,
            "a failed capture never creates a buffer entry"
        );
    }
}

#[test]
fn screen_captured_without_snapshot_fails_closed_and_keeps_the_failure_diagnostic() {
    let filter = test_filter();
    let tx_hash = B256::repeat_byte(0xCD);

    let (decision, out) = capture_tracing(|| {
        filter.screen_captured_without_snapshot(
            tx_hash,
            &CaptureOutcome::Complete { logs: vec![], observations: test_observations() },
        )
    });
    assert_eq!(decision, Screen::Deny);
    assert!(
        out.contains("no rule snapshot was pinned for this block"),
        "expected the no-snapshot diagnostic, got: {out}"
    );

    let (decision, out) = capture_tracing(|| {
        filter.screen_captured_without_snapshot(
            tx_hash,
            &CaptureOutcome::LimitExceeded {
                attempted: 10_001,
                limit: 10_000,
                budget: ObservationBudget::NativeTransfer,
            },
        )
    });
    assert_eq!(decision, Screen::Deny);
    assert!(
        out.contains("observation budget exceeded"),
        "expected the budget diagnostic, got: {out}"
    );
    assert!(
        !out.contains("no rule snapshot was pinned"),
        "a failed capture keeps its own diagnostic, got: {out}"
    );
}

// ---------------------------------------------------------------------------------------------
// N8: the candidate profile uses the filter's native budget and the given snapshot
// ---------------------------------------------------------------------------------------------

#[test]
fn candidate_inspector_config_uses_the_filter_native_budget_and_the_given_snapshot() {
    let rules = rule_set(vec![deny_on_alias("guard", "p", proxy(), json!(true))]);
    let filter = FilterHandle::for_test(
        FilterConfig { max_native_transfers_per_tx: 3, ..Default::default() },
        rules,
        Arc::new(SystemClock),
    );
    let snapshot = filter.rule_snapshot();

    let mut inspector = RcsInspector::new(filter.candidate_inspector_config(Some(&snapshot)));
    inspector.start_capture();
    for index in 0..4u8 {
        inspector.observe_native_transfer(
            Address::repeat_byte(0xA0 + index),
            Address::repeat_byte(0xB0 + index),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
    }
    match inspector.finish_capture(&[]) {
        CaptureOutcome::LimitExceeded { attempted, limit, budget } => {
            assert_eq!(
                (attempted, limit, budget),
                (4, 3, ObservationBudget::NativeTransfer),
                "the filter's native budget bounds the candidate profile"
            );
        }
        other => panic!("expected LimitExceeded, got {other:?}"),
    }

    // The selector set and its stamp come from the snapshot passed in, never from the live rules.
    assert_eq!(
        filter.candidate_callstack_capture_config(Some(&snapshot)).stamp_for_test(),
        Some(snapshot.stamp())
    );
}
