//! Top-level value transfers and per-transaction lifecycle: root/child reverts and isolation
//! between consecutive captures.

use super::*;
use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Signature, TxKind};
use op_revm::OpSpecId;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;

#[test]
fn inspector_state_does_not_cross_transactions_through_real_evm() {
    // Two candidate txs share ONE inspector (as in the real per-block loop), distinct senders so
    // neither depends on the other committing. tx1 makes TWO internal transfers against a budget
    // of 1, so it overflows. R10 item 6 / R9 item 4: `finish_capture` clears ALL capture state on
    // EVERY exit — including the `LimitExceeded` path — so the inspector is left clean IMMEDIATELY
    // when tx1 finishes; nothing residual is carried to the next `start_capture` for it to wipe.
    // tx2 therefore starts from an already-clean inspector regardless (start_capture's reset is
    // belt-and-suspenders). If the every-exit cleanup regressed, tx2 would inherit tx1's transfers
    // or be born overflowed and the asserts below would fail.
    let sender_a = Address::from([0xA5; 20]);
    let sender_b = Address::from([0xB5; 20]);
    let c1 = Address::from([0xC1; 20]);
    let d1a = Address::from([0xD1; 20]);
    let d1b = Address::from([0xD3; 20]);
    let c2 = Address::from([0xC2; 20]);
    let d2 = Address::from([0xD2; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender_a, account(1_000_000_000, None));
    db.insert_account_info(sender_b, account(1_000_000_000, None));
    db.insert_account_info(c1, account(1_000, Some(call_value_1_twice(d1a, d1b))));
    db.insert_account_info(c2, account(1_000, Some(call_value_1_to(d2))));

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(1),
    );

    // tx1 -> c1: two transfers over a budget of 1 ⇒ overflow (LimitExceeded).
    evm.components_mut().1.start_capture();
    let r1 = evm.transact(call_tx_from(sender_a, c1)).expect("tx1 executes");
    let o1 = evm.components_mut().1.finish_capture(r1.result.logs());
    assert!(
        matches!(o1, CaptureOutcome::LimitExceeded { .. }),
        "tx1 must overflow the native-transfer budget, got {o1:?}"
    );
    // R10 item 6: the `LimitExceeded` exit cleared ALL state IMMEDIATELY — assert the inspector is
    // clean right now, BEFORE tx2's `start_capture`, so cross-tx isolation does not depend on the
    // next reset. (Fields are visible: this test module is a descendant of `rcs_capture`.)
    {
        let insp = evm.components_mut().1;
        assert!(insp.queue.is_empty(), "queue cleared immediately on the LimitExceeded exit");
        assert!(insp.checkpoints.is_empty(), "checkpoints cleared immediately");
        assert_eq!(insp.observed_count, 0, "observed_count reset immediately");
        assert!(!insp.overflowed, "overflow flag reset immediately");
        assert!(!insp.active, "inspector inactive immediately after finish");
        assert!(
            insp.create_recipients.is_empty(),
            "create_recipients cleared immediately on the LimitExceeded exit"
        );
    }

    // tx2 -> c2 on the SAME (already-clean) inspector.
    evm.components_mut().1.start_capture();
    let r2 = evm.transact(call_tx_from(sender_b, c2)).expect("tx2 executes");
    let o2 = evm.components_mut().1.finish_capture(r2.result.logs());
    match o2 {
        CaptureOutcome::Complete { logs: s, .. } => {
            assert_eq!(s.len(), 1, "tx2 sees only its own transfer (state cleared on every exit)");
            assert_eq!(s[0].topics()[2], d2.into_word(), "tx2's transfer targets d2 only");
        }
        other => {
            panic!("tx2 expected clean Complete(1) from an already-clean tx1, got {other:?}")
        }
    }
}

// ── Regression: root CALL value transfer ─────────────────────────────────────────────────────

fn always_revert_code() -> Vec<u8> {
    // PUSH1 0 PUSH1 0 REVERT
    vec![0x60, 0x00, 0x60, 0x00, 0xfd]
}

fn call_with_value_to(value: u8, dst: Address) -> Vec<u8> {
    let mut code = call_prefix(value, dst);
    code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
    code
}

#[test]
fn top_level_call_with_value_emits_transfer() {
    // A top-level CALL tx with non-zero tx.value fires the call hook at journal depth=0.
    // The non-zero value must produce exactly one native Transfer event from the caller to
    // the target contract.
    let contract = Address::from([0xCC; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(contract, account(0, None));

    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Call(contract),
        value: U256::from(3u64),
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    let op_tx = OpTx::from_recovered_tx(&tx, caller());

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(op_tx).expect("tx executes");
    assert!(result.result.is_success(), "top-level CALL with value should succeed");
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());

    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "one native transfer expected from top-level call value");
            assert_eq!(stream[0].topics()[1], caller().into_word(), "from = caller");
            assert_eq!(stream[0].topics()[2], contract.into_word(), "to = contract");
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(3u64).to_be_bytes::<32>());
        }
        other => panic!("expected Complete with one transfer, got {other:?}"),
    }
}

// ── Regression: child-frame revert rollback ───────────────────────────────────────────────────

