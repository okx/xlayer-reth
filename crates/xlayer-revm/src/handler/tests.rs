//! Unit tests for [`XLayerAAHandler`](super::XLayerAAHandler).
//!
//! These tests exercise the AA-specific branches of the [`Handler`] trait
//! against in-memory revm state. Non-AA paths are covered by upstream
//! `op-revm`; we only verify that delegation routes correctly for a handful
//! of deposit/regular cases.

// ---------------------------------------------------------------------------
// EIP-8130 conformance regressions
// ---------------------------------------------------------------------------
//
// Tests in this section pin behaviour defined by EIP-8130. Each test:
//   - fixes a concrete spec (OpSpecId::JOVIAN) so a future hardfork that
//     changes semantics is a *new* test rather than a silent drift here;
//   - asserts against error *variants* for upstream-owned errors (e.g.
//     `InvalidTransaction::InvalidChainId`) so a future rename surfaces as
//     a compile-time break, not a silent false positive;
//   - asserts against short, stable substrings for our own
//     `InvalidTransaction::Str(..)` errors — the strings are our code, so
//     if we rename them we also update the test.
//
// Upstream revm/op-revm fork bumps that touch these code paths will either:
//   a) rename an imported symbol — tests fail to compile, we fix;
//   b) restructure an error enum — tests fail to compile, we fix;
//   c) change *behaviour* — tests fail at runtime, which is the signal we
//      need to re-read the EIP or adjust our handler accordingly.
//
// -- When AA hardforks introduce fork-gated semantics ----------------------
//
// All tests below currently pin `OpSpecId::JOVIAN` because EIP-8130 was
// introduced whole-cloth at JOVIAN and the handler has no `spec.is_*()`
// branches inside these paths. Once a future fork gates AA behaviour,
// follow one of two patterns (cf. `tempo/crates/revm/src/handler.rs`):
//
//   * New-in-fork behaviour (e.g. a rule introduced at FJORD):
//     write TWO tests — `*_pre_fjord` on JOVIAN asserting old behaviour,
//     `*_post_fjord` on FJORD asserting new. The pre-test is a regression
//     guard against accidental back-porting. See tempo's
//     `test_self_sponsored_fee_payer_{rejected_post_t2,not_rejected_pre_t2}`.
//
//   * Same rule, fork-varying parameter (e.g. gas cost changes at FJORD):
//     single table-driven test — `for spec in [JOVIAN, FJORD, ...]` with
//     an `if spec.is_fjord() { a } else { b }` on the expected value, and
//     `"{spec:?}: ..."` in every assert message so failures pinpoint the
//     offending fork. See tempo's `test_2d_nonce_gas_in_intrinsic_gas`.

use op_revm::{
    transaction::error::OpTransactionError, L1BlockInfo, OpBuilder, OpSpecId, OpTransaction,
};
use revm::{
    bytecode::Bytecode,
    context::{result::InvalidTransaction, BlockEnv, CfgEnv, TxEnv},
    context_interface::{result::EVMError, ContextTr, JournalTr},
    database::InMemoryDB,
    database_interface::EmptyDB,
    handler::{EthFrame, EvmTr, FrameResult, Handler},
    interpreter::{
        interpreter::EthInterpreter, CallOutcome, Gas, InstructionResult, InterpreterResult,
    },
    primitives::{Address, Bytes, B256, U256},
    state::AccountInfo,
    Context, Journal, MainContext,
};

