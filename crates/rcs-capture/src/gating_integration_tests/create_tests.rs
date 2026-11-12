//! CREATE / CREATE2 endowment capture: revocation, nonce overflow, address-derivation seams and
//! inactive-mode invariants.

use super::*;
use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Signature, TxKind};
use op_revm::OpSpecId;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;
use revm::state::{AccountInfo, Bytecode};

// ── helpers for CREATE / DELEGATECALL tests ───────────────────────────────

fn create_tx_with_value(from: Address, value: U256) -> OpTx {
    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Create,
        value,
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    OpTx::from_recovered_tx(&tx, from)
}

/// Runs a top-level CREATE tx (no bytecode body — empty init-code) through the real
/// composite-wrapped OpEvm and returns the capture outcome and success flag.
fn run_capturing_create_tx(
    db: InMemoryDB,
    from: Address,
    value: U256,
    budget: usize,
) -> (CaptureOutcome, bool) {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(budget),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(create_tx_with_value(from, value)).expect("tx executes");
    let success = result.result.is_success();
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());
    (outcome, success)
}

// ── T1b: inactive inspector — CREATE tx ──────────────────────────────────

#[test]
fn inactive_create_tx_mutates_no_capture_state() {
    // A CREATE tx runs through the production EVM path without ever calling start_capture.
    // The inspector must leave create_recipients, checkpoints, and queue empty throughout.
    let deployer = Address::from([0xDE; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(deployer, account(1_000_000_000, None));

    let (outcome, _) = run_capturing_create_tx(db, deployer, U256::ZERO, 10);
    assert!(
        matches!(outcome, CaptureOutcome::Complete { logs: ref s, .. } if s.is_empty()),
        "inactive inspector must produce an empty Complete on a CREATE tx"
    );
}

// ── T1c: D-ISO — inactive inspector on value-bearing CREATE tx ───────────

#[test]
fn inactive_inspector_diso_create_tx_no_mutation() {
    // When start_capture() is never called, the create hook's inactive-gate returns
    // immediately without pushing to create_recipients. This is the D-ISO invariant
    // for the CREATE hook: a value-bearing CREATE must not mutate capture-private state.
    let deployer = Address::from([0xDF; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(deployer, account(1_000_000_000, None));
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    // Deliberately do NOT call start_capture() — inspector stays inactive.
    evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");
    let inspector = evm.components_mut().1;
    assert!(
        inspector.create_recipients.is_empty(),
        "inactive create hook must not push to create_recipients"
    );
    assert!(inspector.checkpoints.is_empty(), "inactive push_checkpoint must not add entries");
}

// ── T1d: create_recipients balanced push/pop ─────────────────────────────

#[test]
fn create_recipients_push_pop_balanced() {
    // A CREATE followed by a reverted inner CALL means:
    //   create hook  → push_checkpoint + create_recipients.push(Some(addr))
    //   create_end (success) → pop(false) does NOT revert; create_recipients.pop()
    // After finish_capture the create_recipients vec must be drained to zero.
    let factory = Address::from([0xF2; 20]);
    let expected_created = factory.create2(B256::ZERO, keccak256(b""));
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));

    // After the tx the inspector has been reset by finish_capture.
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(call_tx(factory)).expect("tx executes");
    let logs = result.result.logs();
    let _outcome = evm.components_mut().1.finish_capture(logs);

    let inspector = evm.components_mut().1;
    assert!(
        inspector.create_recipients.is_empty(),
        "create_recipients must be empty after finish_capture; addr={expected_created:?}"
    );
    assert!(inspector.checkpoints.is_empty(), "checkpoints must be empty after finish_capture");
}

// ── T4a: CREATE2 EIP-684 collision revokes endowment ─────────────────────

#[test]
fn internal_create2_eip684_collision_endowment_revoked() {
    // When a CREATE2 target already has nonce > 0 (EIP-684 collision), the deployment
    // fails: outcome.result.is_ok() == false. The `create_end` hook must detect this and
    // call pop_checkpoint(true), rolling back the 7-wei endowment from the queue.
    let factory = Address::from([0xF3; 20]);
    let collision_target = factory.create2(B256::ZERO, keccak256(b""));
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));
    // Pre-populate the CREATE2 target with nonce=1 to trigger EIP-684 collision.
    db.insert_account_info(collision_target, AccountInfo { nonce: 1, ..Default::default() });

    let (outcome, success) = run_capturing_tx(db, factory, 16);
    assert!(success, "top-level CALL to factory must succeed even if inner CREATE2 fails");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => assert!(
            stream.is_empty(),
            "EIP-684 collision must revoke the CREATE2 endowment; stream must be empty"
        ),
        other => panic!("expected Complete(empty) after CREATE2 collision, got {other:?}"),
    }
}

