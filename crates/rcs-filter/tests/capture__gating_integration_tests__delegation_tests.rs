//! Delegated code attribution: transfers made inside a DELEGATECALL frame belong to the
//! delegator, following the executed code rather than the stored one.

use super::*;
use alloy_consensus::{SignableTransaction, TxEip7702};
use alloy_eips::eip7702::Authorization;
use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::Signature;
use op_revm::OpSpecId;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;

/// Runtime bytecode intended to run inside a DELEGATECALL context.
/// When invoked, this code:
///   1. CALL `call_dst` with 1 wei
///   2. CREATE with 1 wei and empty init code
///   3. STOP
///
/// Both operations attribute to the delegating address (execution context).
fn delegated_call_and_create(call_dst: Address) -> Vec<u8> {
    // CALL: retSize=0, retOffset=0, argsSize=0, argsOffset=0, value=1, addr=call_dst, gas
    let mut code = call_prefix(1, call_dst);
    // GAS; CALL; POP
    code.extend_from_slice(&[0x5A, 0xF1, 0x50]);
    // CREATE(value=1, offset=0, size=0): PUSH1 0 (size) PUSH1 0 (offset) PUSH1 1 (value) CREATE
    code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0xF0]);
    // POP; STOP
    code.extend_from_slice(&[0x50, 0x00]);
    code
}

// ── DELEGATECALL attribution baseline ────────────────────────────────────

#[test]
fn delegatecall_carries_no_transfer_event() {
    // A plain DELEGATECALL with no value transfer must not generate a native Transfer.
    let delegator = Address::from([0xD0; 20]);
    let implementation = Address::from([0xD1; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    // delegator has balance but its code only does a DELEGATECALL with zero value
    db.insert_account_info(
        delegator,
        account(1_000, Some(delegatecall_to_then_stop(implementation))),
    );
    // implementation: just STOP (no further transfer)
    db.insert_account_info(implementation, account(0, Some(vec![0x00])));

    let (outcome, success) = run_capturing_tx(db, delegator, 10);
    assert!(success);
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => assert!(
            stream.is_empty(),
            "a DELEGATECALL with no value must not produce a native Transfer"
        ),
        other => panic!("expected Complete(empty), got {other:?}"),
    }
}

// ── T2c: DELEGATECALL simulation — CALL+CREATE attributed to delegating context ──

#[test]
fn delegatecall_nested_call_and_create_attributed_to_delegator() {
    // delegator holds balance and has code that DELEGATECALLs to `implementation`.
    // `implementation`'s code does CALL(1 wei, call_recipient) and CREATE(1 wei, empty).
    // Both operations run in `delegator`'s context → attributed to `delegator`.
    let delegator = Address::from([0xE0; 20]);
    let implementation = Address::from([0xE1; 20]);
    let call_recipient = Address::from([0xE2; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(
        delegator,
        account(1_000_000, Some(delegatecall_to_then_stop(implementation))),
    );
    db.insert_account_info(
        implementation,
        account(0, Some(delegated_call_and_create(call_recipient))),
    );
    db.insert_account_info(call_recipient, account(0, Some(vec![0x00])));

    let (outcome, success) = run_capturing_tx(db, delegator, 16);
    assert!(success, "top-level tx must succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 2, "expected two transfers: CALL(1) + CREATE(1)");
            for log in &stream {
                assert_eq!(log.address, NATIVE_ASSET_ADDRESS);
                // Both transfers originate from delegator (execution context for DELEGATECALL)
                assert_eq!(
                    log.topics()[1],
                    delegator.into_word(),
                    "transfer.from must be the delegating address, not the implementation"
                );
            }
        }
        other => panic!("expected Complete with 2 transfers, got {other:?}"),
    }
}

// ── T2a: EIP-7702 type-4 tx — transfers attributed to the authority address ──────────────

#[test]
fn eip7702_type4_delegated_transfers_attributed_to_authority() {
    // A genuine EIP-7702 type-4 transaction (TxEip7702) sets the delegation designation for
    // `authority` to `delegate_impl`. When the tx executes, the EVM runs `delegate_impl`'s
    // code in the context of `authority`. CALL(1 wei) + CREATE(1 wei) inside `delegate_impl`
    // must both be attributed to `authority` (the effective sender of those transfers).
    let delegate_impl = Address::from([0xE1; 20]);
    let call_recipient = Address::from([0xE2; 20]);
    let auth = Authorization { chain_id: U256::ZERO, address: delegate_impl, nonce: 0 }
        .into_signed(Signature::test_signature());
    let authority = auth.recover_authority().expect("deterministic authority from test_signature");
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(authority, account(1_000_000, None));
    db.insert_account_info(
        delegate_impl,
        account(0, Some(delegated_call_and_create(call_recipient))),
    );
    db.insert_account_info(call_recipient, account(0, None));
    let tx = TxEip7702 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 2_000_000,
        max_fee_per_gas: 0,
        max_priority_fee_per_gas: 0,
        to: authority,
        value: U256::ZERO,
        authorization_list: vec![auth],
        input: Default::default(),
        access_list: Default::default(),
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
            assert_eq!(stream.len(), 2, "EIP-7702 delegated code must capture CALL(1) + CREATE(1)");
            for log in &stream {
                assert_eq!(log.address, NATIVE_ASSET_ADDRESS);
                assert_eq!(
                    log.topics()[1],
                    authority.into_word(),
                    "transfer.from must be authority (EIP-7702 execution context)"
                );
            }
        }
        other => panic!("expected Complete with 2 transfers, got {other:?}"),
    }
}