use super::{
    helpers::{aa_lock_slot, aa_nonce_slot, ACCOUNT_CONFIG_ADDRESS, NONCE_KEY_MAX},
    XLayerAAHandler,
};
use crate::{
    constants::{MAX_ACCOUNT_CHANGES_PER_TX, MAX_CALLS_PER_TX, XLAYERAA_TX_TYPE},
    precompiles::NONCE_MANAGER_ADDRESS,
    transaction::{decode_phase_statuses, XLayerAACall, XLayerAACodePlacement, XLayerAAParts},
    tx_env::XLayerAATransaction,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

type TestCtx<DB> =
    Context<BlockEnv, XLayerAATransaction<TxEnv>, CfgEnv<OpSpecId>, DB, Journal<DB>, L1BlockInfo>;

fn base_ctx() -> TestCtx<EmptyDB> {
    Context::mainnet()
        .with_tx(XLayerAATransaction::new(OpTransaction::<TxEnv>::default()))
        .with_cfg(CfgEnv::new_with_spec(OpSpecId::JOVIAN))
        .with_chain(L1BlockInfo::default())
}

/// Turn a `TxEnv`-based builder result into an AA transaction with the
/// given `XLayerAAParts` and the `0x7B` tx type byte.
fn build_aa_tx(base: TxEnv, parts: XLayerAAParts) -> XLayerAATransaction<TxEnv> {
    let mut base = base;
    base.tx_type = XLAYERAA_TX_TYPE;
    let op = OpTransaction {
        base,
        enveloped_tx: Some(Bytes::from_static(&[0u8])),
        deposit: Default::default(),
    };
    XLayerAATransaction::with_parts(op, parts)
}

// ---------------------------------------------------------------------------
// validate_env
// ---------------------------------------------------------------------------

#[test]
fn validate_env_rejects_too_many_calls() {
    let parts = XLayerAAParts {
        call_phases: vec![(0..=MAX_CALLS_PER_TX as u64).map(|_| XLayerAACall::default()).collect()],
        ..Default::default()
    };

    let ctx =
        base_ctx().with_tx(build_aa_tx(TxEnv::builder().gas_limit(1_000_000).build_fill(), parts));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_env(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("too many calls"), "{err:?}");
}

#[test]
fn validate_env_rejects_too_many_account_changes() {
    let parts = XLayerAAParts {
        account_change_units: MAX_ACCOUNT_CHANGES_PER_TX + 1,
        ..Default::default()
    };

    let ctx =
        base_ctx().with_tx(build_aa_tx(TxEnv::builder().gas_limit(1_000_000).build_fill(), parts));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_env(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("too many account changes"), "{err:?}");
}

#[test]
fn validate_env_rejects_expired_tx() {
    let parts = XLayerAAParts { expiry: 100, ..Default::default() };

    let block = BlockEnv { timestamp: U256::from(200u64), ..Default::default() };

    let ctx = base_ctx()
        .with_block(block)
        .with_tx(build_aa_tx(TxEnv::builder().gas_limit(1_000_000).build_fill(), parts));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_env(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("expired"), "{err:?}");
}

#[test]
fn validate_env_passes_unexpired_tx() {
    let parts = XLayerAAParts { expiry: 500, ..Default::default() };

    let block = BlockEnv { timestamp: U256::from(100u64), ..Default::default() };
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.disable_base_fee = true;

    let ctx = base_ctx()
        .with_block(block)
        .with_cfg(cfg)
        .with_tx(build_aa_tx(TxEnv::builder().gas_limit(1_000_000).build_fill(), parts));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    Handler::validate_env(&handler, &mut evm).unwrap();
}

// ---------------------------------------------------------------------------
// validate_initial_tx_gas
// ---------------------------------------------------------------------------

#[test]
fn validate_initial_tx_gas_returns_aa_intrinsic() {
    let parts = XLayerAAParts { aa_intrinsic_gas: 50_000, ..Default::default() };

    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.disable_base_fee = false; // not estimation

    let ctx = base_ctx()
        .with_cfg(cfg)
        .with_tx(build_aa_tx(TxEnv::builder().gas_limit(100_000).build_fill(), parts));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let gas = Handler::validate_initial_tx_gas(&handler, &mut evm).unwrap();
    assert_eq!(gas.initial_gas, 50_000);
    assert_eq!(gas.floor_gas, 0);
}

#[test]
fn validate_initial_tx_gas_rejects_intrinsic_above_gas_limit() {
    let parts = XLayerAAParts { aa_intrinsic_gas: 200_000, ..Default::default() };

    let ctx =
        base_ctx().with_tx(build_aa_tx(TxEnv::builder().gas_limit(100_000).build_fill(), parts));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_initial_tx_gas(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("CallGasCostMoreThanGasLimit"), "{err:?}");
}

#[test]
fn validate_initial_tx_gas_estimation_adds_calldata_overhead() {
    let parts = XLayerAAParts {
        aa_intrinsic_gas: 50_000,
        sender_auth_empty: true,
        payer_auth_empty: true,
        ..Default::default()
    };

    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.disable_base_fee = true; // estimation mode

    let ctx = base_ctx()
        .with_cfg(cfg)
        .with_tx(build_aa_tx(TxEnv::builder().gas_limit(200_000).build_fill(), parts));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let gas = Handler::validate_initial_tx_gas(&handler, &mut evm).unwrap();
    // 50_000 + 2 * ESTIMATION_AUTH_CALLDATA_GAS (1_100 each).
    assert_eq!(gas.initial_gas, 50_000 + 1_100 * 2);
}

// ---------------------------------------------------------------------------
// last_frame_result — delegated path, identical to op-revm behaviour
// ---------------------------------------------------------------------------

fn call_last_frame_return(
    ctx: TestCtx<EmptyDB>,
    instruction_result: InstructionResult,
    gas: Gas,
) -> Gas {
    let mut evm = ctx.build_op();
    let mut exec_result = FrameResult::Call(CallOutcome::new(
        InterpreterResult { result: instruction_result, output: Bytes::new(), gas },
        0..0,
    ));

    let mut handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    Handler::last_frame_result(&mut handler, &mut evm, &mut exec_result).unwrap();
    Handler::refund(&handler, &mut evm, &mut exec_result, 0);
    *exec_result.gas()
}

#[test]
fn last_frame_result_revert_regular_tx() {
    let ctx = base_ctx()
        .with_tx(XLayerAATransaction::new(
            OpTransaction::<TxEnv>::builder().base(TxEnv::builder().gas_limit(100)).build_fill(),
        ))
        .with_cfg(CfgEnv::new_with_spec(OpSpecId::BEDROCK));

    let gas = call_last_frame_return(ctx, InstructionResult::Revert, Gas::new(90));
    assert_eq!(gas.remaining(), 90);
    assert_eq!(gas.spent(), 10);
    assert_eq!(gas.refunded(), 0);
}

#[test]
fn last_frame_result_stop_regular_tx() {
    let ctx = base_ctx()
        .with_tx(XLayerAATransaction::new(
            OpTransaction::<TxEnv>::builder().base(TxEnv::builder().gas_limit(100)).build_fill(),
        ))
        .with_cfg(CfgEnv::new_with_spec(OpSpecId::REGOLITH));

    let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
    assert_eq!(gas.remaining(), 90);
    assert_eq!(gas.spent(), 10);
    assert_eq!(gas.refunded(), 0);
}

#[test]
fn last_frame_result_deposit_bedrock_spends_all() {
    let ctx = base_ctx()
        .with_tx(XLayerAATransaction::new(
            OpTransaction::<TxEnv>::builder()
                .base(TxEnv::builder().gas_limit(100))
                .source_hash(B256::from([1u8; 32]))
                .build_fill(),
        ))
        .with_cfg(CfgEnv::new_with_spec(OpSpecId::BEDROCK));

    let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
    assert_eq!(gas.remaining(), 0);
    assert_eq!(gas.spent(), 100);
    assert_eq!(gas.refunded(), 0);
}

// ---------------------------------------------------------------------------
// Deposit delegation: mint value credit
// ---------------------------------------------------------------------------

#[test]
fn validate_against_state_deposit_credits_mint() {
    let caller = Address::ZERO;
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller, AccountInfo { balance: U256::from(1000), ..Default::default() });

    let mut ctx: TestCtx<InMemoryDB> = Context::mainnet()
        .with_db(db)
        .with_tx(XLayerAATransaction::new(OpTransaction::<TxEnv>::default()))
        .with_cfg(CfgEnv::new_with_spec(OpSpecId::REGOLITH))
        .with_chain(L1BlockInfo {
            l1_base_fee: U256::from(1_000),
            l1_fee_overhead: Some(U256::from(1_000)),
            l1_base_fee_scalar: U256::from(1_000),
            ..Default::default()
        });
    ctx.modify_tx(|tx| {
        tx.op.deposit.source_hash = B256::from([1u8; 32]);
        tx.op.deposit.mint = Some(10);
    });

    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    Handler::validate_against_state_and_deduct_caller(&handler, &mut evm).unwrap();

    let account = evm.ctx().journal_mut().load_account(caller).unwrap();
    assert_eq!(account.info.balance, U256::from(1010));
}

