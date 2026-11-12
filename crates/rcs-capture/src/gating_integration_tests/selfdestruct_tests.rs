//! SELFDESTRUCT-to-self attribution and its controls: existing vs created contracts, journal-write
//! gating, out-of-gas leaks across windows and inactive gates.

use super::*;
use alloy_evm::{Evm, EvmEnv, EvmFactory};
use alloy_op_evm::{OpEvmFactory, OpTx};
use op_revm::OpSpecId;
use rcs_filter::callstack::Observations;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;
// ── screen_tx integration tests ───────────────────────────────────────────
// These tests run a real OpEvm transaction, capture the native transfer
// stream, then feed it directly into FilterHandle::screen_tx, proving
// the full pipeline from EVM execution through the rule engine.

/// Runtime bytecode: `CALL(stipend, dst, value=1, 0, 0, 0, 0); STOP` — a value-bearing
/// internal transfer with a FIXED gas immediate (`PUSH2 stipend`) instead of `GAS`, so the
/// callee frame's gas can be shaped to run out mid-execution. The idiom mirrors
/// `delegatecall_to_then_stop`'s `PUSH3 500000` in place of `GAS`. The callee's actual gas is
/// `stipend + 2300`, because a value-bearing `CALL` adds the 2,300-gas call stipend on top of
/// whatever is explicitly requested; a small `stipend` keeps the EIP-150 63/64-forwarding cap
/// irrelevant.
fn call_fixed_gas_value_1_to(stipend: u16, dst: Address) -> Vec<u8> {
    let mut code = call_prefix(0x01, dst);
    code.extend_from_slice(&[0x61, (stipend >> 8) as u8, (stipend & 0xff) as u8]); // PUSH2 stipend
    code.extend_from_slice(&[0xf1, 0x00]); // CALL STOP
    code
}

/// Runtime bytecode: `ADDRESS; SELFDESTRUCT` — a Cancun existing-contract SELFDESTRUCT whose
/// beneficiary is itself. On the pinned Cancun-or-later branch this writes NO journal entry.
fn address_then_selfdestruct() -> Vec<u8> {
    vec![0x30, 0xff] // ADDRESS SELFDESTRUCT
}

/// Runtime bytecode: `STOP` — the control body for the SELFDESTRUCT-to-self fixtures.
fn stop_only() -> Vec<u8> {
    vec![0x00] // STOP
}

/// Runtime bytecode: two sequential `CALL(gas, dst, value=1, 0,0,0,0)` to the same `dst`,
/// each with its own `GAS; CALL; POP` so the second call's args sit above the first's leftover
/// return value. Used to prove the fix is not content-based: two legitimate identical transfers.
fn call_value_1_twice_to(dst: Address) -> Vec<u8> {
    let mut code = call_prefix(0x01, dst);
    code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
    code.extend_from_slice(&call_prefix(0x01, dst));
    code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
    code
}

/// Runtime bytecode: `PUSH1 1; PUSH1 0; SSTORE` prologue, then `CALL(gas, dst, value=1, …); STOP`
/// — an unrelated journal-writing opcode immediately before a value-bearing internal CALL.
fn sstore_then_call_value_1_to(dst: Address) -> Vec<u8> {
    let mut code = vec![0x60, 0x01, 0x60, 0x00, 0x55]; // PUSH1 1; PUSH1 0; SSTORE
    code.extend_from_slice(&call_value_1_to(dst));
    code
}

/// Runtime bytecode: `CALL(value=1, victim)`, then `SSTORE`, then `CALL(value=0, victim2); STOP`.
/// Used to prove a journal-writing opcode earlier in the SAME window does not arm a later,
/// unrelated, no-write `SELFDESTRUCT`.
fn call_1_then_sstore_then_call_0_to(victim: Address, victim2: Address) -> Vec<u8> {
    let mut code = call_prefix(0x01, victim);
    code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
    code.extend_from_slice(&[0x60, 0x01, 0x60, 0x00, 0x55]); // PUSH1 1; PUSH1 0; SSTORE
    code.extend_from_slice(&call_prefix(0x00, victim2));
    code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
    code
}

