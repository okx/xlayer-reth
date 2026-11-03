//! Call-frame observation behavior, driven through a real EVM.
//!
//! These tests pin what distinguishes a proxy's own direct delegatecall from a delegatecall made
//! inside implementation business logic, that the reported hash is of the code that actually
//! executed, and that a frame which never executed never becomes an observation.

use super::*;
use alloy_consensus::{SignableTransaction, TxEip7702, TxLegacy};
use alloy_eips::eip7702::Authorization;
use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Signature, TxKind};
use op_revm::OpSpecId;
use rcs_filter::callstack::{CallObservation, CallObservationKind};
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

/// The runtime code hash an observation must report for `code`: keccak256 of the bytecode the
/// EVM loads, which is also what the account's `code_hash` holds for a non-delegated account.
fn code_hash(code: &[u8]) -> B256 {
    keccak256(code)
}

fn stamp() -> SnapshotStamp {
    SnapshotStamp {
        protocol_version: 2,
        content_version: 42,
        selector_digest: B256::repeat_byte(0x5e),
    }
}

fn call_tx(target: Address) -> OpTx {
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
fn run_observing_tx(
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
fn observations(outcome: CaptureOutcome) -> Vec<CallObservation> {
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
fn delegatecall_to(dst: Address) -> Vec<u8> {
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
fn callcode_to(dst: Address) -> Vec<u8> {
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
fn call_to(dst: Address) -> Vec<u8> {
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
fn call_twice_to(dst: Address) -> Vec<u8> {
    let mut code = call_to(dst);
    code.pop(); // drop the trailing STOP
    code.extend_from_slice(&call_to(dst));
    code
}

/// `DELEGATECALL` twice from the same frame, to two destinations, then STOP.
fn delegatecall_twice(first: Address, second: Address) -> Vec<u8> {
    let mut code = delegatecall_to(first);
    code.pop();
    code.extend_from_slice(&delegatecall_to(second));
    code
}

/// `STOP` only.
fn stop_only() -> Vec<u8> {
    vec![0x00]
}

/// `PUSH1 0; PUSH1 0; REVERT` — executes, then reverts the frame.
fn revert_after_executing() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0xFD]
}

/// Proxy code that upgrades its own implementation pointer, delegatecalls through it, then
/// restores the pointer — all in one transaction:
/// `SSTORE(0, rogue); DELEGATECALL(SLOAD(0)); SSTORE(0, approved); STOP`.
///
/// Pre-state and post-state storage both name `approved`, so any value derived from storage
/// rather than from execution would report the wrong implementation.
fn upgrade_execute_rollback(rogue: Address, approved: Address) -> Vec<u8> {
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
fn create_twice() -> Vec<u8> {
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
fn run_unarmed_tx(db: InMemoryDB, target: Address, selectors: &[Address]) -> usize {
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

/// Proxy → DELEGATECALL → implementation, with the proxy selected: the positive control for the
/// two tests below. Exactly the two frames whose hash is actually reported get hashed.
#[test]
fn a_selected_proxy_hashes_exactly_the_two_frames_it_reports() {
    reset_callstack_hash_counter();
    let proxy = Address::from([0xD1; 20]);
    let implementation = Address::from([0xD2; 20]);
    let proxy_code = delegatecall_to(implementation);
    let impl_code = stop_only();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(proxy, account(0, Some(proxy_code.clone())));
    db.insert_account_info(implementation, account(0, Some(impl_code.clone())));

    let (outcome, success) = run_observing_tx(db, proxy, &[proxy], 16);
    assert!(success);
    assert_eq!(
        observations(outcome),
        vec![
            CallObservation::call(proxy, code_hash(&proxy_code)),
            CallObservation::direct_delegatecall(
                proxy,
                code_hash(&proxy_code),
                implementation,
                code_hash(&impl_code),
            ),
        ]
    );
    assert_eq!(
        callstack_hash_computations(),
        2,
        "exactly the proxy frame and its direct delegatecall are hashed"
    );
}

/// An unselected frame is never hashed, even when it executes and even when a selected frame
/// runs inside it.
///
/// helper → CALL → selected: two frames execute, one of them is an execution identity some
/// selector asked about, so exactly one hash is computed. A hook that hashed every frame it
/// initialized would report 2 here.
#[test]
fn only_the_frames_whose_identity_is_reported_are_hashed() {
    reset_callstack_hash_counter();
    let helper = Address::from([0xD3; 20]);
    let selected = Address::from([0xD4; 20]);
    let selected_code = stop_only();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(helper, account(0, Some(call_to(selected))));
    db.insert_account_info(selected, account(0, Some(selected_code.clone())));

    let (outcome, success) = run_observing_tx(db, helper, &[selected], 16);
    assert!(success);
    assert_eq!(
        observations(outcome),
        vec![CallObservation::call(selected, code_hash(&selected_code))],
        "the selected address is still observed through the helper"
    );
    assert_eq!(
        callstack_hash_computations(),
        1,
        "the unselected helper frame executed but was never hashed"
    );
}

/// An empty selector set must cost nothing: no bytecode hash and no frame-state work, for a
/// transaction whose frames would otherwise all qualify.
///
/// This is the shape every deployment without a callstack rule runs in, so "inert" has to mean
/// genuinely no work rather than merely no observation.
#[test]
fn an_empty_selector_set_computes_no_hash_and_tracks_no_frame() {
    reset_callstack_hash_counter();
    let proxy = Address::from([0xD5; 20]);
    let implementation = Address::from([0xD6; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(proxy, account(0, Some(delegatecall_to(implementation))));
    db.insert_account_info(implementation, account(0, Some(stop_only())));

    let (outcome, success) = run_observing_tx(db, proxy, &[], 16);
    assert!(success);
    assert!(observations(outcome).is_empty());
    assert_eq!(
        callstack_hash_computations(),
        0,
        "an empty selector set must not hash any frame's bytecode"
    );
}

/// The same inertness, asserted on frame state rather than on hashing: with an empty selector
/// set no [`FrameRecord`] is pushed at all, and the window still closes as `Complete` — an
/// `enter_frame` that pushed while `exit_frame` did not pop (or the reverse) would surface
/// `FrameStackUnbalanced` instead.
#[test]
fn an_empty_selector_set_pushes_no_frame_record() {
    let any = Address::from([0xD7; 20]);
    let mut inspector = RcsInspector::with_callstack(1_000, 16, CallstackCaptureConfig::default());
    inspector.start_capture();
    inspector.enter_frame(any, any, Some(CallScheme::Call));
    assert_eq!(inspector.open_frame_count(), 0, "no frame is tracked for an inert selector set");
    inspector.exit_frame();
    assert_eq!(inspector.open_frame_count(), 0);
    match inspector.finish_capture(&[]) {
        CaptureOutcome::Complete { observations, .. } => assert!(observations.items.is_empty()),
        other => panic!("an inert window must close cleanly, got {other:?}"),
    }
}

/// An inspector that was never armed does no callstack work either — the state the gasless
/// pre-check and every other non-candidate simulation run in. The selector set is deliberately
/// NON-empty here, so only the `active` gate can be what suppresses the work.
#[test]
fn an_unarmed_inspector_computes_no_hash_and_tracks_no_frame() {
    reset_callstack_hash_counter();
    let proxy = Address::from([0xD8; 20]);
    let implementation = Address::from([0xD9; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(proxy, account(0, Some(delegatecall_to(implementation))));
    db.insert_account_info(implementation, account(0, Some(stop_only())));

    let open_frames = run_unarmed_tx(db, proxy, &[proxy]);
    assert_eq!(open_frames, 0, "an unarmed inspector tracks no frame");
    assert_eq!(
        callstack_hash_computations(),
        0,
        "an unarmed inspector must not hash any frame's bytecode"
    );
}

// ── direct call vs. direct delegatecall ──────────────────────────────────────────────────

#[test]
fn a_selected_address_running_its_own_code_is_a_call_observation() {
    let selected = Address::from([0xC1; 20]);
    let code = stop_only();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(selected, account(0, Some(code.clone())));

    let (outcome, success) = run_observing_tx(db, selected, &[selected], 16);
    assert!(success);
    let items = observations(outcome);
    assert_eq!(items, vec![CallObservation::call(selected, code_hash(&code))]);
}

#[test]
fn an_unselected_address_produces_no_observation() {
    let other = Address::from([0xC2; 20]);
    let selected = Address::from([0xC1; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(other, account(0, Some(stop_only())));

    // Capture ran and genuinely saw nothing for the selector: an empty available set.
    let (outcome, success) = run_observing_tx(db, other, &[selected], 16);
    assert!(success);
    assert!(observations(outcome).is_empty());
}

#[test]
fn an_inert_selector_set_observes_nothing() {
    let target = Address::from([0xC3; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(target, account(0, Some(stop_only())));

    let (outcome, success) = run_observing_tx(db, target, &[], 16);
    assert!(success);
    assert!(observations(outcome).is_empty());
}

#[test]
fn a_selected_proxy_reports_its_own_code_and_its_direct_delegatecall() {
    // A standard proxy invocation legitimately yields BOTH observations.
    let proxy = Address::from([0xB1; 20]);
    let implementation = Address::from([0xB2; 20]);
    let proxy_code = delegatecall_to(implementation);
    let impl_code = stop_only();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(proxy, account(0, Some(proxy_code.clone())));
    db.insert_account_info(implementation, account(0, Some(impl_code.clone())));

    let (outcome, success) = run_observing_tx(db, proxy, &[proxy], 16);
    assert!(success);
    let items = observations(outcome);
    assert_eq!(
        items,
        vec![
            CallObservation::call(proxy, code_hash(&proxy_code)),
            CallObservation::direct_delegatecall(
                proxy,
                code_hash(&proxy_code),
                implementation,
                code_hash(&impl_code),
            ),
        ]
    );
}

#[test]
fn an_implementation_internal_delegatecall_is_not_the_proxys_direct_delegatecall() {
    // proxy -> DELEGATECALL -> relay -> DELEGATECALL -> library.
    //
    // Both inner frames keep `proxy` as their execution identity, so a predicate based on the
    // frame's own target would report `library` as the proxy's implementation. Exactly one
    // direct delegatecall exists, and its implementation is `relay`: the second hop is made
    // by implementation business logic, which is explicitly out of scope.
    let proxy = Address::from([0xB1; 20]);
    let relay = Address::from([0xB2; 20]);
    let library = Address::from([0xB3; 20]);
    let proxy_code = delegatecall_to(relay);
    let relay_code = delegatecall_to(library);
    let library_code = stop_only();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(proxy, account(0, Some(proxy_code.clone())));
    db.insert_account_info(relay, account(0, Some(relay_code.clone())));
    db.insert_account_info(library, account(0, Some(library_code)));

    let (outcome, success) = run_observing_tx(db, proxy, &[proxy], 16);
    assert!(success);
    let items = observations(outcome);
    let delegate: Vec<&CallObservation> =
        items.iter().filter(|o| o.kind == CallObservationKind::DirectDelegatecall).collect();
    assert_eq!(delegate.len(), 1, "exactly one direct delegatecall: {items:?}");
    assert_eq!(delegate[0].impl_address, Some(relay));
    assert_eq!(delegate[0].impl_code_hash, Some(code_hash(&relay_code)));
    // `library` must appear nowhere.
    assert!(
        !items.iter().any(|o| o.impl_address == Some(library)),
        "an implementation-internal delegatecall must not be reported: {items:?}"
    );
}

#[test]
fn an_indirect_call_through_a_helper_still_observes_the_selected_address() {
    let helper = Address::from([0xD1; 20]);
    let selected = Address::from([0xD2; 20]);
    let selected_code = stop_only();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(helper, account(0, Some(call_to(selected))));
    db.insert_account_info(selected, account(0, Some(selected_code.clone())));

    let (outcome, success) = run_observing_tx(db, helper, &[selected], 16);
    assert!(success);
    assert_eq!(
        observations(outcome),
        vec![CallObservation::call(selected, code_hash(&selected_code))]
    );
}

#[test]
fn repeated_calls_to_the_same_address_are_independent_observations() {
    let helper = Address::from([0xD1; 20]);
    let selected = Address::from([0xD2; 20]);
    let selected_code = stop_only();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(helper, account(0, Some(call_twice_to(selected))));
    db.insert_account_info(selected, account(0, Some(selected_code.clone())));

    let (outcome, success) = run_observing_tx(db, helper, &[selected], 16);
    assert!(success);
    let items = observations(outcome);
    assert_eq!(items.len(), 2, "each frame is its own observation: {items:?}");
    assert!(items.iter().all(|o| *o == CallObservation::call(selected, code_hash(&selected_code))));
}

#[test]
fn two_direct_delegatecalls_from_one_frame_each_emit_an_observation() {
    let proxy = Address::from([0xB1; 20]);
    let first = Address::from([0xB2; 20]);
    let second = Address::from([0xB3; 20]);
    let proxy_code = delegatecall_twice(first, second);
    let first_code = stop_only();
    let second_code = revert_after_executing();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(proxy, account(0, Some(proxy_code.clone())));
    db.insert_account_info(first, account(0, Some(first_code.clone())));
    db.insert_account_info(second, account(0, Some(second_code.clone())));

    let (outcome, _) = run_observing_tx(db, proxy, &[proxy], 16);
    let items = observations(outcome);
    let impls: Vec<Option<Address>> = items
        .iter()
        .filter(|o| o.kind == CallObservationKind::DirectDelegatecall)
        .map(|o| o.impl_address)
        .collect();
    assert_eq!(impls, vec![Some(first), Some(second)], "both hops observed: {items:?}");
}

#[test]
fn callcode_is_not_a_direct_delegatecall() {
    // `CALLCODE` runs another account's code but leaves the caller as its own target, so it
    // describes no proxy-implementation relationship and must not be reported as one.
    let selected = Address::from([0xB1; 20]);
    let other = Address::from([0xB2; 20]);
    let selected_code = callcode_to(other);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(selected, account(0, Some(selected_code.clone())));
    db.insert_account_info(other, account(0, Some(stop_only())));

    let (outcome, success) = run_observing_tx(db, selected, &[selected], 16);
    assert!(success);
    let items = observations(outcome);
    assert!(
        items.iter().all(|o| o.kind == CallObservationKind::Call),
        "CALLCODE must not produce a direct_delegatecall: {items:?}"
    );
    assert!(items.iter().all(|o| o.impl_address.is_none()));
}

// ── the hash must be of the code that actually executed ──────────────────────────────────

#[test]
fn an_upgrade_executed_and_rolled_back_in_one_transaction_reports_what_ran() {
    // The proxy points at `approved` before and after the transaction, but delegatecalls
    // through `rogue` in between. Reading the pointer from storage — before or after — would
    // report `approved`; only the execution-time observation reports what really ran.
    let proxy = Address::from([0xB1; 20]);
    let rogue = Address::from([0xEE; 20]);
    let approved = Address::from([0xB3; 20]);
    let proxy_code = upgrade_execute_rollback(rogue, approved);
    let rogue_code = vec![0x60, 0x01, 0x50, 0x00]; // PUSH1 1; POP; STOP — distinct bytecode
    let approved_code = stop_only();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(proxy, account(0, Some(proxy_code.clone())));
    db.insert_account_info(rogue, account(0, Some(rogue_code.clone())));
    db.insert_account_info(approved, account(0, Some(approved_code.clone())));

    let (outcome, success) = run_observing_tx(db, proxy, &[proxy], 16);
    assert!(success, "the upgrade/execute/rollback transaction itself succeeds");
    let items = observations(outcome);
    let delegate: Vec<&CallObservation> =
        items.iter().filter(|o| o.kind == CallObservationKind::DirectDelegatecall).collect();
    assert_eq!(delegate.len(), 1, "{items:?}");
    assert_eq!(
        delegate[0].impl_address,
        Some(rogue),
        "the executed implementation, not the pointer"
    );
    assert_eq!(delegate[0].impl_code_hash, Some(code_hash(&rogue_code)));
    assert_ne!(
        delegate[0].impl_code_hash,
        Some(code_hash(&approved_code)),
        "the restored pointer's code must not be what is reported"
    );
}

#[test]
fn an_executed_then_reverted_frame_remains_observable() {
    // A revert undoes effects, not the fact that the code ran. A malicious call that executed
    // and was then reverted is exactly what a risk rule needs to still see.
    let helper = Address::from([0xD1; 20]);
    let selected = Address::from([0xD2; 20]);
    let selected_code = revert_after_executing();
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(helper, account(0, Some(call_to(selected))));
    db.insert_account_info(selected, account(0, Some(selected_code.clone())));

    let (outcome, success) = run_observing_tx(db, helper, &[selected], 16);
    assert!(success, "the helper swallows the inner revert");
    assert_eq!(
        observations(outcome),
        vec![CallObservation::call(selected, code_hash(&selected_code))],
        "the reverted frame's execution identity survives"
    );
}

#[test]
fn a_frame_that_never_executed_produces_no_observation() {
    // The `call` hook fires before the frame is initialized, and a call into an account with
    // no code returns without ever creating an interpreter. Emitting at hook arrival would
    // have reported this call as if it had run.
    let helper = Address::from([0xD1; 20]);
    let empty = Address::from([0xD3; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(helper, account(0, Some(call_to(empty))));
    db.insert_account_info(empty, account(0, None));

    let (outcome, success) = run_observing_tx(db, helper, &[empty], 16);
    assert!(success);
    assert!(
        observations(outcome).is_empty(),
        "a call whose frame never executed must not appear as an observation"
    );
}

#[test]
fn a_precompile_frame_produces_no_observation() {
    // A precompile is answered without an interpreter, so it never reaches the confirmed-
    // execution point even though its `call` hook fires.
    let helper = Address::from([0xD1; 20]);
    let identity_precompile =
        Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(helper, account(0, Some(call_to(identity_precompile))));

    let (outcome, success) = run_observing_tx(db, helper, &[identity_precompile], 16);
    assert!(success);
    assert!(observations(outcome).is_empty(), "a precompile frame runs no bytecode");
}

#[test]
fn eip7702_delegation_is_a_call_observation_with_the_executed_delegate_code() {
    // Under EIP-7702 the account's own `code_hash` names the delegation designator while the
    // code that runs is the delegate's. The observation must report the delegate's executed
    // code hash — and must NOT describe this as a direct delegatecall, which would
    // misrepresent the relationship as a proxy implementation.
    let delegate = Address::from([0xE1; 20]);
    let delegate_code = stop_only();
    let auth = Authorization { chain_id: U256::ZERO, address: delegate, nonce: 0 }
        .into_signed(Signature::test_signature());
    let authority = auth.recover_authority().expect("deterministic test authority");
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(authority, account(1_000_000, None));
    db.insert_account_info(delegate, account(0, Some(delegate_code.clone())));
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
        RcsInspector::with_callstack(
            1_000,
            16,
            CallstackCaptureConfig::new(stamp(), vec![authority]),
        ),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(op_tx).expect("tx executes");
    assert!(result.result.is_success());
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());
    let items = observations(outcome);
    assert_eq!(
        items,
        vec![CallObservation::call(authority, code_hash(&delegate_code))],
        "a 7702-delegated call is a call observation carrying the delegate's executed hash"
    );
    assert!(items[0].impl_address.is_none(), "7702 delegation is not a direct delegatecall");
    assert!(items[0].impl_code_hash.is_none());
}

// ── budgets and frame-stack invariants ───────────────────────────────────────────────────

#[test]
fn the_observation_budget_accepts_the_limit_th_and_fails_closed_on_the_next() {
    let helper = Address::from([0xD1; 20]);
    let selected = Address::from([0xD2; 20]);
    let build = || {
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(helper, account(0, Some(call_twice_to(selected))));
        db.insert_account_info(selected, account(0, Some(stop_only())));
        db
    };

    // Two observations with a budget of two: the limit-th is accepted.
    let (outcome, _) = run_observing_tx(build(), helper, &[selected], 2);
    assert_eq!(observations(outcome).len(), 2);

    // The (limit+1)-th overflows, and a truncated set must never be screened as complete.
    let (outcome, success) = run_observing_tx(build(), helper, &[selected], 1);
    assert!(success, "the EVM keeps executing normally after an observation overflow");
    match outcome {
        CaptureOutcome::LimitExceeded { attempted, limit, budget } => {
            assert_eq!(attempted, 2);
            assert_eq!(limit, 1);
            assert_eq!(budget, ObservationBudget::Callstack);
            assert_eq!(budget.as_str(), "callstack");
        }
        other => panic!("expected a callstack budget overflow, got {other:?}"),
    }
}

#[test]
fn create_frames_keep_the_frame_stack_balanced_and_emit_no_observation() {
    // A create frame runs init code, not deployed runtime code, so the only hash available
    // for it would be creation bytecode. It is tracked purely for frame balance.
    let factory = Address::from([0xF1; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(factory, account(1_000, Some(create_twice())));

    let (outcome, success) = run_observing_tx(db, factory, &[factory, factory.create(0)], 16);
    assert!(success);
    let items = observations(outcome);
    // The factory's own top-level frame is a call observation; neither CREATE frame is.
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].address, factory);
    assert_eq!(items[0].kind, CallObservationKind::Call);
    assert!(
        !items.iter().any(|o| o.address == factory.create(0)),
        "a created address's init-code frame is not an observation: {items:?}"
    );
}

#[test]
fn an_unbalanced_frame_stack_fails_closed() {
    // Driven directly, because a real EVM always balances its frames. A leftover entry means
    // parentage cannot be trusted, so the capture must not be reported as complete.
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        16,
        CallstackCaptureConfig::new(stamp(), vec![Address::repeat_byte(0x11)]),
    );
    inspector.start_capture();
    inspector.enter_frame(
        Address::repeat_byte(0x11),
        Address::repeat_byte(0x11),
        Some(CallScheme::Call),
    );
    match inspector.finish_capture(&[]) {
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::FrameStackUnbalanced { remaining },
        } => assert_eq!(remaining, 1),
        other => panic!("expected an unbalanced frame stack, got {other:?}"),
    }
}

#[test]
fn exhausting_the_frame_stack_bound_fails_closed() {
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        usize::MAX,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    inspector.start_capture();
    for _ in 0..=MAX_OPEN_FRAMES {
        inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    }
    match inspector.finish_capture(&[]) {
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::FrameStackOverflow { depth },
        } => assert_eq!(depth, MAX_OPEN_FRAMES),
        other => panic!("expected a frame-stack overflow, got {other:?}"),
    }
}

#[test]
fn a_staged_candidate_is_discarded_when_its_frame_never_executes() {
    // The stage/commit/discard discipline, exercised directly: entering a frame allocates no
    // observation, and leaving it without a confirmed execution leaves none behind.
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        16,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    inspector.start_capture();
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    assert!(inspector.observations.is_empty(), "staging allocates no observation");
    assert_eq!(inspector.callstack_observed_count, 0, "staging consumes no budget");
    inspector.exit_frame();
    assert!(observations(inspector.finish_capture(&[])).is_empty());

    // Whereas a confirmed execution commits exactly one.
    inspector.start_capture();
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    inspector.confirm_frame_execution(B256::repeat_byte(0xaa));
    inspector.exit_frame();
    assert_eq!(
        observations(inspector.finish_capture(&[])),
        vec![CallObservation::call(selected, B256::repeat_byte(0xaa))]
    );
}

#[test]
fn capture_state_does_not_leak_between_transactions() {
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        1,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    // First window overflows its observation budget.
    inspector.start_capture();
    for _ in 0..2 {
        inspector.enter_frame(selected, selected, Some(CallScheme::Call));
        inspector.confirm_frame_execution(B256::repeat_byte(0xaa));
        inspector.exit_frame();
    }
    assert!(matches!(
        inspector.finish_capture(&[]),
        CaptureOutcome::LimitExceeded { budget: ObservationBudget::Callstack, .. }
    ));
    // The next window starts clean: the sticky overflow and the counter are both reset.
    inspector.start_capture();
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    inspector.confirm_frame_execution(B256::repeat_byte(0xbb));
    inspector.exit_frame();
    assert_eq!(
        observations(inspector.finish_capture(&[])),
        vec![CallObservation::call(selected, B256::repeat_byte(0xbb))]
    );
}

#[test]
fn hooks_are_inert_until_capture_is_armed() {
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        16,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    // Not armed: a gasless pre-check or any non-candidate simulation must capture nothing.
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    inspector.confirm_frame_execution(B256::repeat_byte(0xaa));
    inspector.exit_frame();
    assert!(inspector.frames.is_empty());
    assert!(inspector.observations.is_empty());
    assert_eq!(inspector.callstack_observed_count, 0);
}

#[test]
fn aborting_a_capture_clears_the_observation_state() {
    let selected = Address::repeat_byte(0x11);
    let mut inspector = RcsInspector::with_callstack(
        1_000,
        16,
        CallstackCaptureConfig::new(stamp(), vec![selected]),
    );
    inspector.start_capture();
    inspector.enter_frame(selected, selected, Some(CallScheme::Call));
    inspector.confirm_frame_execution(B256::repeat_byte(0xaa));
    inspector.abort_capture();
    assert!(inspector.observations.is_empty());
    assert!(inspector.frames.is_empty());
    assert_eq!(inspector.callstack_observed_count, 0);
}