// ---------------------------------------------------------------------------
// AA validate_against_state: deducts from payer, not caller
// ---------------------------------------------------------------------------

#[test]
fn validate_against_state_aa_deducts_from_payer() {
    let sender = Address::with_last_byte(0x11);
    let payer = Address::with_last_byte(0x22);

    let mut db = InMemoryDB::default();
    db.insert_account_info(
        sender,
        AccountInfo { balance: U256::from(1_000), ..Default::default() },
    );
    db.insert_account_info(
        payer,
        AccountInfo { balance: U256::from(10_000), ..Default::default() },
    );

    let parts = XLayerAAParts {
        sender,
        payer,
        nonce_key: U256::from(1),
        aa_intrinsic_gas: 21_000,
        ..Default::default()
    };

    // Use a pre-Isthmus spec so operator-fee paths are inactive.
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::HOLOCENE);
    cfg.disable_base_fee = true;
    cfg.disable_nonce_check = true;
    cfg.disable_balance_check = true;

    let tx = build_aa_tx(
        TxEnv::builder().caller(sender).gas_limit(100_000).gas_price(10).build_fill(),
        parts,
    );

    let ctx: TestCtx<InMemoryDB> =
        Context::mainnet().with_db(db).with_cfg(cfg).with_tx(tx).with_chain(L1BlockInfo::default());

    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    Handler::validate_against_state_and_deduct_caller(&handler, &mut evm).unwrap();

    // Payer balance should have been decremented (non-zero).
    let payer_acc = evm.ctx().journal_mut().load_account(payer).unwrap();
    assert!(payer_acc.info.balance < U256::from(10_000), "payer balance unchanged");

    // Sender balance should be untouched.
    let sender_acc = evm.ctx().journal_mut().load_account(sender).unwrap();
    assert_eq!(sender_acc.info.balance, U256::from(1_000));
}