// ── T4b: revoked CREATE still consumes observed_count budget ─────────────

#[test]
fn revoked_create_still_consumes_native_budget() {
    // observed_count increments in observe_native_transfer regardless of later revocation.
    // With budget=1, after a CREATE2 collision (revoked), observed_count == 1 before
    // finish_capture; after finish_capture it resets to 0 and the stream is empty.
    let factory = Address::from([0xF4; 20]);
    let collision_target = factory.create2(B256::ZERO, keccak256(b""));
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));
    db.insert_account_info(collision_target, AccountInfo { nonce: 1, ..Default::default() });

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(1),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(call_tx(factory)).expect("tx executes");
    // After transact, before finish_capture: observed_count is sticky (== 1).
    assert_eq!(
        evm.components_mut().1.observed_count,
        1,
        "observed_count must be 1 after revoked CREATE2 (sticky counter, not rolled back)"
    );
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert!(stream.is_empty(), "revoked endowment must not appear in the final stream")
        }
        other => panic!("expected Complete(empty), got {other:?}"),
    }
}

/// Runtime bytecode: CALL(gas, call_dst, 1 wei, 0, 0, 0, 0); POP; CREATE2(7 wei, salt=0, empty)
fn call_value_1_then_create2_endowment(call_dst: Address) -> Vec<u8> {
    let mut code = call_prefix(0x01, call_dst);
    // GAS CALL POP
    code.extend_from_slice(&[0x5a, 0xf1, 0x50]);
    // PUSH1 0 (salt) PUSH1 0 (size) PUSH1 0 (offset) PUSH1 7 (value) CREATE2
    code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x07, 0xf5]);
    code
}

// ── T4b-ext: budget=N, then N+1 colliding CREATE → LimitExceeded ──────────────────────────

#[test]
fn revoked_create_with_prior_transfer_exceeds_budget_limit() {
    // Proves that observed_count from a revoked CREATE is real budget consumption:
    // with limit=1, one CALL (fills the budget) plus a revoked CREATE2 collision
    // (one more observation) must produce LimitExceeded{attempted:2, limit:1}.
    let factory = Address::from([0xF5; 20]);
    let call_recipient = Address::from([0xF6; 20]);
    let collision_target = factory.create2(B256::ZERO, keccak256(b""));
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(
        factory,
        account(1_000, Some(call_value_1_then_create2_endowment(call_recipient))),
    );
    db.insert_account_info(call_recipient, account(0, None));
    db.insert_account_info(collision_target, AccountInfo { nonce: 1, ..Default::default() });

    let (outcome, success) = run_capturing_tx(db, factory, 1);
    assert!(success, "top-level CALL to factory must succeed even on budget overflow");
    assert!(
        matches!(
            outcome,
            CaptureOutcome::LimitExceeded {
                attempted: 2,
                limit: 1,
                budget: ObservationBudget::NativeTransfer,
            }
        ),
        "one CALL + one revoked CREATE2 must exceed budget=1; got {outcome:?}"
    );
}

