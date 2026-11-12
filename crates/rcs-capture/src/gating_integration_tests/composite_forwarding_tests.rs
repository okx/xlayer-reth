//! Capture through the production `PostExecCompositeInspector` on `evm.transact`: real-log
//! cross-check, revert rollback, interleaved transfers and composite warming.

use super::*;
use alloy_evm::{Evm, EvmEnv, EvmFactory};
use alloy_op_evm::{post_exec, OpEvmFactory, OpTx};
use op_revm::OpSpecId;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;

#[test]
fn internal_call_transfer_captured_through_composite() {
    // caller → contract (value 0); contract makes an internal CALL transferring 1 wei to dst.
    // Proves the composite forwards `call` to the inner RcsInspector at runtime, and that
    // `transfer_value`/`transfer_from`/`transfer_to` are read correctly. The top-level
    // caller→contract call carries zero transfer value so it generates no event.
    let contract = Address::from([0xCC; 20]);
    let dst = Address::from([0xDD; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(contract, account(1_000, Some(call_value_1_to(dst))));

    let (outcome, success) = run_capturing_tx(db, contract, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "exactly one internal native transfer expected");
            assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
            assert_eq!(stream[0].topics()[1], contract.into_word());
            assert_eq!(stream[0].topics()[2], dst.into_word());
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(1u64).to_be_bytes::<32>());
        }
        other => panic!("expected Complete with one native transfer, got {other:?}"),
    }
}

#[test]
fn internal_create_endowment_captured_through_composite() {
    // A CREATE with a non-zero endowment yields one native Transfer from the creator to
    // the created address (recipient derivation runs through the composite-forwarded `create`).
    // The factory is pre-inserted with nonce=0; CREATE at nonce=0 produces a deterministic addr.
    let factory = Address::from([0xF1; 20]);
    let expected_created = factory.create(0);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(factory, account(1_000, Some(create_endowment_7())));

    let (outcome, success) = run_capturing_tx(db, factory, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "one CREATE endowment transfer expected");
            assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
            assert_eq!(stream[0].topics()[1], factory.into_word());
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(7u64).to_be_bytes::<32>());
            // Recipient (topic2) must be the exact CREATE-derived address.
            assert_eq!(
                stream[0].topics()[2],
                expected_created.into_word(),
                "recipient must be the exact nonce-derived CREATE address"
            );
        }
        other => panic!("expected Complete with one CREATE transfer, got {other:?}"),
    }
}

#[test]
fn selfdestruct_transfer_captured_through_composite() {
    // SELFDESTRUCT (value != 0, contract != beneficiary) is a qualifying native transfer even in
    // the root frame — proves the composite forwards `selfdestruct` to the inner inspector.
    let victim = Address::from([0xE1; 20]);
    let beneficiary = Address::from([0xB1; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(victim, account(500, Some(selfdestruct_to(beneficiary))));

    let (outcome, success) = run_capturing_tx(db, victim, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "one SELFDESTRUCT transfer expected");
            assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
            assert_eq!(stream[0].topics()[1], victim.into_word());
            assert_eq!(stream[0].topics()[2], beneficiary.into_word());
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(500u64).to_be_bytes::<32>());
        }
        other => panic!("expected Complete with one SELFDESTRUCT transfer, got {other:?}"),
    }
}

/// Runtime bytecode: `CALL(gas, dst, value=0, 0,0,0,0); STOP` — a zero-value child call whose
/// failure is ignored.
fn call_value_0_to(dst: Address) -> Vec<u8> {
    let mut code = call_prefix(0x00, dst);
    code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
    code
}

/// Runtime bytecode: `LOG0(offset=0, size=0); REVERT(0, 0)` — emits one real (empty) log, then
/// reverts the frame.
fn log_then_revert() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0xa0, 0x60, 0x00, 0x60, 0x00, 0xfd]
}