// ---------------------------------------------------------------------------
// AA nonce increment: NonceManager slot written
// ---------------------------------------------------------------------------

#[test]
fn validate_against_state_aa_increments_nonce_manager_slot() {
    let sender = Address::with_last_byte(0x11);
    let payer = sender;

    let mut db = InMemoryDB::default();
    db.insert_account_info(
        sender,
        AccountInfo { balance: U256::from(10_000_000u128), ..Default::default() },
    );

    let parts = XLayerAAParts {
        sender,
        payer,
        nonce_key: U256::from(7),
        aa_intrinsic_gas: 21_000,
        ..Default::default()
    };

    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.disable_base_fee = true;
    cfg.disable_nonce_check = true;

    let tx = build_aa_tx(
        TxEnv::builder().caller(sender).gas_limit(100_000).nonce(0).build_fill(),
        parts,
    );

    let ctx: TestCtx<InMemoryDB> =
        Context::mainnet().with_db(db).with_cfg(cfg).with_tx(tx).with_chain(L1BlockInfo::default());

    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    Handler::validate_against_state_and_deduct_caller(&handler, &mut evm).unwrap();

    let slot = aa_nonce_slot(sender, U256::from(7));
    let new_seq = evm.ctx().journal_mut().sload(NONCE_MANAGER_ADDRESS, slot).unwrap().data;
    assert_eq!(new_seq, U256::from(1), "NonceManager slot should be incremented");
}