#[test]
fn root_success_child_revert_keeps_root_drops_child() {
    // Root CALL (value=5) succeeds; the contract makes an internal CALL (value=3) that
    // reverts. The root Transfer survives; the reverted child Transfer is removed.
    let contract = Address::from([0xC2; 20]);
    let revert_target = Address::from([0xED; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(contract, account(1_000, Some(call_with_value_to(0x03, revert_target))));
    db.insert_account_info(revert_target, account(0, Some(always_revert_code())));

    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Call(contract),
        value: U256::from(5u64),
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    let op_tx = OpTx::from_recovered_tx(&tx, caller());

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(op_tx).expect("tx executes");
    assert!(result.result.is_success(), "root tx should succeed");
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());

    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "only root Transfer survives; child dropped");
            assert_eq!(stream[0].topics()[1], caller().into_word(), "from = caller");
            assert_eq!(stream[0].topics()[2], contract.into_word(), "to = contract");
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(5u64).to_be_bytes::<32>());
        }
        other => panic!("expected Complete with one transfer, got {other:?}"),
    }
}

// ── Regression: root failure drops all transfers ──────────────────────────────────────────────

#[test]
fn whole_tx_root_failure_drops_all() {
    // Root tx reverts. No Transfer event should survive.
    let revert_target = Address::from([0xEC; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(revert_target, account(0, Some(always_revert_code())));

    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Call(revert_target),
        value: U256::from(5u64),
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    let op_tx = OpTx::from_recovered_tx(&tx, caller());

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(op_tx).expect("tx executes");
    assert!(!result.result.is_success(), "root tx should revert");
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());

    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert!(stream.is_empty(), "root revert must drop all transfers");
        }
        other => panic!("expected Complete(empty), got {other:?}"),
    }
}

// ── Regression: second start_capture resets all inspector state ───────────────────────────────

#[test]
fn next_capture_starts_clean() {
    // After a completed capture, start_capture on the same inspector starts clean.
    // Two distinct senders (both nonce=0) share one inspector so neither depends on
    // the other committing state changes.
    let sender_a = Address::from([0xA2; 20]);
    let sender_b = Address::from([0xA3; 20]);
    let contract = Address::from([0xC3; 20]);
    let revert_target = Address::from([0xEB; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender_a, account(1_000_000_000, None));
    db.insert_account_info(sender_b, account(1_000_000_000, None));
    db.insert_account_info(contract, account(1_000, Some(call_with_value_to(0x03, revert_target))));
    db.insert_account_info(revert_target, account(0, Some(always_revert_code())));

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );

    // First tx (sender_a): root value=5, internal CALL (value=3) reverts.
    let tx1 = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Call(contract),
        value: U256::from(5u64),
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    evm.components_mut().1.start_capture();
    let result1 = evm.transact(OpTx::from_recovered_tx(&tx1, sender_a)).expect("tx1");
    let _outcome1 = evm.components_mut().1.finish_capture(result1.result.logs());

    // Second tx (sender_b): zero value — must produce an independent, empty stream.
    let tx2 = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Call(contract),
        value: U256::ZERO,
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    evm.components_mut().1.start_capture();
    let result2 = evm.transact(OpTx::from_recovered_tx(&tx2, sender_b)).expect("tx2");
    let outcome2 = evm.components_mut().1.finish_capture(result2.result.logs());

    match outcome2 {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert!(stream.is_empty(), "second tx (zero value) must produce an empty stream");
        }
        other => panic!("expected Complete(empty) for second tx, got {other:?}"),
    }
}

// ── T1 proof-boundary: two consecutive captures are independent ───────────────────────────────

#[test]
fn two_consecutive_txs_capture_independently() {
    // State from one capture window must not carry over into the next. tx1 captures one
    // internal transfer; tx2 hits an empty contract (no code, no value) and must produce an
    // empty stream — not the leftovers from tx1.
    let sender1 = Address::from([0xA4; 20]);
    let sender2 = Address::from([0xA5; 20]);
    let target = Address::from([0xC4; 20]);
    let recv = Address::from([0xC5; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender1, account(1_000_000_000, None));
    db.insert_account_info(sender2, account(1_000_000_000, None));
    db.insert_account_info(target, account(1_000, Some(call_with_value_to(5, recv))));
    db.insert_account_info(recv, account(0, None));
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    evm.components_mut().1.start_capture();
    let result1 = evm.transact(call_tx_from(sender1, target)).expect("tx1 executes");
    let outcome1 = evm.components_mut().1.finish_capture(result1.result.logs());
    match outcome1 {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "tx1 must produce 1 transfer")
        }
        other => panic!("expected Complete with 1 transfer for tx1, got {other:?}"),
    }
    evm.components_mut().1.start_capture();
    let result2 = evm.transact(call_tx_from(sender2, recv)).expect("tx2 executes");
    let outcome2 = evm.components_mut().1.finish_capture(result2.result.logs());
    match outcome2 {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert!(stream.is_empty(), "tx2 must produce 0 transfers — no carry-over from tx1")
        }
        other => panic!("expected Complete(empty) for tx2, got {other:?}"),
    }
}
