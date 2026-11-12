use super::*;

fn addr(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

fn real_log(byte: u8) -> Log {
    Log {
        address: Address::repeat_byte(byte),
        data: LogData::new_unchecked(vec![B256::repeat_byte(byte)], Bytes::new()),
    }
}

// ---- Task 4: native asset address + virtual Transfer encoder --------------------------------

#[test]
fn native_asset_address_matches_identifier() {
    assert_eq!(
        NATIVE_ASSET_ADDRESS,
        "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE".parse::<Address>().unwrap()
    );
}

#[test]
fn native_transfer_log_has_exact_shape() {
    let log = native_transfer_log(addr(0xAA), addr(0xBB), U256::from(123u64));
    assert_eq!(log.address, NATIVE_ASSET_ADDRESS);
    assert_eq!(log.topics()[0], keccak256("Transfer(address,address,uint256)"));
    assert_eq!(log.topics()[1], addr(0xAA).into_word());
    assert_eq!(log.topics()[2], addr(0xBB).into_word());
    assert_eq!(log.data.data.as_ref(), &U256::from(123u64).to_be_bytes::<32>());
}

// ---- Task 5: NoOpInspector capture-control seam ---------------------------------------------

#[test]
fn noop_inspector_finish_is_passthrough() {
    let mut inspector = NoOpInspector;
    inspector.start_capture();
    assert_eq!(inspector.finish_capture(&[]), CaptureOutcome::Passthrough);
    inspector.abort_capture();
}

// ---- Task 6: lifecycle, unified counter, sticky overflow ------------------------------------

#[test]
fn inactive_inspector_ignores_all_observations() {
    let mut inspector = RcsInspector::new(10);
    // Observations before start_capture are dropped by the inactive gate.
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    inspector.record_real_log(real_log(3));
    inspector.start_capture();
    assert_eq!(inspector.observed_count, 0);
    assert!(
        matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete { logs: v, .. } if v.is_empty())
    );
}

#[test]
fn inactive_inspector_push_pop_checkpoint_and_create_recipients_stay_clean() {
    // push_checkpoint / pop_checkpoint / create_recipients.push are all gated on self.active.
    // An inspector that was never started must leave all internal state untouched.
    let mut inspector = RcsInspector::new(10);
    inspector.push_checkpoint();
    inspector.push_checkpoint();
    inspector.push_checkpoint();
    assert!(inspector.checkpoints.is_empty(), "inactive push_checkpoint must not add entries");
    inspector.pop_checkpoint(false);
    inspector.pop_checkpoint(true);
    assert!(inspector.checkpoints.is_empty(), "inactive pop_checkpoint must not modify anything");
    assert!(
        inspector.create_recipients.is_empty(),
        "inactive inspector must leave create_recipients untouched"
    );
    assert!(inspector.queue.is_empty(), "inactive inspector must leave queue untouched");
    assert_eq!(inspector.observed_count, 0, "inactive inspector must leave observed_count at 0");
}

#[test]
fn observed_count_increments_and_does_not_roll_back() {
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.push_checkpoint(); // root frame checkpoint at 0
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    assert_eq!(inspector.observed_count, 3);
    // Roll the frame back: virtual events truncated, but the count stays.
    inspector.pop_checkpoint(true);
    assert!(inspector.queue.is_empty());
    assert_eq!(inspector.observed_count, 3);
}

#[test]
fn limit_th_allowed_limit_plus_one_overflows_sticky() {
    let mut inspector = RcsInspector::new(2);
    inspector.start_capture();
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    assert!(matches!(
        inspector.finish_capture(&[]),
        CaptureOutcome::LimitExceeded {
            attempted: 3,
            limit: 2,
            budget: ObservationBudget::NativeTransfer,
        }
    ));
}

#[test]
fn identical_from_to_value_not_deduplicated() {
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(5u64), NativeTransferKind::Call);
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(5u64), NativeTransferKind::Call);
    match inspector.finish_capture(&[]) {
        CaptureOutcome::Complete { logs, .. } => assert_eq!(logs.len(), 2),
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn zero_and_self_transfers_are_ignored() {
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.observe_native_transfer(addr(1), addr(2), U256::ZERO, NativeTransferKind::Call);
    inspector.observe_native_transfer(addr(1), addr(1), U256::from(9u64), NativeTransferKind::Call);
    assert_eq!(inspector.observed_count, 0);
    assert!(
        matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete { logs: v, .. } if v.is_empty())
    );
}