// ---------------------------------------------------------------------------
// AA reimburse_caller credits payer
// ---------------------------------------------------------------------------

#[test]
fn reimburse_caller_aa_refunds_payer() {
    let sender = Address::with_last_byte(0xAA);
    let payer = Address::with_last_byte(0xBB);

    let mut db = InMemoryDB::default();
    db.insert_account_info(sender, AccountInfo { balance: U256::ZERO, ..Default::default() });
    db.insert_account_info(payer, AccountInfo { balance: U256::ZERO, ..Default::default() });

    let parts = XLayerAAParts { sender, payer, ..Default::default() };

    // Use a pre-Isthmus spec to avoid needing operator-fee scalars on the
    // (default) L1BlockInfo used in this test.
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::HOLOCENE);
    cfg.disable_base_fee = true;
    cfg.disable_nonce_check = true;

    let tx = build_aa_tx(
        TxEnv::builder().caller(sender).gas_limit(100_000).gas_price(10).build_fill(),
        parts,
    );

    let ctx: TestCtx<InMemoryDB> =
        Context::mainnet().with_db(db).with_cfg(cfg).with_tx(tx).with_chain(L1BlockInfo::default());

    let mut evm = ctx.build_op();

    // Frame result with 50k unused gas and no refunds.
    let gas = Gas::new(50_000);
    let mut exec_result = FrameResult::Call(CallOutcome::new(
        InterpreterResult { result: InstructionResult::Stop, output: Bytes::new(), gas },
        0..0,
    ));

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    Handler::reimburse_caller(&handler, &mut evm, &mut exec_result).unwrap();

    // Payer received 50_000 * 10 = 500_000.
    let payer_acc = evm.ctx().journal_mut().load_account(payer).unwrap();
    assert_eq!(payer_acc.info.balance, U256::from(500_000u64));

    // Sender balance unchanged.
    let sender_acc = evm.ctx().journal_mut().load_account(sender).unwrap();
    assert_eq!(sender_acc.info.balance, U256::ZERO);
}

// --- P0 #1: phase break ----------------------------------------------------