#[test]
fn existing_contract_selfdestruct_to_self_is_not_captured() {
    // Cancun existing-contract SELFDESTRUCT-to-self writes NO journal entry. A stale-callback
    // replay bug would double-count the enclosing CALL's own transfer here.
    let entry = Address::from([0x51; 20]);
    let victim = Address::from([0x52; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
    db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

    let (outcome, success) = run_capturing_tx(db, entry, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(
                stream.len(),
                1,
                "expected exactly the enclosing CALL's transfer, no replay from SELFDESTRUCT"
            );
            assert_eq!(stream[0].topics()[1], entry.into_word());
            assert_eq!(stream[0].topics()[2], victim.into_word());
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(1u64).to_be_bytes::<32>());
        }
        other => panic!("expected Complete with exactly one transfer, got {other:?}"),
    }
}

#[test]
fn stop_control_matches_selfdestruct_to_self_capture_count() {
    // Control for the test above: the victim just STOPs instead of self-destructing. The
    // identical single-transfer count isolates SELFDESTRUCT-to-self as the only variable.
    let entry = Address::from([0x51; 20]);
    let victim = Address::from([0x52; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
    db.insert_account_info(victim, account(0, Some(stop_only())));

    let (outcome, success) = run_capturing_tx(db, entry, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1);
            assert_eq!(stream[0].topics()[1], entry.into_word());
            assert_eq!(stream[0].topics()[2], victim.into_word());
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(1u64).to_be_bytes::<32>());
        }
        other => panic!("expected Complete with exactly one transfer, got {other:?}"),
    }
}

#[test]
fn selfdestruct_to_self_consumes_no_native_budget() {
    // Sharpest discriminator: at budget 1, a stale replay would double-count and overflow to
    // LimitExceeded instead of completing.
    let entry = Address::from([0x51; 20]);
    let victim = Address::from([0x52; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
    db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

    let (outcome, success) = run_capturing_tx(db, entry, 1);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => assert_eq!(stream.len(), 1),
        other => panic!("expected Complete (budget 1 is not exceeded), got {other:?}"),
    }
}

#[test]
fn stop_control_at_budget_1_completes() {
    // Control for the test above: proves budget 1 is genuinely at the limit, so the previous
    // test's pass is not slack from an oversized budget.
    let entry = Address::from([0x51; 20]);
    let victim = Address::from([0x52; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
    db.insert_account_info(victim, account(0, Some(stop_only())));

    let (outcome, success) = run_capturing_tx(db, entry, 1);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => assert_eq!(stream.len(), 1),
        other => panic!("expected Complete at budget 1, got {other:?}"),
    }
}

#[test]
fn two_identical_native_transfers_are_both_observed() {
    // The fix must be structural, not content-based. Two legitimate identical transfers in
    // one transaction must both still be observed.
    let entry = Address::from([0x57; 20]);
    let dst = Address::from([0x58; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_twice_to(dst))));

    let (outcome, success) = run_capturing_tx(db, entry, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 2, "two identical transfers must both be observed");
            assert_eq!(stream[0].topics(), stream[1].topics());
            assert_eq!(stream[0].data.data, stream[1].data.data);
        }
        other => panic!("expected Complete with two transfers, got {other:?}"),
    }
}

#[test]
fn nested_selfdestruct_to_self_after_unrelated_journal_entry_is_not_captured() {
    // The stale entry can come from anywhere earlier in the (flat, transaction-wide) journal,
    // not just the immediately enclosing frame. A shape two frames deep with an intervening
    // SSTORE between the value-bearing CALL and the SELFDESTRUCT.
    let entry = Address::from([0x61; 20]);
    let mid = Address::from([0x62; 20]);
    let victim = Address::from([0x63; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_to(mid))));
    db.insert_account_info(mid, account(10, Some(sstore_then_call_value_1_to(victim))));
    db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

    let (outcome, success) = run_capturing_tx(db, entry, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(
                stream.len(),
                2,
                "expected only the two real value-bearing CALLs, no SELFDESTRUCT replay"
            );
        }
        other => panic!("expected Complete with exactly two transfers, got {other:?}"),
    }
}