#[test]
fn start_capture_resets_state_between_txs() {
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    inspector.record_real_log(real_log(3));
    // A fresh window wipes everything.
    inspector.start_capture();
    assert!(inspector.queue.is_empty());
    assert_eq!(inspector.observed_count, 0);
    assert!(!inspector.overflowed);
}

// ---- Task 8: finish_capture RealLog cross-check ---------------------------------------------

#[test]
fn matching_real_logs_yield_complete() {
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.record_real_log(real_log(1));
    inspector.observe_native_transfer(addr(4), addr(5), U256::from(7u64), NativeTransferKind::Call);
    inspector.record_real_log(real_log(2));
    match inspector.finish_capture(&[real_log(1), real_log(2)]) {
        // real subsequence [1,2] == result.logs() [1,2]; virtual native log interleaves.
        CaptureOutcome::Complete { logs: stream, .. } => assert_eq!(stream.len(), 3),
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn real_log_count_mismatch_fails_closed() {
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.record_real_log(real_log(1));
    assert!(matches!(
        inspector.finish_capture(&[real_log(1), real_log(2)]),
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::RealLogMismatch { observed: 1, expected: 2, .. }
        }
    ));
}

#[test]
fn real_log_order_mismatch_fails_closed() {
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.record_real_log(real_log(1));
    inspector.record_real_log(real_log(2));
    assert!(matches!(
        inspector.finish_capture(&[real_log(2), real_log(1)]),
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::RealLogMismatch { first_bad_index: 0, .. }
        }
    ));
}

/// Asserts the inspector is fully reset the instant a budget-overflow finish returns — not only
/// after the next `start_capture`.
fn assert_capture_state_is_clean(inspector: &RcsInspector) {
    assert!(!inspector.active, "inspector must be inactive after finish");
    assert!(inspector.queue.is_empty(), "queue must be cleared on the failing exit");
    assert!(inspector.checkpoints.is_empty(), "checkpoints must be cleared on the failing exit");
    assert_eq!(inspector.observed_count, 0, "observed_count must be reset on the failing exit");
    assert!(!inspector.overflowed, "overflow flag must be reset on the failing exit");
    assert!(
        inspector.create_recipients.is_empty(),
        "create_recipients must be cleared on the failing exit"
    );
    assert_eq!(
        inspector.selfdestruct_journal_mark, None,
        "selfdestruct_journal_mark must be cleared on the failing exit"
    );
    assert!(
        !inspector.selfdestruct_wrote_journal,
        "selfdestruct_wrote_journal must be cleared on the failing exit"
    );
}

// ── T1a: finish_capture resets all state (includes create_recipients) on LimitExceeded ──────────
#[test]
fn finish_capture_clears_state_on_limit_exceeded_exit() {
    // A `LimitExceeded` finish must leave the inspector fully reset IMMEDIATELY (spec §5.2, R9
    // item 4): the earlier implementation returned on this path before draining the queue.
    let mut inspector = RcsInspector::new(1);
    inspector.start_capture();
    inspector.push_checkpoint();
    observe_n(&mut inspector, NativeTransferKind::Call, 2);
    // Drive the SELFDESTRUCT-watermark fields to a non-initial state first, so the clearing
    // assertion below is not vacuous.
    inspector.selfdestruct_journal_mark = Some(7);
    inspector.selfdestruct_wrote_journal = true;
    assert!(matches!(
        inspector.finish_capture(&[]),
        CaptureOutcome::LimitExceeded {
            attempted: 2,
            limit: 1,
            budget: ObservationBudget::NativeTransfer,
        }
    ));
    assert_capture_state_is_clean(&inspector);
}

#[test]
fn finish_capture_clears_state_on_invariant_exit() {
    // An `InvariantViolation` (RealLog mismatch) finish must ALSO leave the inspector fully reset
    // immediately, never leaking the captured queue into the next transaction (R9 item 4).
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.push_checkpoint();
    inspector.record_real_log(real_log(1));
    // Drive the SELFDESTRUCT-watermark fields to a non-initial state first, so the clearing
    // assertion below is not vacuous.
    inspector.selfdestruct_journal_mark = Some(7);
    inspector.selfdestruct_wrote_journal = true;
    // `result.logs()` is empty while one real log was captured → RealLogMismatch.
    assert!(matches!(
        inspector.finish_capture(&[]),
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::RealLogMismatch { observed: 1, expected: 0, .. }
        }
    ));
    assert_capture_state_is_clean(&inspector);
}