#[test]
fn execution_skips_remaining_phases_after_failure() {
    // EIP-8130 Block execution: "if any call in a phase reverts, all state
    // changes for that phase are discarded and remaining phases are skipped."
    //
    // Observation: `execution()` encodes the per-phase success flags into
    // the returned `FrameResult` output. Correct behaviour → output has
    // exactly one (failed) entry. Pre-fix behaviour continued the loop,
    // producing two entries.
    let sender = Address::with_last_byte(0x11);
    let revert_target = Address::with_last_byte(0xAA);
    let nop_target = Address::with_last_byte(0xBB);

    let mut db = InMemoryDB::default();
    db.insert_account_info(
        sender,
        AccountInfo { balance: U256::from(1_000_000_000u128), ..Default::default() },
    );
    // 0xfe = INVALID opcode — any call to this address aborts with an error.
    db.insert_account_info(
        revert_target,
        AccountInfo {
            code: Some(Bytecode::new_raw(Bytes::from_static(&[0xfe]))),
            ..Default::default()
        },
    );
    // 0x00 = STOP — any call here halts cleanly and succeeds.
    db.insert_account_info(
        nop_target,
        AccountInfo {
            code: Some(Bytecode::new_raw(Bytes::from_static(&[0x00]))),
            ..Default::default()
        },
    );

    let parts = XLayerAAParts {
        sender,
        payer: sender,
        nonce_key: U256::from(1),
        aa_intrinsic_gas: 21_000,
        call_phases: vec![
            vec![XLayerAACall { to: revert_target, data: Bytes::new(), value: U256::ZERO }],
            vec![XLayerAACall { to: nop_target, data: Bytes::new(), value: U256::ZERO }],
        ],
        ..Default::default()
    };

    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.disable_base_fee = true;
    cfg.disable_nonce_check = true;

    let tx = build_aa_tx(TxEnv::builder().caller(sender).gas_limit(1_000_000).build_fill(), parts);

    let ctx: TestCtx<InMemoryDB> =
        Context::mainnet().with_db(db).with_cfg(cfg).with_tx(tx).with_chain(L1BlockInfo::default());

    let mut evm = ctx.build_op();
    let mut handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();

    Handler::validate_env(&handler, &mut evm).unwrap();
    let init_gas = Handler::validate_initial_tx_gas(&handler, &mut evm).unwrap();
    Handler::validate_against_state_and_deduct_caller(&handler, &mut evm).unwrap();
    let frame_result = Handler::execution(&mut handler, &mut evm, &init_gas).unwrap();

    let FrameResult::Call(outcome) = frame_result else {
        panic!("expected Call outcome");
    };
    let statuses = decode_phase_statuses(&outcome.result.output);
    assert_eq!(
        statuses.len(),
        1,
        "phase 1 must be skipped after phase 0 failure; got statuses={statuses:?}"
    );
    assert!(!statuses[0], "phase 0 should be marked as failed");
}

// --- P0 #2: nonce-free structural MUSTs ------------------------------------

#[test]
fn validate_env_rejects_nonce_free_without_expiry() {
    // EIP-8130: when `nonce_key == NONCE_KEY_MAX`, `expiry` MUST be non-zero.
    let parts = XLayerAAParts {
        nonce_key: NONCE_KEY_MAX,
        expiry: 0,
        nonce_free_hash: Some(B256::from([1u8; 32])),
        ..Default::default()
    };

    let ctx =
        base_ctx().with_tx(build_aa_tx(TxEnv::builder().gas_limit(1_000_000).build_fill(), parts));
    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_env(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("non-zero expiry"), "{err:?}");
}

#[test]
fn validate_env_rejects_nonce_free_with_nonzero_sequence() {
    // EIP-8130: when `nonce_key == NONCE_KEY_MAX`, `nonce_sequence` MUST be 0.
    let parts = XLayerAAParts {
        nonce_key: NONCE_KEY_MAX,
        expiry: 1_000,
        nonce_free_hash: Some(B256::from([1u8; 32])),
        ..Default::default()
    };

    let ctx = base_ctx()
        .with_tx(build_aa_tx(TxEnv::builder().gas_limit(1_000_000).nonce(42).build_fill(), parts));
    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_env(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("nonce_sequence == 0"), "{err:?}");
}

// --- P1-a: nonce_free_hash required ---------------------------------------

#[test]
fn validate_env_rejects_nonce_free_without_hash() {
    // Our implementation uses `nonce_free_hash` as the replay-protection key;
    // `None` would collide every such tx onto the zero-hash slot.
    let parts = XLayerAAParts {
        nonce_key: NONCE_KEY_MAX,
        expiry: 1_000,
        nonce_free_hash: None,
        ..Default::default()
    };

    let ctx =
        base_ctx().with_tx(build_aa_tx(TxEnv::builder().gas_limit(1_000_000).build_fill(), parts));
    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_env(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("nonce_free_hash"), "{err:?}");
}

// --- P1-b: lock check at step 1 for delegation entries --------------------