#[test]
fn journal_writing_opcode_does_not_arm_a_later_selfdestruct() {
    // An earlier journal-writing opcode (SSTORE) in the SAME window must not leak into a
    // later, unrelated, no-write SELFDESTRUCT. The watermark is taken per-opcode, not merely
    // "did the journal grow at some point in this window".
    let entry = Address::from([0x64; 20]);
    let victim = Address::from([0x65; 20]);
    let victim2 = Address::from([0x66; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(
        entry,
        account(10, Some(call_1_then_sstore_then_call_0_to(victim, victim2))),
    );
    db.insert_account_info(victim2, account(0, Some(address_then_selfdestruct())));

    let (outcome, success) = run_capturing_tx(db, entry, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(
                stream.len(),
                1,
                "only the real value-bearing CALL should be observed; the SSTORE must not \
                     arm the later no-write SELFDESTRUCT"
            );
        }
        other => panic!("expected Complete with exactly one transfer, got {other:?}"),
    }
}

/// Runtime bytecode: two sequential `CALL(gas, dst, value=1, 0,0,0,0)` to two DIFFERENT
/// destinations, each with its own `GAS; CALL; POP`.
fn call_value_1_to_two(a: Address, b: Address) -> Vec<u8> {
    let mut code = call_prefix(0x01, a);
    code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
    code.extend_from_slice(&call_prefix(0x01, b));
    code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
    code
}

/// A `quota` audit rule matching `Transfer(address,address,uint256)` on
/// [`NATIVE_ASSET_ADDRESS`], for driving a real capture stream through `rcs-filter` screening.
fn quota_native_transfer_rule() -> rcs_filter::rules::RuleSet {
    let json = format!(
        r#"{{
              "id": "quota-native-transfer",
              "event_abis": {{
                "transfer": {{
                  "type": "event", "name": "Transfer",
                  "inputs": [
                    {{ "name": "from", "type": "address", "indexed": true }},
                    {{ "name": "to", "type": "address", "indexed": true }},
                    {{ "name": "value", "type": "uint256", "indexed": false }}
                  ],
                  "anonymous": false
                }}
              }},
              "audit_types": ["quota"],
              "condition": {{ "==": [{{ "var": "transfer.address" }}, "{native:#x}"] }},
              "action": "audit",
              "audit_timeout_action": "allow"
            }}"#,
        native = NATIVE_ASSET_ADDRESS
    );
    let raw: rcs_filter::rules::RawRule = serde_json::from_str(&json).expect("valid rule json");
    rcs_filter::rules::load_rules(1, 0, vec![raw])
}