#[test]
fn new_selfdestruct_state_cleared_on_every_finish_exit_and_abort() {
    // Both new fields are driven to a NON-initial state first (so the clearing
    // assertion is not vacuous), then cleared on every exit: Complete, LimitExceeded,
    // InvariantViolation, and abort_capture.

    // Complete.
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.selfdestruct_journal_mark = Some(7);
    inspector.selfdestruct_wrote_journal = true;
    assert_eq!(inspector.selfdestruct_journal_mark, Some(7));
    assert!(inspector.selfdestruct_wrote_journal);
    assert!(matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete { .. }));
    assert_capture_state_is_clean(&inspector);

    // LimitExceeded.
    let mut inspector = RcsInspector::new(1);
    inspector.start_capture();
    inspector.push_checkpoint();
    observe_n(&mut inspector, NativeTransferKind::Call, 2);
    inspector.selfdestruct_journal_mark = Some(7);
    inspector.selfdestruct_wrote_journal = true;
    assert_eq!(inspector.selfdestruct_journal_mark, Some(7));
    assert!(inspector.selfdestruct_wrote_journal);
    assert!(matches!(
        inspector.finish_capture(&[]),
        CaptureOutcome::LimitExceeded {
            attempted: 2,
            limit: 1,
            budget: ObservationBudget::NativeTransfer,
        }
    ));
    assert_capture_state_is_clean(&inspector);

    // InvariantViolation.
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.push_checkpoint();
    inspector.record_real_log(real_log(1));
    inspector.selfdestruct_journal_mark = Some(7);
    inspector.selfdestruct_wrote_journal = true;
    assert_eq!(inspector.selfdestruct_journal_mark, Some(7));
    assert!(inspector.selfdestruct_wrote_journal);
    assert!(matches!(inspector.finish_capture(&[]), CaptureOutcome::InvariantViolation { .. }));
    assert_capture_state_is_clean(&inspector);

    // abort_capture.
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.selfdestruct_journal_mark = Some(7);
    inspector.selfdestruct_wrote_journal = true;
    assert_eq!(inspector.selfdestruct_journal_mark, Some(7));
    assert!(inspector.selfdestruct_wrote_journal);
    inspector.abort_capture();
    assert_capture_state_is_clean(&inspector);
}

#[test]
fn unconsumed_selfdestruct_flag_does_not_cross_windows() {
    // A SELFDESTRUCT that exits abnormally so the `selfdestruct` callback never fires
    // leaves `selfdestruct_wrote_journal` set; the NEXT window must not inherit it, and a
    // following ordinary (non-SELFDESTRUCT) transfer in that window must not be mis-attributed
    // or doubled.
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.selfdestruct_wrote_journal = true; // simulates the abnormal-exit leak
    let _ = inspector.finish_capture(&[]);
    assert_capture_state_is_clean(&inspector);

    inspector.start_capture();
    assert!(!inspector.selfdestruct_wrote_journal, "the next window must start clean");
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    match inspector.finish_capture(&[]) {
        CaptureOutcome::Complete { logs, .. } => {
            assert_eq!(logs.len(), 1, "the ordinary transfer must not be mis-attributed or doubled")
        }
        other => panic!("expected Complete with one transfer, got {other:?}"),
    }
}

// ---- Task 11: budget boundaries + unified SELFDESTRUCT budget -------------------------------

fn observe_n(inspector: &mut RcsInspector, kind: NativeTransferKind, n: usize) {
    for _ in 0..n {
        inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), kind);
    }
}