// ── top_level_create_with_value_emits_transfer ───────────────────────────

#[test]
fn top_level_create_with_value_emits_transfer() {
    // A top-level CREATE tx with non-zero value sends the endowment from the deployer
    // to the derived contract address. The inspector must capture that as one Transfer.
    let deployer = Address::from([0xDE; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(deployer, account(1_000_000_000, None));

    let (outcome, _) = run_capturing_create_tx(db, deployer, U256::from(5u64), 10);
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 1, "one Transfer expected for top-level CREATE endowment");
            assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
            assert_eq!(
                stream[0].topics()[1],
                deployer.into_word(),
                "transfer.from must be the deployer"
            );
            assert_eq!(
                stream[0].topics()[2],
                deployer.create(0).into_word(),
                "transfer.to must be the contract address derived from deployer + nonce 0"
            );
        }
        other => panic!("expected Complete with one Transfer, got {other:?}"),
    }
}

// ── D7b: successful-CREATE address mismatch fails closed (InvariantViolation) ────────────────
#[test]
fn create_address_mismatch_through_real_create_end_fails_closed() {
    // D7b: a successful CREATE whose actual deployed address differs from the pre-derived
    // expected address stored in `create_recipients` must produce
    // `InvariantViolation { CreateAddressMismatch }`, NEVER `Complete`. The
    // `FORCE_CREATE_ADDRESS_MISMATCH` seam pushes `Address::repeat_byte(0xFE)` as the expected
    // recipient in the real `create` hook, so `create_end` sees a genuine `e != a` on an
    // actually-successful, value-bearing CREATE (is_ok=true, address=Some(real)).
    // Falsifiable: deleting the D7b branch in `create_end` causes this test to fail
    // (`Complete(empty)` instead of `InvariantViolation`); deleting the variant causes a
    // compile error in this `matches!`.
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    FORCE_CREATE_ADDRESS_MISMATCH.with(|f| f.set(true));
    let (outcome, success) = run_capturing_create_tx(db, caller(), U256::from(1), 16);
    assert!(success, "the CREATE must succeed (is_ok=true) for D7b to be reachable");
    assert!(
        matches!(
            outcome,
            CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::CreateAddressMismatch { .. }
            }
        ),
        "successful CREATE with mismatched pre-derived/actual address must fail closed with \
             CreateAddressMismatch, got {outcome:?}"
    );
}

// ── Regression: D7a nonce-overflow endowment revocation (is_ok=true, address=None) ──────────

#[test]
fn create_is_ok_early_return_no_endowment_leak() {
    // D7a: when a factory account's nonce = u64::MAX the internal CREATE returns
    // CreateOutcome { address: None, is_ok: true } (revm nonce-overflow path). The
    // pre-pushed endowment event must be revoked even though is_ok is true.
    // The top-level CALL endowment (value=5) is the only Transfer that must survive.
    let factory = Address::from([0xFA; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(
        factory,
        AccountInfo {
            balance: U256::from(1_000u64),
            nonce: u64::MAX,
            code: Some(Bytecode::new_raw(Bytes::from(create_endowment_7()))),
            ..Default::default()
        },
    );

    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Call(factory),
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
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());

    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(
                stream.len(),
                1,
                "only the top-level transfer survives; nonce-overflow endowment must be revoked"
            );
            assert_eq!(stream[0].topics()[1], caller().into_word(), "from = caller");
            assert_eq!(stream[0].topics()[2], factory.into_word(), "to = factory");
            assert_eq!(stream[0].data.data.as_ref(), &U256::from(5u64).to_be_bytes::<32>());
        }
        other => panic!("expected Complete with one transfer, got {other:?}"),
    }
}

// ── Regression: early CREATE failure produces no event or address leak ────────────────────────