#[test]
fn reverted_child_frame_rolls_back_real_log_and_cross_check_passes() {
    // A zero-value child CALL emits a real log then REVERTs. The captured real log must be
    // truncated on the sub-frame revert, and the finish_capture RealLog cross-check against
    // `result.logs()` (which excludes reverted logs) must still pass → Complete(empty).
    let parent = Address::from([0xA1; 20]);
    let child = Address::from([0xA2; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(parent, account(0, Some(call_value_0_to(child))));
    db.insert_account_info(child, account(0, Some(log_then_revert())));

    let (outcome, success) = run_capturing_tx(db, parent, 16);
    assert!(success, "parent tx ignores the child revert and succeeds");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert!(
                stream.is_empty(),
                "reverted child frame's real log must be rolled back, got {stream:?}"
            );
        }
        other => panic!("expected Complete(empty) after child revert, got {other:?}"),
    }
}

#[test]
fn internal_transfer_then_real_log_interleaved_captured_through_composite() {
    // A successful contract makes an internal value CALL and then emits a real LOG0. The stream
    // must contain BOTH the virtual native `Transfer` and the persisted real log, interleaved in
    // execution order [virtual, real]. This is the non-tautological real-log proof: if the
    // composite stopped forwarding `log`/`log_full` to the inner inspector, the RealLog
    // subsequence would be empty while `result.logs()` has one entry, so `finish_capture` would
    // fail closed with `InvariantViolation` instead of `Complete` — the assert below would fail.
    let contract = Address::from([0xC5; 20]);
    let dst = Address::from([0xD5; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(contract, account(1_000, Some(call_value_1_then_log0(dst))));

    let (outcome, success) = run_capturing_tx(db, contract, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 2, "one virtual transfer + one real log expected, in order");
            // [0] virtual native transfer (contract -> dst, value 1).
            assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
            assert_eq!(stream[0].topics()[1], contract.into_word());
            assert_eq!(stream[0].topics()[2], dst.into_word());
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(1u64).to_be_bytes::<32>());
            // [1] the persisted real LOG0 — proves real-log forwarding + the cross-check passed.
            assert_ne!(
                stream[1].address, NATIVE_ASSET_ADDRESS,
                "second entry must be the real log, not another virtual transfer"
            );
            assert_eq!(stream[1].address, contract, "real log carries the emitting contract addr");
        }
        other => panic!("expected Complete([virtual, real]) interleaved, got {other:?}"),
    }
}

#[test]
fn internal_create2_endowment_captured_through_composite() {
    // A CREATE2 with a non-zero endowment yields one native Transfer from the creator to the
    // CREATE2-derived address, through the composite-forwarded `create` hook on the production
    // `evm.transact` path (distinct from the plain-CREATE case).
    // The bytecode uses salt=0 and empty init code; the expected address is deterministic.
    let factory = Address::from([0xF2; 20]);
    let expected_created = factory.create2(B256::ZERO, keccak256(b""));
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));

    let (outcome, success) = run_capturing_tx(db, factory, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "one CREATE2 endowment transfer expected");
            assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
            assert_eq!(stream[0].topics()[1], factory.into_word());
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(7u64).to_be_bytes::<32>());
            // Recipient (topic2) must be the exact CREATE2-derived address.
            assert_eq!(
                stream[0].topics()[2],
                expected_created.into_word(),
                "recipient must be the exact salt+init_code_hash CREATE2 address"
            );
        }
        other => panic!("expected Complete with one CREATE2 transfer, got {other:?}"),
    }
}

/// Runtime bytecode: `LOG0(0, 0); STOP` — emits one real (empty) log then returns successfully.
fn log_then_stop() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0xa0, 0x00]
}

/// Runtime bytecode: `CALL(gas, child, value=1, 0,0,0,0); POP; REVERT(0, 0)` — a successful child
/// value-call (which emits a real log) followed by the PARENT frame reverting.
fn call_child_then_revert(child: Address) -> Vec<u8> {
    let mut code = call_prefix(0x01, child);
    // GAS CALL POP  PUSH1 0 (size) PUSH1 0 (offset) REVERT
    code.extend_from_slice(&[0x5a, 0xf1, 0x50, 0x60, 0x00, 0x60, 0x00, 0xfd]);
    code
}