#[test]
fn native_budget_boundary_10000_allowed_10001_overflows() {
    let mut ok = RcsInspector::new(10_000);
    ok.start_capture();
    observe_n(&mut ok, NativeTransferKind::Call, 10_000);
    match ok.finish_capture(&[]) {
        CaptureOutcome::Complete { logs, .. } => assert_eq!(logs.len(), 10_000),
        other => panic!("expected Complete at limit, got {other:?}"),
    }

    let mut over = RcsInspector::new(10_000);
    over.start_capture();
    observe_n(&mut over, NativeTransferKind::Call, 10_001);
    assert!(matches!(
        over.finish_capture(&[]),
        CaptureOutcome::LimitExceeded {
            attempted: 10_001,
            limit: 10_000,
            budget: ObservationBudget::NativeTransfer,
        }
    ));
}

#[path = "capture__tests__capture_mode_tests.rs"]
#[cfg(test)]
mod capture_mode_tests;

#[test]
fn selfdestruct_shares_unified_budget_with_call() {
    // limit 1: one qualifying CALL then one qualifying SELFDESTRUCT ⇒ the second overflows.
    let mut inspector = RcsInspector::new(1);
    inspector.start_capture();
    inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), NativeTransferKind::Call);
    inspector.observe_native_transfer(
        addr(3),
        addr(4),
        U256::from(1u64),
        NativeTransferKind::SelfDestruct,
    );
    assert!(matches!(
        inspector.finish_capture(&[]),
        CaptureOutcome::LimitExceeded {
            attempted: 2,
            limit: 1,
            budget: ObservationBudget::NativeTransfer,
        }
    ));
}

#[test]
fn selfdestruct_then_parent_revert_removes_event_but_count_stays() {
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.push_checkpoint(); // parent frame
    inspector.observe_native_transfer(
        addr(3),
        addr(4),
        U256::from(9u64),
        NativeTransferKind::SelfDestruct,
    );
    assert_eq!(inspector.observed_count, 1);
    inspector.pop_checkpoint(true); // parent reverts
    assert!(inspector.queue.is_empty());
    assert_eq!(inspector.observed_count, 1); // count does not roll back
}

#[test]
fn selfdestruct_zero_or_self_target_consumes_no_budget() {
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.observe_native_transfer(
        addr(3),
        addr(4),
        U256::ZERO,
        NativeTransferKind::SelfDestruct,
    );
    inspector.observe_native_transfer(
        addr(3),
        addr(3),
        U256::from(9u64),
        NativeTransferKind::SelfDestruct,
    );
    assert_eq!(inspector.observed_count, 0);
    assert!(
        matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete { logs: v, .. } if v.is_empty())
    );
}

// ---- Task 7: context-free CALL/CREATE hook decision logic ------------------------------------
// Exercise the field/depth decisions the revm hooks delegate to (the hooks themselves only
// extract depth/inputs from the EVM context). Runtime execution through OpEvm is the follow-up.

fn armed(limit: usize) -> RcsInspector {
    let mut inspector = RcsInspector::new(limit);
    inspector.start_capture();
    inspector
}

