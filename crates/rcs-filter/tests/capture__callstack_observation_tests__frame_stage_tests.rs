//! Direct-driven capture stage and frame-stack tests: enter/confirm/exit, stack invariants,
//! staged-candidate commit/discard and lifecycle resets, without a real EVM round trip.

use super::*;

/// The same inertness, asserted on frame state rather than on hashing: with an empty selector
/// set no [`FrameRecord`] is pushed at all, and the window still closes as `Complete` — an
/// `enter_frame` that pushed while `exit_frame` did not pop (or the reverse) would surface
/// `FrameStackUnbalanced` instead.
#[test]
fn an_empty_selector_set_pushes_no_frame_record() {
    let any = Address::from([0xD7; 20]);
    let mut inspector = RcsInspector::with_callstack(1_000, 16, CallstackCaptureConfig::default());
    inspector.start_capture();
    inspector.enter_frame(any, any, Some(CallScheme::Call));
    assert_eq!(inspector.open_frame_count(), 0, "no frame is tracked for an inert selector set");
    inspector.exit_frame();
    assert_eq!(inspector.open_frame_count(), 0);
    match inspector.finish_capture(&[]) {
        CaptureOutcome::Complete { observations, .. } => assert!(observations.items.is_empty()),
        other => panic!("an inert window must close cleanly, got {other:?}"),
    }
}

#[test]
fn an_unbalanced_frame_stack_fails_closed() {
    // Driven directly, because a real EVM always balances its frames. A leftover entry means
    // parentage cannot be trusted, so the capture must not be reported as complete.
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        16,
        CallstackCaptureConfig::new(stamp(), vec![Address::repeat_byte(0x11)]),
    );
    inspector.start_capture();
    inspector.enter_frame(
        Address::repeat_byte(0x11),
        Address::repeat_byte(0x11),
        Some(CallScheme::Call),
    );
    match inspector.finish_capture(&[]) {
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::FrameStackUnbalanced { remaining },
        } => assert_eq!(remaining, 1),
        other => panic!("expected an unbalanced frame stack, got {other:?}"),
    }
}

#[test]
fn exhausting_the_frame_stack_bound_fails_closed() {
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        usize::MAX,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    inspector.start_capture();
    for _ in 0..=MAX_OPEN_FRAMES {
        inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    }
    match inspector.finish_capture(&[]) {
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::FrameStackOverflow { depth },
        } => assert_eq!(depth, MAX_OPEN_FRAMES),
        other => panic!("expected a frame-stack overflow, got {other:?}"),
    }
}

#[test]
fn a_staged_candidate_is_discarded_when_its_frame_never_executes() {
    // The stage/commit/discard discipline, exercised directly: entering a frame allocates no
    // observation, and leaving it without a confirmed execution leaves none behind.
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        16,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    inspector.start_capture();
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    assert!(inspector.observations.is_empty(), "staging allocates no observation");
    assert_eq!(inspector.callstack_observed_count, 0, "staging consumes no budget");
    inspector.exit_frame();
    assert!(observations(inspector.finish_capture(&[])).is_empty());

    // Whereas a confirmed execution commits exactly one.
    inspector.start_capture();
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    inspector.confirm_frame_execution(B256::repeat_byte(0xaa));
    inspector.exit_frame();
    assert_eq!(
        observations(inspector.finish_capture(&[])),
        vec![CallObservation::call(selected, B256::repeat_byte(0xaa))]
    );
}

#[test]
fn capture_state_does_not_leak_between_transactions() {
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        1,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    // First window overflows its observation budget.
    inspector.start_capture();
    for _ in 0..2 {
        inspector.enter_frame(selected, selected, Some(CallScheme::Call));
        inspector.confirm_frame_execution(B256::repeat_byte(0xaa));
        inspector.exit_frame();
    }
    assert!(matches!(
        inspector.finish_capture(&[]),
        CaptureOutcome::LimitExceeded { budget: ObservationBudget::Callstack, .. }
    ));
    // The next window starts clean: the sticky overflow and the counter are both reset.
    inspector.start_capture();
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    inspector.confirm_frame_execution(B256::repeat_byte(0xbb));
    inspector.exit_frame();
    assert_eq!(
        observations(inspector.finish_capture(&[])),
        vec![CallObservation::call(selected, B256::repeat_byte(0xbb))]
    );
}

#[test]
fn hooks_are_inert_until_capture_is_armed() {
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        16,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    // Not armed: a gasless pre-check or any non-candidate simulation must capture nothing.
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    inspector.confirm_frame_execution(B256::repeat_byte(0xaa));
    inspector.exit_frame();
    assert!(inspector.frames.is_empty());
    assert!(inspector.observations.is_empty());
    assert_eq!(inspector.callstack_observed_count, 0);
}

#[test]
fn aborting_a_capture_clears_the_observation_state() {
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        16,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    inspector.start_capture();
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    inspector.confirm_frame_execution(B256::repeat_byte(0xaa));
    inspector.abort_capture();
    assert!(inspector.observations.is_empty());
    assert!(inspector.frames.is_empty());
    assert_eq!(inspector.callstack_observed_count, 0);
}