#[test]
fn selfdestruct_to_self_quota_actions_report_one_transfer() {
    // Third control dimension: drive the real capture stream through rcs-filter screening and
    // assert exactly one quota ActionItem, not two.
    let entry = Address::from([0x51; 20]);
    let victim = Address::from([0x52; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
    db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

    let (outcome, success) = run_capturing_tx(db, entry, 16);
    assert!(success, "top-level tx should succeed");
    let stream = match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => stream,
        other => panic!("expected Complete, got {other:?}"),
    };

    let rules = quota_native_transfer_rule();
    let input = rcs_filter::handle::ScreenInput {
        tx_hash: B256::ZERO,
        origin: entry,
        tx_to: Some(victim),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &stream,
        observations: Observations::NOT_CAPTURED,
    };
    match rcs_filter::matching::evaluate(&rules, &input) {
        rcs_filter::matching::MatchOutcome::Audit { actions, .. } => {
            let quota = actions.get("quota").expect("quota audit_type key present");
            assert_eq!(quota.len(), 1, "expected exactly one quota action, not a replay");
            assert_eq!(quota[0].params.get("value").and_then(|v| v.as_str()), Some("1"));
        }
        other => panic!("expected Audit with quota actions, got {other:?}"),
    }
}

#[test]
fn stop_control_quota_actions_report_one_transfer() {
    // Control for the test above: identical single ActionItem.
    let entry = Address::from([0x51; 20]);
    let victim = Address::from([0x52; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
    db.insert_account_info(victim, account(0, Some(stop_only())));

    let (outcome, success) = run_capturing_tx(db, entry, 16);
    assert!(success, "top-level tx should succeed");
    let stream = match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => stream,
        other => panic!("expected Complete, got {other:?}"),
    };

    let rules = quota_native_transfer_rule();
    let input = rcs_filter::handle::ScreenInput {
        tx_hash: B256::ZERO,
        origin: entry,
        tx_to: Some(victim),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &stream,
        observations: Observations::NOT_CAPTURED,
    };
    match rcs_filter::matching::evaluate(&rules, &input) {
        rcs_filter::matching::MatchOutcome::Audit { actions, .. } => {
            let quota = actions.get("quota").expect("quota audit_type key present");
            assert_eq!(quota.len(), 1);
            assert_eq!(quota[0].params.get("value").and_then(|v| v.as_str()), Some("1"));
        }
        other => panic!("expected Audit with quota actions, got {other:?}"),
    }
}

#[test]
fn genuine_and_self_targeted_selfdestructs_in_one_tx_report_only_the_genuine() {
    // A genuine SELFDESTRUCT and a to-self SELFDESTRUCT in the same transaction, in both call
    // orders. An over-broad guard that suppressed the genuine one, or a flag that carried from
    // the first SELFDESTRUCT to the second, would under- or over-report here.
    let entry = Address::from([0x71; 20]);
    let victim_a = Address::from([0x72; 20]); // genuine: selfdestructs to `beneficiary`
    let victim_b = Address::from([0x73; 20]); // to-self: contributes nothing
    let beneficiary = Address::from([0x74; 20]);

    let run = |first: Address, second: Address| -> Vec<Log> {
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to_two(first, second))));
        db.insert_account_info(victim_a, account(0, Some(selfdestruct_to(beneficiary))));
        db.insert_account_info(victim_b, account(0, Some(address_then_selfdestruct())));

        let (outcome, success) = run_capturing_tx(db, entry, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete { logs: stream, .. } => stream,
            other => panic!("expected Complete, got {other:?}"),
        }
    };

    for (first, second, label) in
        [(victim_a, victim_b, "genuine-then-self"), (victim_b, victim_a, "self-then-genuine")]
    {
        let stream = run(first, second);
        assert_eq!(
            stream.len(),
            3,
            "{label}: expected the two CALL transfers plus the one genuine SELFDESTRUCT transfer"
        );
        let genuine =
            stream.iter().filter(|log| log.topics()[2] == beneficiary.into_word()).count();
        assert_eq!(genuine, 1, "{label}: exactly one transfer must reach the beneficiary");
    }
}