#[test]
fn create_early_failure_no_address_or_event_leak() {
    // A factory issues CREATE with value=7 but has zero balance; the CREATE fails
    // (insufficient balance). The pre-pushed endowment event must not survive.
    let factory = Address::from([0xFB; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(factory, account(0, Some(create_endowment_7())));

    let (outcome, success) = run_capturing_tx(db, factory, 16);
    assert!(success, "top-level zero-value tx should succeed even if inner CREATE fails");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert!(
                stream.is_empty(),
                "failed CREATE with zero-value top-level must leave an empty stream"
            );
        }
        other => panic!("expected Complete(empty), got {other:?}"),
    }
}

// ── True nonce-overflow: is_ok=true, address=None (distinct from EIP-684 collision) ──────────

#[test]
fn create_nonce_overflow_is_ok_true_address_none_revoked() {
    // When a factory's nonce = u64::MAX, a CREATE with endowment returns
    // is_ok=true, address=None (nonce-overflow, not EIP-684 collision where is_ok=false).
    // The D7a path revokes the endowment; with a zero-value outer call, the stream is empty.
    let factory = Address::from([0xF6; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(
        factory,
        AccountInfo {
            balance: U256::from(1_000u64),
            nonce: u64::MAX,
            code: Some(Bytecode::new_raw(Bytes::from(create_endowment_7()))),
            ..Default::default()
        },
    );

    let (outcome, success) = run_capturing_tx(db, factory, 16);
    assert!(success, "outer zero-value call to factory must succeed");
    // Completion validity is asserted on the REAL `CreateOutcome` the EVM handed `create_end`,
    // not inferred from the flattened stream: the nonce-overflow completion is
    // `is_ok() == true` with `address == None`. The sticky observation count is also read at
    // that same instant — the revocation above must NOT have decremented it, so it still
    // carries the one phantom CREATE observation.
    let (is_ok, address_is_none, observed_at_create_end, address) =
        CREATE_END_OUTCOME.with(|s| s.get());
    assert!(is_ok, "nonce-overflow CREATE completion is is_ok() == true");
    assert!(address_is_none, "nonce-overflow CREATE completion carries address == None");
    assert!(address.is_none(), "the recorded completion address must be None");
    assert_eq!(
        observed_at_create_end, 1,
        "the revoked CREATE observation must stay counted (sticky observed_count)"
    );
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert!(
                stream.is_empty(),
                "nonce-overflow CREATE endowment must be revoked; stream must be empty"
            );
        }
        other => panic!("expected Complete(empty) for nonce-overflow CREATE, got {other:?}"),
    }
}

#[test]
fn create2_nonce_overflow_is_ok_true_address_none_revoked() {
    // When a factory's nonce = u64::MAX, a CREATE2 with endowment returns
    // is_ok=true, address=None (nonce-overflow, not EIP-684 collision where is_ok=false).
    // The D7a path revokes the endowment; with a zero-value outer call, the stream is empty.
    let factory = Address::from([0xF7; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(
        factory,
        AccountInfo {
            balance: U256::from(1_000u64),
            nonce: u64::MAX,
            code: Some(Bytecode::new_raw(Bytes::from(create2_endowment_7()))),
            ..Default::default()
        },
    );

    let (outcome, success) = run_capturing_tx(db, factory, 16);
    assert!(success, "outer zero-value call to factory must succeed");
    // Same real-completion assertions as the CREATE case, on the CREATE2 path: `is_ok() == true`
    // with `address == None` (nonce overflow, NOT the contract-creation collision case, which
    // reports `is_ok() == false`), and a sticky observation count that survived the revocation.
    let (is_ok, address_is_none, observed_at_create_end, address) =
        CREATE_END_OUTCOME.with(|s| s.get());
    assert!(is_ok, "nonce-overflow CREATE2 completion is is_ok() == true");
    assert!(address_is_none, "nonce-overflow CREATE2 completion carries address == None");
    assert!(address.is_none(), "the recorded completion address must be None");
    assert_eq!(
        observed_at_create_end, 1,
        "the revoked CREATE2 observation must stay counted (sticky observed_count)"
    );
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert!(
                stream.is_empty(),
                "nonce-overflow CREATE2 endowment must be revoked; stream must be empty"
            );
        }
        other => panic!("expected Complete(empty) for nonce-overflow CREATE2, got {other:?}"),
    }
}