#[test]
fn call_internal_nonzero_transfer_is_observed() {
    let mut i = armed(10);
    i.record_call_observation(Some(U256::from(5u64)), addr(1), addr(2));
    assert_eq!(i.observed_count, 1);
    match i.finish_capture(&[]) {
        CaptureOutcome::Complete { logs, .. } => {
            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0].address, NATIVE_ASSET_ADDRESS);
            assert_eq!(logs[0].topics()[1], addr(1).into_word());
            assert_eq!(logs[0].topics()[2], addr(2).into_word());
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn call_apparent_value_generates_no_event() {
    // DELEGATECALL / STATICCALL: transfer_value() is None.
    let mut i = armed(10);
    i.record_call_observation(None, addr(1), addr(2));
    assert_eq!(i.observed_count, 0);
}

#[test]
fn call_self_transfer_generates_no_event() {
    // CALLCODE reports a transfer value but caller == target, so it is a self-transfer.
    let mut i = armed(10);
    i.record_call_observation(Some(U256::from(5u64)), addr(1), addr(1));
    assert_eq!(i.observed_count, 0);
}

#[test]
fn create_internal_endowment_is_observed_to_created_address() {
    let mut i = armed(10);
    let created = addr(9);
    i.record_create_observation(U256::from(7u64), addr(1), created);
    match i.finish_capture(&[]) {
        CaptureOutcome::Complete { logs, .. } => {
            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0].topics()[2], created.into_word()); // recipient = created address
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn create_zero_endowment_generates_no_event() {
    let mut i = armed(10);
    i.record_create_observation(U256::ZERO, addr(1), addr(2));
    assert_eq!(i.observed_count, 0);
}

// ---- R8: per-operation, never netted -------------------------------------------------------

#[test]
fn reverse_transfers_are_two_ordered_events_not_netted() {
    // A→B: v then B→A: v (account net zero) must yield TWO ordered virtual Transfers, in order,
    // never offset/netted/deduped.
    let mut i = armed(10);
    i.observe_native_transfer(addr(0xA), addr(0xB), U256::from(1u64), NativeTransferKind::Call);
    i.observe_native_transfer(addr(0xB), addr(0xA), U256::from(1u64), NativeTransferKind::Call);
    match i.finish_capture(&[]) {
        CaptureOutcome::Complete { logs, .. } => {
            assert_eq!(logs.len(), 2, "net-zero pair must not be offset/deduped");
            // Execution order preserved: first A→B, then B→A.
            assert_eq!(logs[0].topics()[1], addr(0xA).into_word());
            assert_eq!(logs[0].topics()[2], addr(0xB).into_word());
            assert_eq!(logs[1].topics()[1], addr(0xB).into_word());
            assert_eq!(logs[1].topics()[2], addr(0xA).into_word());
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn reverse_transfers_parent_revert_removes_both_count_stays() {
    let mut i = armed(10);
    i.push_checkpoint(); // parent frame
    i.observe_native_transfer(addr(0xA), addr(0xB), U256::from(1u64), NativeTransferKind::Call);
    i.observe_native_transfer(addr(0xB), addr(0xA), U256::from(1u64), NativeTransferKind::Call);
    assert_eq!(i.observed_count, 2);
    i.pop_checkpoint(true); // parent reverts → both dropped from the stream
    assert!(i.queue.is_empty());
    assert_eq!(i.observed_count, 2, "observed_count must not roll back on revert");
}

#[test]
fn address_mismatch_flag_cleared_by_both_finish_and_abort() {
    // The sticky mismatch flag must not survive either exit path: `finish_capture` reports it
    // exactly once and clears it, and `abort_capture` clears it without reporting. A flag left
    // set would make the NEXT capture window fail closed on a stale mismatch.
    //
    // (a) armed flag → finish_capture surfaces it AND clears it.
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.address_mismatch_create_index = Some(3);
    let outcome = inspector.finish_capture(&[]);
    assert_eq!(
        outcome,
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::CreateAddressMismatch { create_index: 3 }
        },
        "an armed mismatch flag must surface as InvariantViolation on finish_capture"
    );
    assert!(
        inspector.address_mismatch_create_index.is_none(),
        "finish_capture must clear the sticky mismatch flag on its exit path"
    );

    // (b) armed flag → abort_capture clears it.
    inspector.start_capture();
    inspector.address_mismatch_create_index = Some(7);
    inspector.abort_capture();
    assert!(
        inspector.address_mismatch_create_index.is_none(),
        "abort_capture must clear the sticky mismatch flag on its exit path"
    );
    assert!(!inspector.active, "abort_capture must leave the inspector disarmed");
}

// ── T1 proof-boundary: abort_capture clears mid-window state ─────────────────────────────────

#[test]
fn abort_capture_mid_window_leaves_state_clean() {
    // abort_capture must reset all inspector state even when called with entries mid-flight:
    // a partially-filled queue, an open checkpoint, and an in-progress create_recipients entry.
    let mut inspector = RcsInspector::new(10);
    inspector.start_capture();
    inspector.push_checkpoint();
    inspector.observe_native_transfer(
        Address::from([0x01u8; 20]),
        Address::from([0x02u8; 20]),
        U256::from(5u64),
        NativeTransferKind::Call,
    );
    inspector.create_recipients.push(Some(Address::from([0x03u8; 20])));
    inspector.abort_capture();
    assert!(!inspector.active, "must be inactive after abort");
    assert!(inspector.queue.is_empty(), "queue must be cleared by abort");
    assert!(inspector.checkpoints.is_empty(), "checkpoints must be cleared by abort");
    assert_eq!(inspector.observed_count, 0, "observed_count must be reset by abort");
    assert!(!inspector.overflowed, "overflow flag must be reset by abort");
    assert!(inspector.create_recipients.is_empty(), "create_recipients must be cleared by abort");
}
