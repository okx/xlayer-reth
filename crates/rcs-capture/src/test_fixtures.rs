use super::*;
use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Signature, TxKind};
use op_revm::OpSpecId;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;
use revm::state::{AccountInfo, Bytecode};

/// Produces a REAL [`CaptureOutcome::InvariantViolation`] carrying a
/// [`CaptureInvariantError::RealLogMismatch`] from a production `evm.transact` on an
/// `OpEvm<_, RcsInspector, _>` — NOT a hand-constructed outcome. A contract emits one real `LOG0`
/// (captured through the composite-forwarded `log` hook), then `finish_capture` runs against a
/// deliberately tampered `result.logs()` slice so the RealLog cross-check diverges (observed 1 vs
/// expected 2). This is the runtime source asserted by test 33 (the runtime `InvariantViolation`
/// outcome). The loop-level test 35 in `context.rs` does NOT consume this fixture — it forces the
/// mismatch through the real candidate loop's own inspector via `FORCE_REALLOG_MISMATCH`.
pub(crate) fn real_runtime_reallogmismatch_outcome() -> CaptureOutcome {
    let caller = Address::from([0xAA; 20]);
    let contract = Address::from([0xC7; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        caller,
        AccountInfo { balance: U256::from(1_000_000_000u64), ..Default::default() },
    );
    // Runtime bytecode: `LOG0(0, 0); STOP` — emits one real (empty) log then returns successfully.
    db.insert_account_info(
        contract,
        AccountInfo {
            code: Some(Bytecode::new_raw(Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xa0, 0x00]))),
            ..Default::default()
        },
    );
    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Call(contract),
        value: U256::ZERO,
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    let op_tx = OpTx::from_recovered_tx(&tx, caller);
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
    assert!(result.result.is_success(), "the emitting tx itself succeeds");
    assert_eq!(result.result.logs().len(), 1, "one real log persisted");
    // Force divergence at runtime: append a fabricated log so the captured RealLog subsequence
    // (1) no longer matches the claimed `result.logs()` (2).
    let mut tampered = result.result.logs().to_vec();
    tampered.push(native_transfer_log(caller, NATIVE_ASSET_ADDRESS, U256::from(1u64)));
    evm.components_mut().1.finish_capture(&tampered)
}