// ── T1 proof-boundary: inactive gate at create-return ────────────────────────────────────────

#[test]
fn inactive_create_no_mutation_and_no_derivation_at_create_return() {
    // Inspector is never activated. The `create` hook must push_checkpoint then immediately
    // gate-return without touching `create_recipients` or running address derivation.
    // The CREATE_GATE_SNAPSHOT seam records the state at the exact return point so we can
    // assert cleanliness BEFORE `create_end` fires.
    reset_create_gate_seams();
    let deployer = Address::from([0xDA; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(deployer, account(1_000_000_000, None));
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    // Deliberately do NOT call start_capture().
    evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");
    let inspector = evm.components_mut().1;
    assert!(!inspector.active);
    assert!(inspector.create_recipients.is_empty());
    assert!(inspector.checkpoints.is_empty());
    let (cr_len, cp_len, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
    assert_eq!(cr_len, 0, "create_recipients must be untouched at create hook return");
    assert_eq!(cp_len, 0, "checkpoints must be untouched at create hook return");
    assert!(!capture_body_ran, "the capture body must not run while inactive");
    assert!(!derivation_ran, "address derivation must not run while inactive");
}

// ── CREATE address derivation: a computed value, distinguishable from "never ran" ────────────

#[test]
fn derivation_ran_true_when_journal_read_for_address_derivation() {
    // An armed inspector running a real CREATE-scheme deployment with a non-zero endowment: the
    // `create` hook must report that the created-address derivation actually ran — i.e. that the
    // journal read supplying the deployer nonce executed. The assertion is on the value the hook
    // COMPUTED, so replacing the computation with a literal `false` fails this test.
    reset_create_gate_seams();
    let deployer = Address::from([0xDC; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(deployer, account(1_000_000_000, None));
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    evm.components_mut().1.start_capture();
    let _ = evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");

    let (_, _, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
    assert!(capture_body_ran, "the capture body must have run for an armed inspector");
    assert!(
        derivation_ran,
        "the CREATE-scheme created-address derivation reads the caller nonce from the journal; \
             the hook must report that the derivation ran"
    );
}

#[test]
fn derivation_ran_false_when_create_hook_not_hit() {
    // The not-hit sentinel: an inspector that was never armed leaves BOTH the capture-body flag
    // and the derivation flag clear, so `derivation_ran == false` here cannot be confused with
    // the "ran, but no journal read was needed" state (which reports capture_body_ran == true).
    reset_create_gate_seams();
    let deployer = Address::from([0xDD; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(deployer, account(1_000_000_000, None));
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    // Deliberately do NOT call start_capture().
    let _ = evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");

    let (_, _, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
    assert!(!capture_body_ran, "the capture body must not run while inactive");
    assert!(!derivation_ran, "the not-hit sentinel must report that no derivation ran");
}

// ── distinguish "create hook never invoked" from "invoked while inactive" ──────────────────

#[test]
fn non_create_control_leaves_create_hook_hits_zero() {
    // A real transaction with NO `CREATE` at any depth (pure `CALL` only) must leave the
    // create-hook hit counter at zero, which is what makes `derivation_ran == false` in
    // `derivation_ran_false_when_create_hook_not_hit` a statement about a hook that DID run
    // (because the create hook was never entered) rather than one that never fired.
    reset_create_gate_seams();
    let contract = Address::from([0xC3; 20]);
    let dst = Address::from([0xD4; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(contract, account(1_000, Some(call_value_1_to(dst))));

    let (outcome, success) = run_capturing_tx(db, contract, 16);
    assert!(success, "pure-CALL tx must succeed");
    assert_eq!(
        CREATE_HOOK_HITS.with(|c| c.get()),
        0,
        "a transaction with no CREATE at any depth must never invoke the create hook"
    );
    assert!(
        matches!(outcome, CaptureOutcome::Complete { logs: ref s, .. } if s.len() == 1),
        "the internal CALL transfer really ran: {outcome:?}"
    );
}

#[test]
fn inactive_create_hook_hit_is_nonzero_with_derivation_false() {
    // The other side of the control above: an UNARMED inspector that really does execute a
    // CREATE records a hook hit while reporting neither a capture body nor a derivation —
    // proving `derivation_ran_false_when_create_hook_not_hit`'s `false` comes from a hook that
    // DID run, not one that never fired.
    reset_create_gate_seams();
    let deployer = Address::from([0xD6; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(deployer, account(1_000_000_000, None));
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    // Deliberately do NOT call start_capture().
    evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");

    assert!(
        CREATE_HOOK_HITS.with(|c| c.get()) > 0,
        "the create hook must have been invoked for a real CREATE, even while unarmed"
    );
    let (_, _, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
    assert!(!capture_body_ran, "the capture body must not run while inactive");
    assert!(!derivation_ran, "no address derivation runs while inactive");
}

#[test]
fn derivation_ran_false_but_capture_body_ran_for_create2() {
    // The third state, and the reason the sentinel is not a bare bool: CREATE2 derives its
    // address from salt + init-code hash and reads no journal state, so the capture body runs
    // while the derivation flag stays clear. A reader can therefore always distinguish
    // "hook never ran" (both false) from "ran, no journal read needed" (body true, derive false)
    // from "derived" (both true).
    reset_create_gate_seams();
    let factory = Address::from([0xDE; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));

    let (outcome, success) = run_capturing_tx(db, factory, 16);
    assert!(success, "CREATE2 endowment tx must succeed");
    assert!(
        matches!(outcome, CaptureOutcome::Complete { .. }),
        "CREATE2 endowment must complete, got {outcome:?}"
    );

    let (_, _, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
    assert!(capture_body_ran, "the capture body must have run for an armed inspector");
    assert!(
        !derivation_ran,
        "CREATE2 needs no journal read for the created address, so the derivation flag is clear"
    );
    // The hook was genuinely hit, so this `false` can never be misread as
    // "the hook never ran" — only as "ran, no journal read needed".
    assert!(
        CREATE_HOOK_HITS.with(|c| c.get()) > 0,
        "the create hook must have been invoked for a real CREATE2 endowment"
    );
}

// ── T1 proof-boundary: create_recipients balanced inside create_end ──────────────────────────

#[test]
fn create_recipients_balanced_before_finish_capture() {
    // Each `create` hook push must be balanced by `create_end`'s pop, not deferred to
    // `finish_capture`. The CREATE_END_BALANCE seam captures the lengths immediately after
    // the last pop inside `create_end` — before the test calls `finish_capture` — so the
    // assertion is on the boundary that matters: within the EVM frame, not at cleanup.
    let deployer = Address::from([0xDB; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(deployer, account(1_000_000_000, None));
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(16),
    );
    evm.components_mut().1.start_capture();
    let result =
        evm.transact(create_tx_with_value(deployer, U256::from(7u64))).expect("tx executes");
    // Read seam BEFORE finish_capture — proves push/pop balanced in create_end itself.
    let (cr_len, cp_len) = CREATE_END_BALANCE.with(|s| s.get());
    assert_eq!(cr_len, 0, "create_recipients must be empty after create_end — push/pop balanced");
    assert_eq!(cp_len, 0, "checkpoints must be empty after create_end — push/pop balanced");
    let logs = result.result.logs();
    let _outcome = evm.components_mut().1.finish_capture(logs);
}