#[test]
fn parent_frame_revert_rolls_back_child_transfer_and_real_log_through_composite() {
    // R9 item 2b / spec §8 test 32. A grandchild frame receives a qualifying native transfer and
    // emits a real log; its PARENT frame then reverts (while the top-level tx survives, ignoring
    // the failed sub-call). Through `evm.transact`, both the virtual transfer and the real log
    // must truncate at the parent checkpoint, and `result.logs()` (the reverted subtree's logs are
    // discarded) must match the now-empty RealLog subsequence → Complete(empty): the tx screens as
    // if the events never happened.
    let root = Address::from([0xA0; 20]); // calls parent (value 0), ignores its revert, succeeds
    let parent = Address::from([0xA1; 20]); // calls child (value 1) then REVERTs
    let child = Address::from([0xA2; 20]); // receives value + emits a real log, returns ok
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(root, account(0, Some(call_value_0_to(parent))));
    db.insert_account_info(parent, account(1_000, Some(call_child_then_revert(child))));
    db.insert_account_info(child, account(0, Some(log_then_stop())));

    let (outcome, success) = run_capturing_tx(db, root, 16);
    assert!(success, "the top-level tx ignores the parent frame's revert and succeeds");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => assert!(
            stream.is_empty(),
            "a parent-frame revert must truncate the child transfer AND its real log, got {stream:?}"
        ),
        other => panic!("expected Complete(empty) after a parent-frame revert, got {other:?}"),
    }
}

#[test]
fn runtime_forced_real_log_mismatch_fails_closed_through_composite() {
    // R9 item 2c / spec §8 test 33 (R10: the shared runtime SOURCE for test 35). A real tx emits a
    // real log (captured through the composite-forwarded `log` hook), then `finish_capture` runs
    // against a `result.logs()` forced to DIVERGE from the captured subsequence, so the cross-check
    // fails closed with `InvariantViolation { RealLogMismatch }`. Test 33 asserts the RUNTIME
    // OUTCOME; the builder `Screen::Deny` + no-pending-buffer + no-partial-action guarantees are
    // proven together on the real screening path by test 35 (context.rs), which consumes this same
    // fixture. Stopping at `InvariantViolation` does NOT close the fail-closed loop — that is test
    // 35's job.
    let outcome = crate::test_fixtures::real_runtime_reallogmismatch_outcome();
    assert!(
        matches!(
            outcome,
            CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::RealLogMismatch { observed: 1, expected: 2, .. }
            }
        ),
        "a runtime real-log divergence must fail closed with RealLogMismatch, got {outcome:?}"
    );
}

/// Runtime bytecode: `PUSH1 0; SLOAD; POP; STOP` — reads (warms) storage slot 0.
const WARMING_CONTRACT_CODE: [u8; 5] = [0x60, 0x00, 0x54, 0x50, 0x00];

#[test]
fn composite_post_exec_warming_preserved_with_rcs_inspector_inner() {
    // R9 item 2d / spec §8 item 9. Making `RcsInspector` the composite's INNER inspector must NOT
    // replace the composite's own SDM/post-exec warming. Drive two warming txs through the
    // production `evm.transact` on an `OpEvm<_, RcsInspector, _>` with post-exec tracking active:
    // the second tx re-touches a block-warmed slot and MUST still earn a warming refund, exactly
    // as it does with no capture inspector, while capture runs alongside (a clean empty Complete
    // each time — an SLOAD moves no native value).
    let target = Address::from([0x2C; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(target, account(0, Some(WARMING_CONTRACT_CODE.to_vec())));

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );

    // `transact` does not commit here, so nonce 0 is reused across both txs (as in the upstream
    // OpEvm post-exec test); block-scoped warming is carried on the composite across the two txs.
    let mut warm_once = |tx_index: u64| {
        evm.begin_post_exec_tx(post_exec::PostExecTxContext {
            tx_index,
            kind: post_exec::PostExecTxKind::Normal,
        });
        evm.components_mut().1.start_capture();
        let result = evm.transact(call_tx(target)).expect("warming tx executes");
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());
        assert!(
            matches!(outcome, CaptureOutcome::Complete { logs: ref s, .. } if s.is_empty()),
            "capture must run alongside warming (no native transfer from an SLOAD), got {outcome:?}"
        );
        evm.take_last_post_exec_tx_result().refund_total
    };

    assert_eq!(warm_once(0), 0, "the first tx warms the slot but earns no refund");
    assert!(
        warm_once(1) > 0,
        "the composite's SDM warming must still credit a refund on the second tx — making \
             RcsInspector the inner inspector did not replace the composite's post-exec behavior"
    );
}
