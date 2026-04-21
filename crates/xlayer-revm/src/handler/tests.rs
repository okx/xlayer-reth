//! Unit tests for [`XLayerAAHandler`](super::XLayerAAHandler).
//!
//! These tests exercise the AA-specific branches of the [`Handler`] trait
//! against in-memory revm state. Non-AA paths are covered by upstream
//! `op-revm`; we only verify that delegation routes correctly for a handful
//! of deposit/regular cases.

use op_revm::{
    L1BlockInfo, OpBuilder, OpSpecId, OpTransaction,
    transaction::error::OpTransactionError,
};
use revm::{
    Context, Journal, MainContext,
    context::{BlockEnv, CfgEnv, TxEnv},
    context_interface::{ContextTr, JournalTr, result::EVMError},
    database::InMemoryDB,
    database_interface::EmptyDB,
    handler::{EthFrame, EvmTr, FrameResult, Handler},
    interpreter::{CallOutcome, Gas, InstructionResult, InterpreterResult, interpreter::EthInterpreter},
    primitives::{Address, B256, Bytes, U256},
    state::AccountInfo,
};

use super::{XLayerAAHandler, helpers::aa_nonce_slot};
use crate::{
    constants::{MAX_ACCOUNT_CHANGES_PER_TX, MAX_CALLS_PER_TX, XLAYERAA_TX_TYPE},
    precompiles::NONCE_MANAGER_ADDRESS,
    transaction::{XLayerAACall, XLayerAAParts},
    tx_env::XLayerAATransaction,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

type TestCtx<DB> = Context<
    BlockEnv,
    XLayerAATransaction<TxEnv>,
    CfgEnv<OpSpecId>,
    DB,
    Journal<DB>,
    L1BlockInfo,
>;

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
    let op =
        OpTransaction { base, enveloped_tx: Some(Bytes::from_static(&[0u8])), deposit: Default::default() };
    XLayerAATransaction::with_parts(op, parts)
}

// ---------------------------------------------------------------------------
// validate_env
// ---------------------------------------------------------------------------

#[test]
fn validate_env_rejects_too_many_calls() {
    let parts = XLayerAAParts {
        call_phases: vec![
            (0..=MAX_CALLS_PER_TX as u64).map(|_| XLayerAACall::default()).collect(),
        ],
        ..Default::default()
    };

    let ctx = base_ctx().with_tx(build_aa_tx(
        TxEnv::builder().gas_limit(1_000_000).build_fill(),
        parts,
    ));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
    let err = Handler::validate_env(&handler, &mut evm).unwrap_err();
    assert!(format!("{err:?}").contains("too many calls"), "{err:?}");
}

#[test]
fn validate_env_rejects_too_many_account_changes() {
    let parts = XLayerAAParts {
        account_change_units: MAX_ACCOUNT_CHANGES_PER_TX + 1,
        ..Default::default()
    };

    let ctx = base_ctx().with_tx(build_aa_tx(
        TxEnv::builder().gas_limit(1_000_000).build_fill(),
        parts,
    ));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
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

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
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

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
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

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
    let gas = Handler::validate_initial_tx_gas(&handler, &mut evm).unwrap();
    assert_eq!(gas.initial_gas, 50_000);
    assert_eq!(gas.floor_gas, 0);
}

#[test]
fn validate_initial_tx_gas_rejects_intrinsic_above_gas_limit() {
    let parts = XLayerAAParts { aa_intrinsic_gas: 200_000, ..Default::default() };

    let ctx = base_ctx().with_tx(build_aa_tx(
        TxEnv::builder().gas_limit(100_000).build_fill(),
        parts,
    ));
    let mut evm = ctx.build_op();

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
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

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
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

    let mut handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
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
    db.insert_account_info(
        caller,
        AccountInfo { balance: U256::from(1000), ..Default::default() },
    );

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
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
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
        TxEnv::builder()
            .caller(sender)
            .gas_limit(100_000)
            .gas_price(10)
            .build_fill(),
        parts,
    );

    let ctx: TestCtx<InMemoryDB> = Context::mainnet()
        .with_db(db)
        .with_cfg(cfg)
        .with_tx(tx)
        .with_chain(L1BlockInfo::default());

    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
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

    let ctx: TestCtx<InMemoryDB> = Context::mainnet()
        .with_db(db)
        .with_cfg(cfg)
        .with_tx(tx)
        .with_chain(L1BlockInfo::default());

    let mut evm = ctx.build_op();
    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
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
    db.insert_account_info(
        sender,
        AccountInfo { balance: U256::ZERO, ..Default::default() },
    );
    db.insert_account_info(
        payer,
        AccountInfo { balance: U256::ZERO, ..Default::default() },
    );

    let parts = XLayerAAParts { sender, payer, ..Default::default() };

    // Use a pre-Isthmus spec to avoid needing operator-fee scalars on the
    // (default) L1BlockInfo used in this test.
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::HOLOCENE);
    cfg.disable_base_fee = true;
    cfg.disable_nonce_check = true;

    let tx = build_aa_tx(
        TxEnv::builder()
            .caller(sender)
            .gas_limit(100_000)
            .gas_price(10)
            .build_fill(),
        parts,
    );

    let ctx: TestCtx<InMemoryDB> = Context::mainnet()
        .with_db(db)
        .with_cfg(cfg)
        .with_tx(tx)
        .with_chain(L1BlockInfo::default());

    let mut evm = ctx.build_op();

    // Frame result with 50k unused gas and no refunds.
    let gas = Gas::new(50_000);
    let mut exec_result = FrameResult::Call(CallOutcome::new(
        InterpreterResult { result: InstructionResult::Stop, output: Bytes::new(), gas },
        0..0,
    ));

    let handler: XLayerAAHandler<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>> = XLayerAAHandler::new();
    Handler::reimburse_caller(&handler, &mut evm, &mut exec_result).unwrap();

    // Payer received 50_000 * 10 = 500_000.
    let payer_acc = evm.ctx().journal_mut().load_account(payer).unwrap();
    assert_eq!(payer_acc.info.balance, U256::from(500_000u64));

    // Sender balance unchanged.
    let sender_acc = evm.ctx().journal_mut().load_account(sender).unwrap();
    assert_eq!(sender_acc.info.balance, U256::ZERO);
}