#[test]
fn inactive_inspector_selfdestruct_leaves_new_state_untouched() {
    // A real, NON-capturing OpEvm run (never armed) executing a SELFDESTRUCT-to-self
    // must leave the new watermark fields at their initial values and must never read the
    // journal for the watermark — the seam proves the hook was reached but the active branch
    // never ran, distinguishing "never hit" from "hit while inactive".
    reset_selfdestruct_gate_seams();
    let entry = Address::from([0x51; 20]);
    let victim = Address::from([0x52; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
    db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    // Deliberately never call start_capture.
    let result = evm.transact(call_tx(entry)).expect("tx executes");
    assert!(result.result.is_success());

    let inspector = evm.components_mut().1;
    assert_eq!(inspector.selfdestruct_journal_mark, None);
    assert!(!inspector.selfdestruct_wrote_journal);

    let (hits, active_ran, journal_ran) = SELFDESTRUCT_GATE_SNAPSHOT.with(|s| s.get());
    assert!(hits > 0, "the step hook must still be reached for the SELFDESTRUCT opcode");
    assert!(!active_ran, "the active branch must not run while inactive");
    assert!(!journal_ran, "the watermark journal read must never execute while inactive");
}

#[test]
fn inactive_inspector_selfdestruct_normal_transfer_leaves_new_state_untouched() {
    // The to-self shape above writes NO journal entry, so on field values alone it would
    // pass equally against an implementation whose step/step_end gates were removed (an
    // ungated step/step_end would measure a zero delta either way). This is the
    // discriminating shape: a normal transfer (existing contract, beneficiary != victim,
    // balance > 0) DOES write a journal entry, so an ungated step/step_end would leave
    // `selfdestruct_wrote_journal == true` here -- which is exactly what this test rules
    // out. This pins the specific regression where step/step_end lose their inactive gate
    // while the selfdestruct hook's own gate is retained; it is not a claim that any gate
    // failure in general leaves the flag true.
    reset_selfdestruct_gate_seams();
    let victim = Address::from([0x81; 20]);
    let beneficiary = Address::from([0x82; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(victim, account(500, Some(selfdestruct_to(beneficiary))));

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    // Deliberately never call start_capture.
    let result = evm.transact(call_tx(victim)).expect("tx executes");
    assert!(result.result.is_success());

    let inspector = evm.components_mut().1;
    // Read BEFORE any start_capture/finish_capture, so a reset cannot stand in for the gate.
    assert_eq!(inspector.selfdestruct_journal_mark, None);
    assert!(!inspector.selfdestruct_wrote_journal);

    let (hits, active_ran, journal_ran) = SELFDESTRUCT_GATE_SNAPSHOT.with(|s| s.get());
    assert!(hits > 0, "the step hook must still be reached for the SELFDESTRUCT opcode");
    assert!(!active_ran, "the active branch must not run while inactive");
    assert!(!journal_ran, "the watermark journal read must never execute while inactive");
}

#[test]
fn inactive_then_armed_inspector_still_captures() {
    // The SAME inspector instance that just ran inactive must still capture correctly
    // once armed — proves the inactive gate does not poison a later window.
    let entry = Address::from([0x51; 20]);
    let victim = Address::from([0x52; 20]);
    let second_caller = Address::from([0xBB; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(second_caller, account(1_000_000_000, None));
    db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
    db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    // First: run inactive (never start_capture).
    let result = evm.transact(call_tx_from(caller(), entry)).expect("tx executes");
    assert!(result.result.is_success());

    // Now arm the SAME inspector instance and run a second, independent tx from a different
    // sender (so nonce 0 is valid for both, per `call_tx_from`'s doc comment).
    evm.components_mut().1.start_capture();
    let result2 = evm.transact(call_tx_from(second_caller, entry)).expect("tx executes");
    assert!(result2.result.is_success());
    let outcome = evm.components_mut().1.finish_capture(result2.result.logs());
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "exactly one virtual log for the value-bearing CALL");
            assert_eq!(stream[0].topics()[1], entry.into_word());
            assert_eq!(stream[0].topics()[2], victim.into_word());
        }
        other => panic!("expected Complete with one transfer, got {other:?}"),
    }
}

#[test]
fn selfdestruct_oog_after_journal_write_leaks_flag_but_not_across_windows() {
    // The real abnormal path, not a hand-set simulation: a value-bearing SELFDESTRUCT writes
    // its journal entry (JournalInner::selfdestruct's BalanceTransfer/AccountTouched branch)
    // BEFORE the dynamic gas charge for a new-account + cold-access beneficiary runs. If that
    // charge fails, the opcode halts with OutOfGas; step_end still measures the (already
    // grown) journal and sets the flag, but the selfdestruct callback is dispatched only for
    // InstructionResult::SelfDestruct, so it never fires to consume it.
    //
    // Measured (not computed from the gas table): the victim's beneficiary is NOT
    // pre-inserted (cold, non-existent, so the dynamic charge includes the new-account cost)
    // and the victim is called with a fixed 10_000-gas stipend. The victim frame's actual gas
    // at CALL entry is 10_000 + 2_300 (the value-bearing CALL's own stipend) = 12_300, which
    // sits inside the measured window where the opcode's static dispatch succeeds (so the
    // journal write runs) but the dynamic charge does not (so it halts after writing).
    const STIPEND: u16 = 10_000;

    let third = Address::from([0x95; 20]);
    let victim = Address::from([0x96; 20]);
    let beneficiary = Address::from([0x97; 20]);
    let entry = Address::from([0x98; 20]);
    let entry2 = Address::from([0x99; 20]);
    let dst2 = Address::from([0x9A; 20]);
    let second_caller = Address::from([0xBC; 20]);

    // The GREEN control's fixture (same shape as `entry`/`victim`, differing only in the
    // stipend) is pre-inserted here, alongside the RED fixture, because `db` is moved into
    // `evm` below and the GREEN half runs on the same inspector after the next-window check.
    const STIPEND_OK: u16 = 40_000;
    let third_green = Address::from([0x9B; 20]);
    let victim_green = Address::from([0x9C; 20]);
    let beneficiary_green = Address::from([0x9D; 20]);
    let entry_green = Address::from([0x9E; 20]);
    let third_caller = Address::from([0xBD; 20]);

    reset_selfdestruct_gate_seams();
    assert!(
        SELFDESTRUCT_STEP_OBSERVATIONS.with(|o| o.borrow().is_empty()),
        "the observation seam must start empty; cargo test reuses threads across tests"
    );
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(second_caller, account(1_000_000_000, None));
    db.insert_account_info(third_caller, account(1_000_000_000, None));
    db.insert_account_info(victim, account(500, Some(selfdestruct_to(beneficiary))));
    // entry: a separate, successful ordinary transfer to `third` first (full gas), THEN the
    // fixed-gas call into `victim` whose frame will run out of gas on the dynamic charge. The
    // out-of-gas victim frame reverts its OWN incoming value transfer along with everything
    // else in that frame, so the surviving stream is the `third` transfer, not an empty one.
    let mut entry_code = call_prefix(0x01, third);
    entry_code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
    entry_code.extend_from_slice(&call_fixed_gas_value_1_to(STIPEND, victim));
    db.insert_account_info(entry, account(10, Some(entry_code)));
    db.insert_account_info(entry2, account(10, Some(call_value_1_to(dst2))));
    db.insert_account_info(victim_green, account(500, Some(selfdestruct_to(beneficiary_green))));
    let mut entry_green_code = call_prefix(0x01, third_green);
    entry_green_code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
    entry_green_code.extend_from_slice(&call_fixed_gas_value_1_to(STIPEND_OK, victim_green));
    db.insert_account_info(entry_green, account(10, Some(entry_green_code)));

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(call_tx_from(caller(), entry)).expect("tx executes");
    assert!(result.result.is_success(), "the top-level tx succeeds; only the inner CALL fails");

    // 1. the opcode was reached and measured in an ARMED window.
    let (hits, active_ran, journal_ran) = SELFDESTRUCT_GATE_SNAPSHOT.with(|s| s.get());
    assert!(hits > 0, "the step hook must be reached for the SELFDESTRUCT opcode");
    assert!(active_ran, "the window must be armed");
    assert!(journal_ran, "the watermark must have read the journal for THIS opcode");

    // 2. the callback never fired -- proven DIRECTLY via a dedicated entry counter, not via
    // the absence of a SelfDestruct-kind log (a revert can remove logs on its own). The
    // callback fires if and only if the opcode's result is InstructionResult::SelfDestruct,
    // so hits == 0 here, together with journal_ran/flag below, is direct evidence the result
    // was something else (the measured OutOfGas halt).
    assert_eq!(
        SELFDESTRUCT_CALLBACK_HITS.with(|c| c.get()),
        0,
        "the selfdestruct callback must never have been entered"
    );

    // 2a. THE ITEM: the victim SELFDESTRUCT's ACTUAL result, observed at that opcode's own
    // step_end and bound to that opcode and frame -- not inferred from callback_hits.
    let red = SELFDESTRUCT_STEP_OBSERVATIONS.with(|o| o.borrow().clone());
    assert_eq!(red.len(), 1, "exactly one observation in this measured run");
    let red = red[0];
    assert_eq!(red.opcode, opcode::SELFDESTRUCT, "the record is bound to the SELFDESTRUCT opcode");
    let red_end = red.end.expect("step_end completed the record for this opcode");
    assert_eq!(
        red_end.instruction_result,
        Some(InstructionResult::OutOfGas),
        "THE OBSERVATION: the victim SELFDESTRUCT halted with OutOfGas"
    );
    assert!(red_end.journal_delta >= 1, "the journal write preceded the failed charge");

    // 3. the measured leak, read LIVE before the window ends. A read taken after
    // finish_capture would always see `false` (every window boundary resets it) and would
    // silently hide the leak -- which is exactly why this must be read here, not later.
    let inspector = evm.components_mut().1;
    assert_eq!(inspector.selfdestruct_journal_mark, None, "step_end took the mark");
    assert!(inspector.selfdestruct_wrote_journal, "THE LEAK: the flag is set with no consumer");

    let outcome = inspector.finish_capture(result.result.logs());
    match &outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "only the separate successful transfer survives");
            assert_eq!(stream[0].topics()[1], entry.into_word());
            assert_eq!(stream[0].topics()[2], third.into_word());
        }
        other => panic!("expected Complete with the one surviving transfer, got {other:?}"),
    }
    // Immediately after finish: the flag is cleared. This documents WHY assertion 3 above had
    // to be read live.
    assert!(!evm.components_mut().1.selfdestruct_wrote_journal);

    // 4. the next window on the SAME inspector is clean and correct.
    evm.components_mut().1.start_capture();
    assert!(
        !evm.components_mut().1.selfdestruct_wrote_journal,
        "the next window must not inherit the leaked flag"
    );
    let result2 = evm.transact(call_tx_from(second_caller, entry2)).expect("tx executes");
    assert!(result2.result.is_success());
    match evm.components_mut().1.finish_capture(result2.result.logs()) {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "the next window's ordinary transfer is observed once");
            assert_eq!(stream[0].topics()[1], entry2.into_word());
            assert_eq!(stream[0].topics()[2], dst2.into_word());
        }
        other => {
            panic!("expected Complete with one transfer in the next window, got {other:?}")
        }
    }

    // The GREEN control: same fixture, same initial state and coldness, differing ONLY in the
    // stipend. It brackets the charge that failed in the RED half and proves RED is not vacuous.
    // A full reset between halves keeps each half independently falsifiable: a regression in
    // one half can't be masked by state left over from the other. The fixture's accounts were
    // pre-inserted into `db` above, alongside the RED fixture, because `db` was moved into
    // `evm` before this half runs.
    reset_selfdestruct_gate_seams();
    evm.components_mut().1.start_capture();
    let result3 = evm.transact(call_tx_from(third_caller, entry_green)).expect("tx executes");
    assert!(result3.result.is_success());

    let green = SELFDESTRUCT_STEP_OBSERVATIONS.with(|o| o.borrow().clone());
    assert_eq!(green.len(), 1, "exactly one observation in this measured run");
    let green = green[0];
    assert_eq!(green.opcode, opcode::SELFDESTRUCT);
    let green_end = green.end.expect("step_end completed the record for this opcode");
    assert_eq!(
        green_end.instruction_result,
        Some(InstructionResult::SelfDestruct),
        "the GREEN control reaches the only path that returns SelfDestruct"
    );
    assert_eq!(
        SELFDESTRUCT_CALLBACK_HITS.with(|c| c.get()),
        1,
        "the callback fires exactly once when the result IS SelfDestruct"
    );
    assert!(
        !evm.components_mut().1.selfdestruct_wrote_journal,
        "the callback consumed the flag via mem::take"
    );

    // The record belongs to the victim frame: identical call shape => identical victim depth.
    // Asserted against the measured control, never against a hard-coded literal that was
    // never actually measured on this stack.
    assert_eq!(red_end.depth, green_end.depth, "both records are at the victim frame's depth");

    // A passing assertion prints nothing, so the records are printed explicitly here to make
    // them visible when the suite is run with --nocapture / --show-output.
    println!("T14b RED  observation: {red:?}");
    println!("T14b GREEN observation: {green:?}");

    match evm.components_mut().1.finish_capture(result3.result.logs()) {
        CaptureOutcome::Complete { logs: stream, .. } => {
            // Unlike RED (where the victim frame's OutOfGas reverts its own incoming
            // transfer), GREEN's victim CALL now succeeds, so its value-1 transfer survives
            // alongside the entry's separate transfer to `third_green` and the SELFDESTRUCT
            // transfer itself -- three distinct native transfers, measured, not assumed.
            assert_eq!(stream.len(), 3, "all three native transfers in the GREEN run survive");
            assert_eq!(stream[0].topics()[1], entry_green.into_word());
            assert_eq!(stream[0].topics()[2], third_green.into_word());
            assert_eq!(stream[1].topics()[1], entry_green.into_word());
            assert_eq!(stream[1].topics()[2], victim_green.into_word());
            assert_eq!(stream[2].topics()[1], victim_green.into_word());
            assert_eq!(stream[2].topics()[2], beneficiary_green.into_word());
        }
        other => panic!("expected Complete with the three surviving transfers, got {other:?}"),
    }
}

