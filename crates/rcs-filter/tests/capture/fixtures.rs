//! Shared fixtures for the `capture` integration tests: the prelude every case uses and the
//! real-EVM helpers (accounts, transactions, bytecode builders, the observing runner).

pub(crate) use alloy_primitives::{keccak256, Address, Bytes, Log, LogData, B256, U256};
pub(crate) use rcs_filter::callstack::{CallObservation, CallObservationKind, SnapshotStamp};
pub(crate) use rcs_filter::capture::*;
pub(crate) use revm::interpreter::InstructionResult;
pub(crate) use revm::{bytecode::opcode, inspector::NoOpInspector, interpreter::CallScheme};

// The runtime fixture below produces a REAL `evm.transact`-driven `RealLogMismatch` outcome —
// one runtime source, not a re-implementation. The builder's loop-level fail-closed test does
// not consume it; that test forces the mismatch inside the candidate loop's own
// `RcsInspector::finish_capture` via the `FORCE_REALLOG_MISMATCH` seam.
pub(crate) use alloy_consensus::{SignableTransaction, TxLegacy};
pub(crate) use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
pub(crate) use alloy_op_evm::{OpEvmFactory, OpTx};
pub(crate) use alloy_primitives::{Signature, TxKind};
pub(crate) use op_revm::OpSpecId;
pub(crate) use revm::context::{BlockEnv, CfgEnv};
pub(crate) use revm::database::InMemoryDB;
pub(crate) use revm::state::{AccountInfo, Bytecode};

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

// ---------------------------------------------------------------- real-EVM helpers ------

pub(crate) fn caller() -> Address {
    Address::from([0xAA; 20])
}

pub(crate) fn account(balance: u64, code: Option<Vec<u8>>) -> AccountInfo {
    AccountInfo {
        balance: U256::from(balance),
        code: code.map(|c| Bytecode::new_raw(alloy_primitives::Bytes::from(c))),
        ..Default::default()
    }
}

/// The runtime code hash an observation must report for `code`: keccak256 of the bytecode the
/// EVM loads, which is also what the account's `code_hash` holds for a non-delegated account.
pub(crate) fn code_hash(code: &[u8]) -> B256 {
    keccak256(code)
}

pub(crate) fn stamp() -> SnapshotStamp {
    SnapshotStamp {
        protocol_version: 2,
        content_version: 42,
        selector_digest: B256::repeat_byte(0x5e),
    }
}

pub(crate) fn call_tx(target: Address) -> OpTx {
    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 3_000_000,
        to: TxKind::Call(target),
        value: U256::ZERO,
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    OpTx::from_recovered_tx(&tx, caller())
}

/// Executes one candidate transaction with the given capture selector set and observation
/// budget, through the same seam the builder uses.
pub(crate) fn run_observing_tx(
    db: InMemoryDB,
    target: Address,
    selectors: &[Address],
    observation_budget: usize,
) -> (CaptureOutcome, bool) {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::with_callstack(
            1_000,
            observation_budget,
            CallstackCaptureConfig::new(stamp(), selectors.to_vec()),
        ),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(call_tx(target)).expect("tx executes");
    let success = result.result.is_success();
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());
    (outcome, success)
}

/// Unwraps a completed capture, asserting the stamp travelled with the observations.
pub(crate) fn observations(outcome: CaptureOutcome) -> Vec<CallObservation> {
    match outcome {
        CaptureOutcome::Complete { observations, .. } => {
            assert_eq!(observations.stamp, stamp(), "the capture must carry its own stamp");
            observations.items
        }
        other => panic!("expected a completed capture, got {other:?}"),
    }
}

// ── bytecode builders ────────────────────────────────────────────────────────────────────

/// `DELEGATECALL(500000, dst, 0, 0, 0, 0); POP; STOP`.
pub(crate) fn delegatecall_to(dst: Address) -> Vec<u8> {
    let mut code = vec![
        0x60, 0x00, // PUSH1 0  retSize
        0x60, 0x00, // PUSH1 0  retOffset
        0x60, 0x00, // PUSH1 0  argsSize
        0x60, 0x00, // PUSH1 0  argsOffset
        0x73, // PUSH20 dst
    ];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[0x62, 0x07, 0xA1, 0x20, 0xF4, 0x50, 0x00]);
    code
}

