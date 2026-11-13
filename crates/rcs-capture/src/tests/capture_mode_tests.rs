//! Capture-mode gating: `CallstackOnly` versus the default `NativeAndCallstack` mode, covering
//! the native and call-frame budgets and the stamped call-frame observations.

use super::*;

/// `CallstackOnly` leaves `observed_count` at zero and never reports a
/// `NativeTransfer` budget outcome, even when the transaction performs far more native
/// transfers than `max_native_transfers`. Without the mode gate, attaching the inspector to
/// the deposit or audit-RPC path would consume the native budget and could fail a capture
/// for a reason that has nothing to do with call-frame selectors.
#[test]
fn callstack_only_never_reports_a_native_budget_outcome() {
    let mut inspector = RcsInspector::callstack_only(usize::MAX, selector_config());
    assert_eq!(inspector.mode(), CaptureMode::CallstackOnly);
    inspector.start_capture();
    // 5 native transfers under a native budget that `NativeAndCallstack` would overflow at 1
    // (the mode does not even store a meaningful native limit — it is never consulted).
    observe_n(&mut inspector, NativeTransferKind::Call, 5);
    inspector.record_real_log(real_log(0x11));
    inspector.push_checkpoint();
    inspector.pop_checkpoint(false);

    // The real-log cross-check is skipped too: a NON-empty `result_logs` slice would make the
    // `NativeAndCallstack` cross-check report a mismatch here.
    match inspector.finish_capture(&[real_log(0x11), real_log(0x12)]) {
        CaptureOutcome::Complete { logs, observations } => {
            assert!(logs.is_empty(), "CallstackOnly produces no log channel at all");
            assert!(observations.items.is_empty());
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

/// the SAME sequence under `NativeAndCallstack` does overflow the
/// native budget. Without this the previous test could pass for the wrong reason (e.g. the
/// transfers never reaching the inspector at all).
#[test]
fn native_and_callstack_still_overflows_on_the_same_sequence() {
    let mut inspector = RcsInspector::with_callstack(1, usize::MAX, selector_config());
    assert_eq!(inspector.mode(), CaptureMode::NativeAndCallstack);
    inspector.start_capture();
    observe_n(&mut inspector, NativeTransferKind::Call, 5);
    assert!(matches!(
        inspector.finish_capture(&[]),
        CaptureOutcome::LimitExceeded {
            attempted: 5,
            limit: 1,
            budget: ObservationBudget::NativeTransfer,
        }
    ));
}

/// `NativeAndCallstack` is the default, so every pre-existing construction path keeps
/// exactly today's behaviour. The native-budget overflow boundary above
/// (`native_budget_boundary_10000_allowed_10001_overflows`) is the behavioural assertion;
/// this pins the mode itself so a future constructor cannot silently change it.
#[test]
fn default_mode_is_native_and_callstack() {
    assert_eq!(CaptureMode::default(), CaptureMode::NativeAndCallstack);
    assert_eq!(RcsInspector::new(10).mode(), CaptureMode::NativeAndCallstack);
    assert_eq!(
        RcsInspector::with_callstack(10, 10, CallstackCaptureConfig::default()).mode(),
        CaptureMode::NativeAndCallstack
    );
}

/// The callstack budget is the ONLY live budget in `CallstackOnly`, and it still fails closed:
/// a truncated observation set is not adjudicable.
#[test]
fn callstack_only_still_enforces_the_callstack_budget() {
    let mut inspector = RcsInspector::callstack_only(0, selector_config());
    inspector.start_capture();
    inspector.commit_observation(CallObservation::call(addr(0xAA), B256::repeat_byte(0xCC)));
    assert!(matches!(
        inspector.finish_capture(&[]),
        CaptureOutcome::LimitExceeded { budget: ObservationBudget::Callstack, .. }
    ));
}

/// A `CallstackOnly` window still produces real call-frame observations, stamped with the
/// snapshot that configured it — i.e. the mode switches off the native family only.
#[test]
fn callstack_only_still_produces_stamped_observations() {
    let config = selector_config();
    let stamp = config.stamp_for_test().expect("a configured selector set carries a stamp");
    let mut inspector = RcsInspector::callstack_only(usize::MAX, config);
    inspector.start_capture();
    let observation = CallObservation::call(addr(0xAA), B256::repeat_byte(0xCC));
    inspector.commit_observation(observation);
    match inspector.finish_capture(&[]) {
        CaptureOutcome::Complete { logs, observations } => {
            assert!(logs.is_empty());
            assert_eq!(observations.stamp, stamp);
            assert_eq!(observations.items, vec![observation]);
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

/// A selector-bearing configuration with a real stamp, for the mode tests above.
fn selector_config() -> CallstackCaptureConfig {
    CallstackCaptureConfig::new(
        SnapshotStamp {
            protocol_version: 2,
            content_version: 7,
            selector_digest: B256::repeat_byte(0x5A),
        },
        [addr(0xAA)],
    )
}