// ── T2b: 2-hop DELEGATECALL chain attribution ────────────────────────────

#[test]
fn delegatecall_chain_inner_call_and_create_captured() {
    // root → DELEGATECALL → relay → DELEGATECALL → implementation
    // `implementation`'s CALL and CREATE run in `root`'s context throughout the 2-hop chain.
    let root = Address::from([0xC0; 20]);
    let relay = Address::from([0xC1; 20]);
    let implementation = Address::from([0xC2; 20]);
    let call_recipient = Address::from([0xC3; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(root, account(1_000_000, Some(delegatecall_to_then_stop(relay))));
    db.insert_account_info(relay, account(0, Some(delegatecall_to_then_stop(implementation))));
    db.insert_account_info(
        implementation,
        account(0, Some(delegated_call_and_create(call_recipient))),
    );
    db.insert_account_info(call_recipient, account(0, Some(vec![0x00])));

    let (outcome, success) = run_capturing_tx(db, root, 16);
    assert!(success, "2-hop DELEGATECALL chain tx must succeed");
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 2, "expected two transfers through 2-hop chain");
            for log in &stream {
                assert_eq!(
                    log.topics()[1],
                    root.into_word(),
                    "both transfers must be attributed to root (outermost delegating context)"
                );
            }
        }
        other => panic!("expected Complete with 2 transfers, got {other:?}"),
    }
}

// ── T2 proof-boundary: EIP-7702 origin-is-delegated path ─────────────────────────────────────

#[test]
fn eip7702_origin_itself_delegated_runs_inner_call_and_create() {
    // EIP-7702 case where the tx origin (signer) IS the delegation authority. The tx target is
    // also the origin, so the delegated code runs in the origin's context. The delegated code
    // (delegated_call_and_create) issues CALL(1 wei) + CREATE(1 wei), both attributed to the
    // origin. Two synthetic logs must be captured with from==origin on both.
    //
    // Hardfork basis: OpSpecId::JOVIAN maps through op-revm to SpecId::PRAGUE, enabling
    // EIP-7702 authority-tuple resolution.
    let delegate_impl = Address::from([0xE3; 20]);
    let call_recipient = Address::from([0xE4; 20]);
    // When origin == authority == tx sender, the EVM increments the sender's nonce from 0
    // to 1 before processing the authorization list. auth.nonce must therefore be 1 so that
    // the nonce check inside authorization processing succeeds (auth.nonce == 1 == origin.nonce
    // after increment). With nonce 0, the check would fail (0 != 1) and no delegation code
    // would be installed, causing the call to origin to be a no-op.
    let auth = Authorization { chain_id: U256::ZERO, address: delegate_impl, nonce: 1 }
        .into_signed(Signature::test_signature());
    let origin = auth.recover_authority().expect("deterministic authority from test_signature");
    let mut db = InMemoryDB::default();
    db.insert_account_info(origin, account(1_000_000, None));
    db.insert_account_info(
        delegate_impl,
        account(0, Some(delegated_call_and_create(call_recipient))),
    );
    db.insert_account_info(call_recipient, account(0, None));
    let tx = TxEip7702 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 2_000_000,
        max_fee_per_gas: 0,
        max_priority_fee_per_gas: 0,
        to: origin, // origin calls itself — delegation applies
        value: U256::ZERO,
        authorization_list: vec![auth],
        input: Default::default(),
        access_list: Default::default(),
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    let op_tx = OpTx::from_recovered_tx(&tx, origin);
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
    assert!(result.result.is_success(), "the delegated execution must succeed");
    // The EXACT deployed address the EVM reported for the inner CREATE, captured from the real
    // `CreateOutcome` handed to `create_end`. Asserting against this rules out every "any
    // plausible address" relaxation: a fourth address that is not the real one fails here.
    let (create_is_ok, _, _, deployed_address) = CREATE_END_OUTCOME.with(|s| s.get());
    assert!(create_is_ok, "the inner CREATE must report a successful completion");
    let deployed = deployed_address.expect("a successful CREATE reports its deployed address");
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());
    match outcome {
        CaptureOutcome::Complete { logs: stream, .. } => {
            assert_eq!(stream.len(), 2, "origin-delegated code must capture CALL(1) + CREATE(1)");
            for log in &stream {
                assert_eq!(log.address, NATIVE_ASSET_ADDRESS);
                assert_eq!(
                    log.topics()[1],
                    origin.into_word(),
                    "transfer.from must be origin (signer itself carries the delegation)"
                );
                assert_eq!(
                    log.data.data.as_ref(),
                    &U256::from(1u64).to_be_bytes::<32>(),
                    "each delegated native movement carries exactly 1 wei"
                );
            }
            // Per-transfer recipients: the CALL log pays the call recipient, and the CREATE log
            // pays the deployed contract — the exact address, not merely "some other address".
            assert_eq!(
                stream[0].topics()[2],
                call_recipient.into_word(),
                "CALL transfer.to must be the call recipient"
            );
            assert_eq!(
                stream[1].topics()[2],
                deployed.into_word(),
                "CREATE transfer.to must be the exact deployed address"
            );
            assert_ne!(
                stream[1].topics()[2],
                Address::ZERO.into_word(),
                "CREATE transfer.to must not be the zero address"
            );
            assert_ne!(
                stream[1].topics()[2],
                origin.into_word(),
                "CREATE transfer.to must not be the caller"
            );
            assert_ne!(
                stream[1].topics()[2],
                call_recipient.into_word(),
                "CREATE transfer.to must not be the CALL recipient"
            );
        }
        other => panic!("expected Complete with 2 transfers, got {other:?}"),
    }
}