#[test]
fn state_validation_rejects_locked_account_with_delegation_entry() {
    // EIP-8130 Block execution step 1: "If `account_changes` contains config
    // change or delegation entries, read lock state for `from`. Reject
    // transaction if account is locked."
    //
    // Verifies that the lock check fires for a delegation-only tx (the
    // pre-fix implementation only checked locks when config changes were
    // present) AND that it runs before gas deduction.
    let sender = Address::with_last_byte(0x11);

    let mut db = InMemoryDB::default();
    db.insert_account_info(
        sender,
        AccountInfo { balance: U256::from(10_000_000u128), ..Default::default() },
    );

    // Lock packing: `unlocks_at` is a u64 stored in bytes [11..16] of the
    // 32-byte slot (see `check_account_lock`). Set unlocks_at = 1_000 while
    // block timestamp = 500, so the account is still locked.
    let unlocks_at: u64 = 1_000;
    let mut lock_bytes = [0u8; 32];
    lock_bytes[11..16].copy_from_slice(&unlocks_at.to_be_bytes()[3..8]);
    db.insert_account_storage(
        ACCOUNT_CONFIG_ADDRESS,
        aa_lock_slot(sender),
        U256::from_be_bytes(lock_bytes),
    )
    .unwrap();

    let parts = XLayerAAParts {
        sender,
        payer: sender,
        nonce_key: U256::from(1),
        delegation_target: Some(Address::with_last_byte(0xEE)),
        ..Default::default()
    };

    let block = BlockEnv { timestamp: U256::from(500u64), ..Default::default() };
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.disable_base_fee = true;
    cfg.disable_nonce_check = true;

    let tx = build_aa_tx(TxEnv::builder().caller(sender).gas_limit(100_000).build_fill(), parts);

    let ctx: TestCtx<InMemoryDB> = Context::mainnet()
        .with_db(db)
        .with_cfg(cfg)
        .with_block(block)
        .with_tx(tx)
        .with_chain(L1BlockInfo::default());

    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_against_state_and_deduct_caller(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("account is locked"), "{err:?}");
}

// --- P2-a: chain_id check in AA branch ------------------------------------

#[test]
fn validate_env_rejects_mismatched_chain_id() {
    // Upstream mainnet validate_env performs the EIP-155 chain_id check,
    // but the AA branch returns early and must replicate it.
    //
    // Asserts the *structural* error variant (not a string) because
    // `InvalidChainId` is an upstream symbol — a future rename should
    // surface as a compile error, not as a silently-broken test.
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.chain_id = 1;

    let tx = build_aa_tx(
        TxEnv::builder().gas_limit(1_000_000).chain_id(Some(999)).build_fill(),
        XLayerAAParts::default(),
    );

    let ctx: TestCtx<EmptyDB> =
        Context::mainnet().with_cfg(cfg).with_tx(tx).with_chain(L1BlockInfo::default());
    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_env(&handler, &mut evm).unwrap_err();
    assert!(
        matches!(
            err,
            EVMError::Transaction(OpTransactionError::Base(InvalidTransaction::InvalidChainId))
        ),
        "{err:?}"
    );
}

// --- P2-b: at most one create entry ---------------------------------------

#[test]
fn validate_env_rejects_multiple_create_entries() {
    // EIP-8130 permits at most one create entry per transaction.
    // `code_placements` is the post-parse representation of create entries.
    let parts = XLayerAAParts {
        code_placements: vec![
            XLayerAACodePlacement {
                address: Address::with_last_byte(0x1),
                code: Bytes::from_static(&[0x00]),
            },
            XLayerAACodePlacement {
                address: Address::with_last_byte(0x2),
                code: Bytes::from_static(&[0x00]),
            },
        ],
        account_change_units: 2,
        ..Default::default()
    };

    let ctx =
        base_ctx().with_tx(build_aa_tx(TxEnv::builder().gas_limit(1_000_000).build_fill(), parts));
    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> =
        XLayerAAHandler::new();
    let err = Handler::validate_env(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("multiple create entries"), "{err:?}");
}