/// `CALLCODE(500000, dst, value=0, 0, 0, 0, 0); POP; STOP`.
///
/// `CALLCODE` runs `dst`'s code but keeps the caller as BOTH the target and the executing
/// account, so it is not a proxy-implementation relationship.
pub(crate) fn callcode_to(dst: Address) -> Vec<u8> {
    let mut code = vec![
        0x60, 0x00, // PUSH1 0  retSize
        0x60, 0x00, // PUSH1 0  retOffset
        0x60, 0x00, // PUSH1 0  argsSize
        0x60, 0x00, // PUSH1 0  argsOffset
        0x60, 0x00, // PUSH1 0  value
        0x73, // PUSH20 dst
    ];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[0x62, 0x07, 0xA1, 0x20, 0xF2, 0x50, 0x00]);
    code
}

/// `CALL(500000, dst, value=0, 0, 0, 0, 0); POP; STOP`.
pub(crate) fn call_to(dst: Address) -> Vec<u8> {
    let mut code = vec![
        0x60, 0x00, // PUSH1 0  retSize
        0x60, 0x00, // PUSH1 0  retOffset
        0x60, 0x00, // PUSH1 0  argsSize
        0x60, 0x00, // PUSH1 0  argsOffset
        0x60, 0x00, // PUSH1 0  value
        0x73, // PUSH20 dst
    ];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[0x62, 0x07, 0xA1, 0x20, 0xF1, 0x50, 0x00]);
    code
}

/// `CALL` twice to the same destination, then STOP.
pub(crate) fn call_twice_to(dst: Address) -> Vec<u8> {
    let mut code = call_to(dst);
    code.pop(); // drop the trailing STOP
    code.extend_from_slice(&call_to(dst));
    code
}

/// `DELEGATECALL` twice from the same frame, to two destinations, then STOP.
pub(crate) fn delegatecall_twice(first: Address, second: Address) -> Vec<u8> {
    let mut code = delegatecall_to(first);
    code.pop();
    code.extend_from_slice(&delegatecall_to(second));
    code
}

/// `STOP` only.
pub(crate) fn stop_only() -> Vec<u8> {
    vec![0x00]
}

/// `PUSH1 0; PUSH1 0; REVERT` — executes, then reverts the frame.
pub(crate) fn revert_after_executing() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0xFD]
}

/// Proxy code that upgrades its own implementation pointer, delegatecalls through it, then
/// restores the pointer — all in one transaction:
/// `SSTORE(0, rogue); DELEGATECALL(SLOAD(0)); SSTORE(0, approved); STOP`.
///
/// Pre-state and post-state storage both name `approved`, so any value derived from storage
/// rather than from execution would report the wrong implementation.
pub(crate) fn upgrade_execute_rollback(rogue: Address, approved: Address) -> Vec<u8> {
    let mut code = vec![0x73];
    code.extend_from_slice(rogue.as_slice());
    code.extend_from_slice(&[0x60, 0x00, 0x55]); // PUSH1 0; SSTORE
    code.extend_from_slice(&[
        0x60, 0x00, // PUSH1 0  retSize
        0x60, 0x00, // PUSH1 0  retOffset
        0x60, 0x00, // PUSH1 0  argsSize
        0x60, 0x00, // PUSH1 0  argsOffset
        0x60, 0x00, 0x54, // PUSH1 0; SLOAD -> implementation address
        0x62, 0x07, 0xA1, 0x20, // PUSH3 500000 gas
        0xF4, // DELEGATECALL
        0x50, // POP
    ]);
    code.push(0x73);
    code.extend_from_slice(approved.as_slice());
    code.extend_from_slice(&[0x60, 0x00, 0x55, 0x00]); // PUSH1 0; SSTORE; STOP
    code
}

/// `CREATE(0, 0, 0)` with empty init code, twice, then STOP. Used to check that create frames
/// keep the frame stack balanced without producing observations.
pub(crate) fn create_twice() -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 0..2 {
        code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0xF0, 0x50]);
    }
    code.push(0x00);
    code
}

// ── no selector / not armed: the callstack path must be fully inert ───────────────────────

/// Executes one candidate like [`run_observing_tx`], but WITHOUT arming capture — the exact
/// state the inspector is in during the gasless pre-check and any other non-candidate
/// simulation. Returns the open-frame count observed after execution, read before any
/// lifecycle call could have cleared it.
pub(crate) fn run_unarmed_tx(db: InMemoryDB, target: Address, selectors: &[Address]) -> usize {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::with_callstack(
            1_000,
            16,
            CallstackCaptureConfig::new(stamp(), selectors.to_vec()),
        ),
    );
    let result = evm.transact(call_tx(target)).expect("tx executes");
    assert!(result.result.is_success(), "the unarmed control transaction still executes");
    evm.components_mut().1.open_frame_count()
}