/// Runtime bytecode: `CREATE(value, 0, len)` where the appended init code (copied into memory
/// via `CODECOPY`) deploys a contract whose runtime body is [`address_then_selfdestruct`]
/// (`PUSH2 0x30FF; PUSH1 0; MSTORE; PUSH1 2; PUSH1 30; RETURN`), then `CALL`s the precomputed
/// deployment address with zero value to run that child's SELFDESTRUCT within the SAME
/// transaction that created it.
fn create_endowment_then_call_child(value: u8, child_addr: Address) -> Vec<u8> {
    let init_code: Vec<u8> = vec![0x61, 0x30, 0xff, 0x60, 0x00, 0x52, 0x60, 0x02, 0x60, 0x1e, 0xf3];
    let init_len = init_code.len() as u8;
    let mut head = vec![
        0x60, init_len, // PUSH1 init_len   (CODECOPY size)
        0x60, 0x00, // PUSH1 <codeoffset> -- patched below (CODECOPY offset)
        0x60, 0x00, // PUSH1 0          (CODECOPY destOffset)
        0x39, // CODECOPY
        0x60, init_len, // PUSH1 init_len   (CREATE size)
        0x60, 0x00, // PUSH1 0          (CREATE offset)
        0x60, value, // PUSH1 value      (CREATE value)
        0xf0,  // CREATE
        0x50,  // POP (discard the dynamic created address; we call a precomputed one)
    ];
    head.extend_from_slice(&call_prefix(0x00, child_addr)); // CALL(gas, child_addr, 0, 0,0,0,0)
    head.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
    let codeoffset = head.len() as u8;
    head[3] = codeoffset; // patch the PUSH1 <codeoffset> immediate
    head.extend_from_slice(&init_code);
    head
}

#[test]
fn created_in_tx_selfdestruct_to_self_is_still_filtered_not_double_suppressed() {
    // A SELFDESTRUCT-to-self by a contract CREATED in this same transaction writes a REAL
    // AccountDestroyed journal entry (the flag is true), and the observation is dropped by
    // the pre-existing from==to filter -- not by the new guard.
    // Pins that the new guard did not become a second, over-broad suppression mechanism.
    let factory = Address::from([0x75; 20]);
    let value = 7u8;
    let child = factory.create(0);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(
        factory,
        account(10, Some(create_endowment_then_call_child(value, child))),
    );

    let (outcome, success) = run_capturing_tx(db, factory, 16);
    assert!(success, "top-level tx should succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(
                stream.len(),
                1,
                "only the CREATE endowment transfer; the created-in-tx self-destruct \
                     contributes nothing"
            );
            assert_eq!(stream[0].topics()[1], factory.into_word());
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(value as u64).to_be_bytes::<32>());
        }
        other => {
            panic!("expected Complete with exactly the CREATE endowment transfer, got {other:?}")
        }
    }
}
