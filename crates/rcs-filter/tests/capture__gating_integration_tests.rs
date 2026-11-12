use super::ObservationBudget;
use super::*;
use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Signature, TxKind};
use op_revm::OpSpecId;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;
use revm::state::{AccountInfo, Bytecode};

fn caller() -> Address {
    Address::from([0xAA; 20])
}

fn account(balance: u64, code: Option<Vec<u8>>) -> AccountInfo {
    AccountInfo {
        balance: U256::from(balance),
        code: code.map(|c| Bytecode::new_raw(alloy_primitives::Bytes::from(c))),
        ..Default::default()
    }
}

/// The shared prologue for a `CALL(gas, dst, value, 0, 0, 0, 0)`: pushes the four zero
/// ret/args words, the `value`, and `PUSH20 dst`, leaving the stack ready for `GAS; CALL`.
/// Extracted so the hand-written CALL fixtures below don't each repeat the opcode prefix.
fn call_prefix(value: u8, dst: Address) -> Vec<u8> {
    // PUSH1 0 (retSize) PUSH1 0 (retOffset) PUSH1 0 (argsSize) PUSH1 0 (argsOffset)
    // PUSH1 value       PUSH20 dst
    let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, value, 0x73];
    code.extend_from_slice(dst.as_slice());
    code
}

fn call_tx(target: Address) -> OpTx {
    call_tx_from(caller(), target)
}

/// Same as [`call_tx`] but from an explicit sender. Two candidate txs in one test can then share
/// a single EVM/inspector without a nonce-ordering dependency: each distinct sender uses nonce 0.
fn call_tx_from(from: Address, target: Address) -> OpTx {
    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Call(target),
        value: U256::ZERO,
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    OpTx::from_recovered_tx(&tx, from)
}

/// Executes one candidate tx through a real composite-wrapped `OpEvm` and returns the capture
/// outcome plus whether the top-level tx succeeded. The capture seam (`start_capture` →
/// production `evm.transact` → `finish_capture(result.logs())`) mirrors the builder's real path;
/// `transact` is the same entry point `context.rs` uses (R9 decision), so the gate exercises the
/// production `PostExecCompositeInspector` forwarding and gasless/base-fee handling.
/// `OpSpecId::JOVIAN` maps through op-revm to `SpecId::PRAGUE`, enabling EIP-7702 authority-tuple
/// resolution. Tests that exercise DELEGATECALL attribution with delegated code depend on the
/// JOVIAN→PRAGUE mapping being in effect for the OpEvm spec selection.
fn run_capturing_tx(db: InMemoryDB, target: Address, budget: usize) -> (CaptureOutcome, bool) {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(budget),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(call_tx(target)).expect("tx executes");
    let success = result.result.is_success();
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());
    (outcome, success)
}

/// Runtime bytecode: `CALL(gas, dst, value=1, 0, 0, 0, 0); STOP` — one internal 1-wei transfer.
fn call_value_1_to(dst: Address) -> Vec<u8> {
    let mut code = call_prefix(0x01, dst);
    code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
    code
}

/// Runtime bytecode: `DELEGATECALL(gas, dst, 0, 0, 0, 0); POP; STOP`.
/// The caller delegates execution to `dst`; any CALL/CREATE inside `dst`'s code
/// runs with the delegating address as the execution context.
fn delegatecall_to_then_stop(dst: Address) -> Vec<u8> {
    // Stack order for DELEGATECALL (top first): gas, addr, argsOff, argsSize, retOff, retSize
    // Push in reverse: retSize, retOff, argsSize, argsOff, addr, gas
    let mut code = vec![
        0x60, 0x00, // PUSH1 0  (retSize)
        0x60, 0x00, // PUSH1 0  (retOffset)
        0x60, 0x00, // PUSH1 0  (argsSize)
        0x60, 0x00, // PUSH1 0  (argsOffset)
        0x73, // PUSH20
    ];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[
        0x62, 0x07, 0xA1, 0x20, // PUSH3 500000 (gas)
        0xF4, // DELEGATECALL
        0x50, // POP
        0x00, // STOP
    ]);
    code
}

/// Runtime bytecode: `CREATE(value=7, offset=0, size=0); STOP` — one CREATE with a 7-wei endowment
/// and empty init code.
fn create_endowment_7() -> Vec<u8> {
    // PUSH1 0 (size) PUSH1 0 (offset) PUSH1 7 (value) CREATE STOP
    vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x07, 0xf0, 0x00]
}

/// Runtime bytecode: `PUSH20 beneficiary; SELFDESTRUCT`.
fn selfdestruct_to(beneficiary: Address) -> Vec<u8> {
    let mut code = vec![0x73];
    code.extend_from_slice(beneficiary.as_slice());
    code.push(0xff);
    code
}

/// Runtime bytecode: `CALL(gas, dst, value=1, 0,0,0,0); POP; LOG0(0, 0); STOP` — one internal
/// 1-wei transfer followed by a persisted real `LOG0` in the same (successful) frame.
fn call_value_1_then_log0(dst: Address) -> Vec<u8> {
    let mut code = call_prefix(0x01, dst);
    // GAS CALL POP  PUSH1 0 (size) PUSH1 0 (offset) LOG0  STOP
    code.extend_from_slice(&[0x5a, 0xf1, 0x50, 0x60, 0x00, 0x60, 0x00, 0xa0, 0x00]);
    code
}

/// Runtime bytecode: two internal 1-wei CALLs (to `a` then `b`), each result popped, then STOP.
fn call_value_1_twice(a: Address, b: Address) -> Vec<u8> {
    let mut code = Vec::new();
    for dst in [a, b] {
        code.extend_from_slice(&call_prefix(0x01, dst)); // ret/args, value 1, PUSH20 dst
        code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
    }
    code.push(0x00); // STOP
    code
}

/// Runtime bytecode: `CREATE2(value=7, offset=0, size=0, salt=0); STOP` — a 7-wei endowment with
/// empty init code. CREATE2 pops value(top), offset, size, salt, so the pushes are salt, size,
/// offset, value.
fn create2_endowment_7() -> Vec<u8> {
    // PUSH1 0 (salt) PUSH1 0 (size) PUSH1 0 (offset) PUSH1 7 (value) CREATE2 STOP
    vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x07, 0xf5, 0x00]
}

#[path = "capture__gating_integration_tests__composite_forwarding_tests.rs"]
mod composite_forwarding_tests;
#[path = "capture__gating_integration_tests__create_tests.rs"]
mod create_tests;
#[path = "capture__gating_integration_tests__delegation_tests.rs"]
mod delegation_tests;
#[path = "capture__gating_integration_tests__screen_tx_tests.rs"]
mod screen_tx_tests;
#[path = "capture__gating_integration_tests__selfdestruct_tests.rs"]
mod selfdestruct_tests;
#[path = "capture__gating_integration_tests__tx_lifecycle_tests.rs"]
mod tx_lifecycle_tests;
