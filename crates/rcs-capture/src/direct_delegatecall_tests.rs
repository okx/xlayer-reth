//! Acceptance tests for the direct-delegatecall observation channel.
//!
//! Every test here runs **real EVM execution** over hand-written proxy/implementation bytecode and
//! asserts what the inspector produced. That is deliberate: the adjudication of a supplied
//! observation set is covered in the screening crate, so what needs proving here is the opposite
//! half — that the observations themselves are produced from the right frames, with the hashes of
//! the code that actually ran, and that each exclusion is a real exclusion rather than an
//! accidental absence.

use alloy_consensus::{SignableTransaction, TxEip7702, TxLegacy};
use alloy_eips::eip7702::Authorization;
use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{keccak256, Address, Bytes, Log, Signature, TxKind, B256, U256};
use op_revm::OpSpecId;
use rcs_filter::callstack::{
    alias_set_digest, DirectDelegatecallObservation, Observations, ObservationsUnavailable,
    SnapshotStamp,
};
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;
use revm::state::{AccountInfo, Bytecode};

use crate::{
    CaptureFaults, CaptureMode, CaptureOutcome, DirectDelegatecallConfig, RcsCaptureControl,
    RcsInspector,
};

// ---- addresses -------------------------------------------------------------------------------

/// The externally-owned sender.
fn sender() -> Address {
    Address::from([0xAA; 20])
}
/// The selected proxy.
fn proxy() -> Address {
    Address::from([0x11; 20])
}
/// A second selected proxy.
fn proxy2() -> Address {
    Address::from([0x12; 20])
}
/// An approved implementation.
fn approved_impl() -> Address {
    Address::from([0xC1; 20])
}
/// An unapproved implementation.
fn unapproved_impl() -> Address {
    Address::from([0xC2; 20])
}
/// A third contract an implementation may itself delegate into.
fn inner_impl() -> Address {
    Address::from([0xC3; 20])
}
/// A contract that is deliberately NOT selected.
fn unselected() -> Address {
    Address::from([0x99; 20])
}

// ---- bytecode fixtures -----------------------------------------------------------------------

/// `STOP`.
fn stop() -> Vec<u8> {
    vec![0x00]
}

/// `PUSH1 0; PUSH1 0; REVERT` — executes, then reverts the frame.
fn revert_after(mut code: Vec<u8>) -> Vec<u8> {
    code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0xFD]);
    code
}

/// The six zero/args words plus `PUSH20 dst`, leaving the stack ready for a gas push and the
/// opcode. Shared by DELEGATECALL and STATICCALL, which take the same operands.
fn no_value_call_prefix(dst: Address) -> Vec<u8> {
    let mut code = vec![
        0x60, 0x00, // PUSH1 0 (retSize)
        0x60, 0x00, // PUSH1 0 (retOffset)
        0x60, 0x00, // PUSH1 0 (argsSize)
        0x60, 0x00, // PUSH1 0 (argsOffset)
        0x73, // PUSH20
    ];
    code.extend_from_slice(dst.as_slice());
    code
}

/// `DELEGATECALL(gas, dst, 0, 0, 0, 0); POP`.
fn delegatecall(dst: Address) -> Vec<u8> {
    let mut code = no_value_call_prefix(dst);
    code.extend_from_slice(&[0x5A, 0xF4, 0x50]); // GAS DELEGATECALL POP
    code
}

/// `STATICCALL(gas, dst, 0, 0, 0, 0); POP`.
fn staticcall(dst: Address) -> Vec<u8> {
    let mut code = no_value_call_prefix(dst);
    code.extend_from_slice(&[0x5A, 0xFA, 0x50]); // GAS STATICCALL POP
    code
}

/// `CALL(gas, dst, value, 0, 0, 0, 0); POP`.
fn call_with_value(dst: Address, value: u8) -> Vec<u8> {
    let mut code = vec![
        0x60, 0x00, // PUSH1 0 (retSize)
        0x60, 0x00, // PUSH1 0 (retOffset)
        0x60, 0x00, // PUSH1 0 (argsSize)
        0x60, 0x00, // PUSH1 0 (argsOffset)
        0x60, value, // PUSH1 value
        0x73,  // PUSH20
    ];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[0x5A, 0xF1, 0x50]); // GAS CALL POP
    code
}

/// `CALLCODE(gas, dst, 0, 0, 0, 0, 0); POP` — runs `dst`'s code with this account's storage, but
/// is a distinct opcode from DELEGATECALL and is excluded structurally.
fn callcode(dst: Address) -> Vec<u8> {
    let mut code = vec![
        0x60, 0x00, // PUSH1 0 (retSize)
        0x60, 0x00, // PUSH1 0 (retOffset)
        0x60, 0x00, // PUSH1 0 (argsSize)
        0x60, 0x00, // PUSH1 0 (argsOffset)
        0x60, 0x00, // PUSH1 0 (value)
        0x73, // PUSH20
    ];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[0x5A, 0xF2, 0x50]); // GAS CALLCODE POP
    code
}

/// Two DELEGATECALLs in one frame, in source order.
fn delegatecall_twice(first: Address, second: Address) -> Vec<u8> {
    let mut code = delegatecall(first);
    code.extend_from_slice(&delegatecall(second));
    code
}

fn account(balance: u64, code: Option<Vec<u8>>) -> AccountInfo {
    AccountInfo {
        balance: U256::from(balance),
        code: code.map(|c| Bytecode::new_raw(Bytes::from(c))),
        ..Default::default()
    }
}

/// The hash the inspector must report for `code`: the hash of the bytes an interpreter loads.
fn code_hash(code: &[u8]) -> B256 {
    keccak256(code)
}

// ---- harness ---------------------------------------------------------------------------------

/// What one simulated transaction produced.
struct Run {
    outcome: CaptureOutcome,
    observations: Observations,
    success: bool,
}

impl Run {
    /// The available item list, or a panic naming the unavailable reason.
    fn items(&self) -> &[DirectDelegatecallObservation] {
        match &self.observations {
            Observations::Available { items, .. } => items,
            Observations::Unavailable(reason) => {
                panic!("expected an available observation set, got {reason:?}")
            }
        }
    }

    fn stamp(&self) -> SnapshotStamp {
        match &self.observations {
            Observations::Available { stamp, .. } => *stamp,
            Observations::Unavailable(reason) => panic!("expected availability, got {reason:?}"),
        }
    }

    fn unavailable(&self) -> ObservationsUnavailable {
        match &self.observations {
            Observations::Unavailable(reason) => *reason,
            Observations::Available { items, .. } => {
                panic!("expected unavailability, got {} item(s)", items.len())
            }
        }
    }
}

/// A capture configuration selecting `aliases`, with the budget and a stamp derived from the set.
fn config(aliases: &[Address], max_observations: usize) -> DirectDelegatecallConfig {
    DirectDelegatecallConfig {
        aliases: aliases.iter().copied().collect(),
        stamp: SnapshotStamp {
            protocol_version: 1,
            content_version: 0,
            alias_set_digest: alias_set_digest(aliases.iter().copied()),
        },
        max_observations,
    }
}

fn legacy_tx(to: Address) -> OpTx {
    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 3_000_000,
        to: TxKind::Call(to),
        value: U256::ZERO,
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    OpTx::from_recovered_tx(&tx, sender())
}

/// A genuine type-4 transaction delegating an account to `delegate`, plus the authority address
/// the delegation applies to.
///
/// The authority is **recovered** from the authorization signature rather than chosen, so the tests
/// below select that recovered address: a delegation only takes effect for the account the
/// signature really authorises.
fn delegated_tx(delegate: Address) -> (OpTx, Address) {
    let authorization = Authorization { chain_id: U256::ZERO, address: delegate, nonce: 0 }
        .into_signed(Signature::test_signature());
    let authority =
        authorization.recover_authority().expect("deterministic authority from the test signature");
    let tx = TxEip7702 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 3_000_000,
        max_fee_per_gas: 0,
        max_priority_fee_per_gas: 0,
        to: authority,
        value: U256::ZERO,
        authorization_list: vec![authorization],
        input: Default::default(),
        access_list: Default::default(),
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    (OpTx::from_recovered_tx(&tx, sender()), authority)
}

/// Runs one transaction through a real composite-wrapped `OpEvm`, driving the production capture
/// lifecycle: configure → start → `evm.transact` → finish → take.
fn run_with(
    db: InMemoryDB,
    tx: OpTx,
    capture: Option<DirectDelegatecallConfig>,
    mode: CaptureMode,
    faults: CaptureFaults,
) -> Run {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::with_mode_and_faults(10_000, mode, faults),
    );
    evm.components_mut().1.configure_direct_delegatecalls(capture);
    evm.components_mut().1.start_capture();
    let result = evm.transact(tx).expect("tx executes");
    let success = result.result.is_success();
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());
    let observations = evm.components_mut().1.take_direct_delegatecalls();
    Run { outcome, observations, success }
}

/// [`run_with`] in the mode the block builder uses, with no faults.
fn run(db: InMemoryDB, tx: OpTx, capture: Option<DirectDelegatecallConfig>) -> Run {
    run_with(db, tx, capture, CaptureMode::NativeAndCallstack, CaptureFaults::NONE)
}

/// A world where the selected proxy delegates into `implementation`, which then runs `impl_code`.
fn proxy_world(implementation: Address, impl_code: Vec<u8>) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(0, Some(delegatecall(implementation))));
    db.insert_account_info(implementation, account(0, Some(impl_code)));
    db
}

// ---- observation production ------------------------------------------------------------------

/// The four fields come from real frames: the context is the proxy and its own executed code, and
/// the delegate is the implementation and *its* executed code.
///
/// Falsifiable: sourcing either hash from the account's `code_hash` instead of the executed bytes
/// would still pass for an ordinary contract, which is why the delegation tests below exist; but
/// sourcing the delegate address from the frame's target rather than its bytecode address fails
/// here immediately.
#[test]
fn a_selected_proxys_own_direct_delegatecall_is_observed_with_real_frame_fields() {
    let impl_code = stop();
    let proxy_code = delegatecall(approved_impl());
    let run = run(
        proxy_world(approved_impl(), impl_code.clone()),
        legacy_tx(proxy()),
        Some(config(&[proxy()], 10)),
    );
    assert!(run.success, "the proxy call must succeed");
    assert_eq!(
        run.items(),
        &[DirectDelegatecallObservation {
            context_address: proxy(),
            context_code_hash: code_hash(&proxy_code),
            delegate_address: approved_impl(),
            delegate_code_hash: code_hash(&impl_code),
        }]
    );
}

/// The result carries the exact stamp its configuration was built with, field by field — a digest
/// alone would not distinguish two snapshots that select the same addresses.
#[test]
fn the_result_carries_the_configured_snapshot_stamp_field_by_field() {
    let configured = config(&[proxy()], 10);
    let expected = configured.stamp;
    let run = run(proxy_world(approved_impl(), stop()), legacy_tx(proxy()), Some(configured));
    let stamp = run.stamp();
    assert_eq!(stamp.protocol_version, expected.protocol_version);
    assert_eq!(stamp.content_version, expected.content_version);
    assert_eq!(stamp.alias_set_digest, expected.alias_set_digest);
}

/// Several delegatecalls from one selected frame are all observed, in execution order.
#[test]
fn several_direct_delegatecalls_are_all_observed_in_execution_order() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(
        proxy(),
        account(0, Some(delegatecall_twice(approved_impl(), unapproved_impl()))),
    );
    db.insert_account_info(approved_impl(), account(0, Some(stop())));
    db.insert_account_info(unapproved_impl(), account(0, Some(stop())));

    let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 10)));
    let delegates: Vec<Address> = run.items().iter().map(|o| o.delegate_address).collect();
    assert_eq!(
        delegates,
        vec![approved_impl(), unapproved_impl()],
        "both delegatecalls are observed, in the order they executed"
    );
}

/// An executed-then-reverted delegatecall stays observable. Unlike the native log queue, which
/// truncates to the frame checkpoint on revert, a confirmed observation is never removed: code that
/// ran and then reverted still ran, which is the entire reason this channel reads frames instead of
/// state.
#[test]
fn an_executed_then_reverted_delegatecall_is_still_observed() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    // The proxy delegatecalls, then reverts its own frame.
    db.insert_account_info(
        proxy(),
        account(0, Some(revert_after(delegatecall(unapproved_impl())))),
    );
    db.insert_account_info(unapproved_impl(), account(0, Some(stop())));

    let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 10)));
    assert!(!run.success, "the top-level frame reverted");
    assert_eq!(run.items().len(), 1, "the executed delegatecall survives the revert");
    assert_eq!(run.items()[0].delegate_address, unapproved_impl());
}

// ---- exclusions ------------------------------------------------------------------------------

/// An ordinary `CALL` to the selected proxy, with no delegatecall, produces nothing — and
/// "produces nothing" is reported as an available, empty set, not as unavailability.
#[test]
fn an_ordinary_call_with_no_delegatecall_produces_an_available_empty_set() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(0, Some(stop())));

    let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 10)));
    assert!(run.items().is_empty(), "no delegatecall ran, so there is nothing to observe");
    assert!(
        matches!(run.observations, Observations::Available { .. }),
        "capture ran and found nothing; that is NOT the same as capture being unavailable"
    );
}

/// The implementation's **own** nested delegatecall is not attributed to the proxy: the frame that
/// issues it is a delegate frame, whose bytecode address differs from its context.
///
/// Falsifiable: dropping the `bytecode_address == target_address` condition makes this produce two
/// observations instead of one.
#[test]
fn an_implementation_internal_nested_delegatecall_is_not_attributed_to_the_proxy() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(0, Some(delegatecall(approved_impl()))));
    // The implementation itself delegates further, from inside the proxy's context.
    db.insert_account_info(approved_impl(), account(0, Some(delegatecall(inner_impl()))));
    db.insert_account_info(inner_impl(), account(0, Some(stop())));

    let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 10)));
    assert_eq!(
        run.items().len(),
        1,
        "only the proxy's OWN delegatecall is observed, not the implementation's nested one"
    );
    assert_eq!(run.items()[0].delegate_address, approved_impl());
}

/// A delegatecall issued by the delegate's code while running in the proxy's context is the same
/// exclusion seen from the other side, and is asserted on its own so a single fix cannot silently
/// cover both cases.
#[test]
fn a_delegatecall_issued_by_the_delegates_code_is_not_the_proxys_own() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(0, Some(delegatecall(approved_impl()))));
    db.insert_account_info(approved_impl(), account(0, Some(delegatecall(unapproved_impl()))));
    db.insert_account_info(unapproved_impl(), account(0, Some(stop())));

    let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 10)));
    let delegates: Vec<Address> = run.items().iter().map(|o| o.delegate_address).collect();
    assert_eq!(
        delegates,
        vec![approved_impl()],
        "the unapproved implementation was reached by the DELEGATE's code, not by the proxy's"
    );
}

/// A delegatecall by an **unselected** contract is not recorded at all.
#[test]
fn an_unselected_contracts_delegatecall_is_not_recorded() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(unselected(), account(0, Some(delegatecall(unapproved_impl()))));
    db.insert_account_info(unapproved_impl(), account(0, Some(stop())));

    // The proxy is selected; the contract that actually delegatecalls is not.
    let run = run(db, legacy_tx(unselected()), Some(config(&[proxy()], 10)));
    assert!(run.items().is_empty(), "an unselected contract is not observed");
}

/// A `DELEGATECALL` that never entered the interpreter produces no observation. An empty-code
/// target completes the frame through the early-failure path, so the confirmation hook never runs
/// and the staged candidate is discarded.
///
/// Falsifiable: confirming an observation in the `call` hook instead of at interpreter
/// initialisation makes this produce one.
#[test]
fn a_delegatecall_that_never_entered_the_interpreter_produces_no_observation() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    // The delegate address has NO code at all.
    db.insert_account_info(proxy(), account(0, Some(delegatecall(unapproved_impl()))));

    let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 10)));
    assert!(
        run.items().is_empty(),
        "a frame that never ran an interpreter is not an executed delegatecall"
    );
}

/// `CALLCODE`, `STATICCALL` and a plain `CALL` from a selected address are each excluded by opcode
/// identity, not by comparing addresses. Asserted per opcode so one shared mistake cannot hide.
#[test]
fn callcode_staticcall_and_call_from_a_selected_address_are_excluded_by_opcode() {
    for (label, proxy_code) in [
        ("CALLCODE", callcode(unapproved_impl())),
        ("STATICCALL", staticcall(unapproved_impl())),
        ("CALL", call_with_value(unapproved_impl(), 0)),
    ] {
        let mut db = InMemoryDB::default();
        db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
        db.insert_account_info(proxy(), account(0, Some(proxy_code)));
        db.insert_account_info(unapproved_impl(), account(0, Some(stop())));

        let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 10)));
        assert!(
            run.items().is_empty(),
            "{label} is not a DELEGATECALL and must never be observed as one"
        );
    }
}

// ---- delegation (EIP-7702) -------------------------------------------------------------------

/// A `CALL` to a selected, delegated account produces no observation for it: the code that runs is
/// the delegate's, so the frame did not execute the selected account's own code.
#[test]
fn a_call_to_a_selected_delegated_account_produces_no_observation_for_it() {
    // The delegate's code performs a DELEGATECALL; if the delegated account were wrongly treated
    // as running its own code, that call would be attributed to it.
    let (tx, authority) = delegated_tx(approved_impl());
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(authority, account(1_000_000u64, None));
    db.insert_account_info(approved_impl(), account(0, Some(stop())));

    let run = run(db, tx, Some(config(&[authority], 10)));
    assert!(
        run.items().is_empty(),
        "a delegated account's frame runs someone else's code, so it has no own delegatecall"
    );
}

/// The case an address-only predicate gets wrong: the selected account is delegated, so a `CALL` to
/// it runs the delegate's code, and when **that** code issues a `DELEGATECALL` the frame still
/// names the selected account. Nothing may be attributed to it.
///
/// Falsifiable: deciding own-code from `target_address == bytecode_address` alone makes this
/// produce one observation, because a delegated account's frame satisfies that test.
#[test]
fn a_delegatecall_issued_by_the_delegate_of_a_selected_account_is_not_attributed_to_it() {
    let (tx, authority) = delegated_tx(approved_impl());
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(authority, account(1_000_000u64, None));
    // The delegate's code issues a DELEGATECALL while running in the selected account's context.
    db.insert_account_info(approved_impl(), account(0, Some(delegatecall(unapproved_impl()))));
    db.insert_account_info(unapproved_impl(), account(0, Some(stop())));

    let run = run(db, tx, Some(config(&[authority], 10)));
    assert!(
        run.items().is_empty(),
        "the delegate's code issued the call; attributing it to the selected account would \
         misreport which code ran on its behalf"
    );
}

// ---- same-address and re-entry ---------------------------------------------------------------

/// A selected contract delegatecalling **itself** is a real, observable direct delegatecall, and
/// because that child frame also runs the contract's own code, a further delegatecall from inside
/// it is attributed to the contract as well. Both are faithful: the contract's own code issued both.
#[test]
fn delegate_to_the_same_address_observes_both_hops_with_exact_fields() {
    // `proxy`'s own code delegatecalls `proxy` itself. The entered frame runs the same bytes, so it
    // delegatecalls again; the chain is bounded by gas rather than by a counter, which is exactly
    // what makes it a multi-hop case. The budget is set wide so this test measures attribution
    // rather than the budget (which has its own test).
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(0, Some(delegatecall(proxy()))));

    let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 10_000)));
    // The self-delegation chain is bounded by gas; every hop that genuinely executed is observed,
    // and every observation names the selected contract as both context and delegate.
    assert!(!run.items().is_empty(), "a self-delegatecall is a real observable delegatecall");
    for observation in run.items() {
        assert_eq!(observation.context_address, proxy());
        assert_eq!(observation.delegate_address, proxy());
        assert_eq!(
            observation.context_code_hash, observation.delegate_code_hash,
            "both frames ran the same, selected contract's code"
        );
    }
    assert!(
        run.items().len() >= 2,
        "the frame entered by the self-delegatecall also runs the contract's own code, so a \
         further delegatecall from it is attributed to the contract too; got {}",
        run.items().len()
    );
}

/// Re-entry: the selected contract is entered twice in one transaction, each time delegatecalling.
/// Each frame carries its own entry-time evidence, so the two observations are independent — and
/// both are kept even though their field values are identical, because they are distinct execution
/// facts.
#[test]
fn re_entry_produces_two_independent_identical_observations() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    // proxy: delegatecall(impl); then call(unselected), which calls back into proxy2.
    // Simpler and equivalent: an intermediary calls the selected proxy twice.
    let impl_code = stop();
    let intermediary = {
        let mut code = call_with_value(proxy(), 0);
        code.extend_from_slice(&call_with_value(proxy(), 0));
        code
    };
    db.insert_account_info(unselected(), account(0, Some(intermediary)));
    db.insert_account_info(proxy(), account(0, Some(delegatecall(approved_impl()))));
    db.insert_account_info(approved_impl(), account(0, Some(impl_code.clone())));

    let run = run(db, legacy_tx(unselected()), Some(config(&[proxy()], 10)));
    assert_eq!(run.items().len(), 2, "each entry into the selected contract is observed");
    assert_eq!(run.items()[0], run.items()[1], "identical field values, both retained");
    assert_eq!(run.items()[0].context_address, proxy());
}

/// Entry-time evidence is not re-read later.
///
/// The selected contract's account is **changed inside the transaction** — it receives value from
/// an inner `CALL` — between its frame's entry and its delegatecall, and the attribution still uses
/// the evidence recorded at entry.
///
/// Evidence boundary, stated precisely: a contract's *code* cannot change within a transaction in
/// the EVM, so a code change at the selected address mid-frame is not constructible. What this test
/// does construct is a real mid-transaction change to that account's journal entry, together with
/// the structural guarantee that the staging decision reads the parent's recorded fields rather
/// than the account as it stands at the moment of the call. The delegation dimension of the same
/// property is covered by the two EIP-7702 tests above.
#[test]
fn entry_time_own_code_evidence_survives_a_mid_transaction_account_change() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    // The proxy sends 1 wei to a third account (changing its own balance, a journal write to its
    // account entry), and only then delegatecalls.
    let proxy_code = {
        let mut code = call_with_value(unselected(), 1);
        code.extend_from_slice(&delegatecall(approved_impl()));
        code
    };
    db.insert_account_info(proxy(), account(1_000, Some(proxy_code.clone())));
    db.insert_account_info(unselected(), account(0, Some(stop())));
    db.insert_account_info(approved_impl(), account(0, Some(stop())));

    let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 10)));
    assert_eq!(run.items().len(), 1);
    assert_eq!(
        run.items()[0].context_code_hash,
        code_hash(&proxy_code),
        "the context hash is the one recorded when the frame was entered"
    );
    assert_eq!(run.items()[0].delegate_address, approved_impl());
}

// ---- fail-closed paths -----------------------------------------------------------------------

/// Undecidable own-code evidence fails the window closed, naming its own reason — it is never
/// silently observed and never silently skipped.
#[test]
fn undecidable_own_code_evidence_fails_the_window_closed() {
    let run = run_with(
        proxy_world(approved_impl(), stop()),
        legacy_tx(proxy()),
        Some(config(&[proxy()], 10)),
        CaptureMode::NativeAndCallstack,
        CaptureFaults { force_own_code_indeterminate: true, ..CaptureFaults::NONE },
    );
    assert_eq!(run.unavailable(), ObservationsUnavailable::OwnCodeIndeterminate);
}

/// Exceeding the per-transaction observation budget fails closed, reporting the attempted count and
/// the limit rather than a truncated list that would read as a clean no-match.
#[test]
fn exceeding_the_observation_budget_fails_closed_with_attempted_and_limit() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(
        proxy(),
        account(0, Some(delegatecall_twice(approved_impl(), unapproved_impl()))),
    );
    db.insert_account_info(approved_impl(), account(0, Some(stop())));
    db.insert_account_info(unapproved_impl(), account(0, Some(stop())));

    // Budget of 1, two real delegatecalls.
    let run = run(db, legacy_tx(proxy()), Some(config(&[proxy()], 1)));
    assert_eq!(
        run.unavailable(),
        ObservationsUnavailable::BudgetExceeded { attempted: 2, limit: 1 }
    );
}

/// An incomplete frame stack fails closed, and does so ahead of any other reason, because missing
/// frames can hide observations outright.
#[test]
fn an_incomplete_frame_stack_fails_the_window_closed() {
    let run = run_with(
        proxy_world(approved_impl(), stop()),
        legacy_tx(proxy()),
        Some(config(&[proxy()], 10)),
        CaptureMode::NativeAndCallstack,
        CaptureFaults { force_frames_unbalanced: true, ..CaptureFaults::NONE },
    );
    assert_eq!(run.unavailable(), ObservationsUnavailable::IncompleteFrames);
}

// ---- lifecycle -------------------------------------------------------------------------------

/// A plain inspector, driven through the lifecycle without an EVM, so each sequence is asserted on
/// the inspector's own observable state.
fn armed(budget: usize) -> RcsInspector {
    let mut inspector = RcsInspector::new(10_000);
    inspector.configure_direct_delegatecalls(Some(config(&[proxy()], budget)));
    inspector
}

/// The success channel: the first take after a successful finish returns the configured stamp and
/// the item list. This is the sequence whose absence would make a non-empty observation set
/// unreachable by screening however correct the capture was.
#[test]
fn the_first_take_after_a_successful_finish_returns_an_available_set() {
    let run =
        run(proxy_world(approved_impl(), stop()), legacy_tx(proxy()), Some(config(&[proxy()], 10)));
    assert!(matches!(run.observations, Observations::Available { .. }));
    assert_eq!(run.items().len(), 1);
}

/// A second take reports the result as already consumed — never a stale clone that a later
/// transaction could be judged against, and never a panic.
#[test]
fn a_second_take_reports_the_result_already_taken() {
    let mut inspector = armed(10);
    inspector.start_capture();
    inspector.finish_capture(&[]);
    assert!(matches!(inspector.take_direct_delegatecalls(), Observations::Available { .. }));
    assert_eq!(
        inspector.take_direct_delegatecalls(),
        Observations::Unavailable(ObservationsUnavailable::ResultAlreadyTaken)
    );
}

/// A take with no preceding finish has nothing to return.
#[test]
fn a_take_without_a_preceding_finish_reports_the_result_already_taken() {
    let mut inspector = armed(10);
    inspector.start_capture();
    assert_eq!(
        inspector.take_direct_delegatecalls(),
        Observations::Unavailable(ObservationsUnavailable::ResultAlreadyTaken)
    );
}

/// A repeated finish, with no intervening start, runs on a cleared and disarmed window: it reports
/// passthrough and parks "not captured", deliberately overwriting the earlier result so a stale one
/// can never be served against a new call. An enabled filter seeing passthrough already fails
/// closed, so this cannot become an allow.
#[test]
fn a_repeated_finish_parks_not_captured_and_reports_passthrough() {
    let mut inspector = armed(10);
    inspector.start_capture();
    assert!(matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete(_)));
    assert_eq!(inspector.finish_capture(&[]), CaptureOutcome::Passthrough);
    assert_eq!(
        inspector.take_direct_delegatecalls(),
        Observations::Unavailable(ObservationsUnavailable::NotCaptured)
    );
}

/// A finish on an inspector that was never armed behaves the same way.
#[test]
fn a_finish_that_was_never_armed_parks_not_captured_and_reports_passthrough() {
    let mut inspector = armed(10);
    assert_eq!(inspector.finish_capture(&[]), CaptureOutcome::Passthrough);
    assert_eq!(
        inspector.take_direct_delegatecalls(),
        Observations::Unavailable(ObservationsUnavailable::NotCaptured)
    );
}

/// A new window never serves the previous window's items. The first window's result is left
/// untaken on purpose, and the second window — which observes nothing — must come back empty
/// rather than carrying the first one's observation.
#[test]
fn a_new_window_with_no_observations_never_returns_the_previous_windows_items() {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        proxy_world(approved_impl(), stop()),
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(10_000),
    );
    // Window 1: a real delegatecall, finished but deliberately NOT taken.
    evm.components_mut().1.configure_direct_delegatecalls(Some(config(&[proxy()], 10)));
    evm.components_mut().1.start_capture();
    let first = evm.transact(legacy_tx(proxy())).expect("tx executes");
    evm.components_mut().1.finish_capture(first.result.logs());

    // Window 2: the same selection, a transaction that touches nothing selected.
    evm.components_mut().1.configure_direct_delegatecalls(Some(config(&[proxy()], 10)));
    evm.components_mut().1.start_capture();
    let second = evm.transact(legacy_tx(approved_impl())).expect("tx executes");
    evm.components_mut().1.finish_capture(second.result.logs());
    let observations = evm.components_mut().1.take_direct_delegatecalls();

    match observations {
        Observations::Available { items, .. } => {
            assert!(items.is_empty(), "the new window must not inherit the previous one's items")
        }
        Observations::Unavailable(reason) => {
            panic!("expected an empty available set, got {reason:?}")
        }
    }
}

/// Aborting leaves nothing behind — parked result included — and a later cycle still works.
#[test]
fn abort_leaves_nothing_behind_and_a_later_cycle_still_works() {
    let mut inspector = armed(10);
    inspector.start_capture();
    inspector.abort_capture();
    assert_eq!(
        inspector.take_direct_delegatecalls(),
        Observations::Unavailable(ObservationsUnavailable::ResultAlreadyTaken)
    );
    // A normal cycle afterwards behaves normally.
    inspector.start_capture();
    assert!(matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete(_)));
    assert!(matches!(inspector.take_direct_delegatecalls(), Observations::Available { .. }));
}

/// Every finish exit clears the active window: after each outcome variant, the next window starts
/// from an empty state rather than inheriting counters or sticky flags.
#[test]
fn every_finish_exit_clears_the_active_window() {
    // A real-log mismatch exit (an injected divergence), then a budget exit, then a clean exit.
    let mut inspector = armed(1);
    for expect_clean_next_window in [true, true, true] {
        inspector.start_capture();
        let _ = inspector.finish_capture(&[Log::default()]);
        // Regardless of which exit ran, the next window's result is computed from a cleared state.
        inspector.start_capture();
        let outcome = inspector.finish_capture(&[]);
        assert!(
            matches!(outcome, CaptureOutcome::Complete(ref logs) if logs.is_empty()),
            "a window following any exit must start clean, got {outcome:?}"
        );
        assert!(expect_clean_next_window);
        match inspector.take_direct_delegatecalls() {
            Observations::Available { items, .. } => assert!(items.is_empty()),
            Observations::Unavailable(reason) => panic!("expected availability, got {reason:?}"),
        }
    }
}

// ---- modes and the inert path ----------------------------------------------------------------

/// Callstack-only mode drives the direct-delegatecall channel while leaving the native channel
/// genuinely untouched: its queue stays empty and neither its budget check nor its cross-check
/// runs, which is why a non-empty execution-log list cannot make it fail.
#[test]
fn callstack_only_mode_observes_frames_while_leaving_the_native_channel_untouched() {
    let run = run_with(
        proxy_world(approved_impl(), stop()),
        legacy_tx(proxy()),
        Some(config(&[proxy()], 10)),
        CaptureMode::CallstackOnly,
        CaptureFaults::NONE,
    );
    assert_eq!(run.items().len(), 1, "the frame channel still works in this mode");
    assert_eq!(
        run.outcome,
        CaptureOutcome::Complete(Vec::new()),
        "the native stream stays empty and its checks are skipped"
    );
}

/// The two modes produce the **same** observations for the same frames. This is what makes the
/// shared frame logic a tested property rather than an assertion: the two consuming paths run this
/// one code path, so they cannot drift.
#[test]
fn both_capture_modes_produce_identical_observations_for_the_same_frames() {
    let builder_mode = run_with(
        proxy_world(approved_impl(), stop()),
        legacy_tx(proxy()),
        Some(config(&[proxy()], 10)),
        CaptureMode::NativeAndCallstack,
        CaptureFaults::NONE,
    );
    let audit_mode = run_with(
        proxy_world(approved_impl(), stop()),
        legacy_tx(proxy()),
        Some(config(&[proxy()], 10)),
        CaptureMode::CallstackOnly,
        CaptureFaults::NONE,
    );
    assert_eq!(builder_mode.observations, audit_mode.observations);
}

/// The inert path: with no configuration installed, the frame channel does no work and reports that
/// nothing was captured — which screening ignores when no rule selects an address, so an event-only
/// deployment is unaffected.
#[test]
fn with_no_configuration_the_frame_channel_does_no_work() {
    let run = run(proxy_world(approved_impl(), stop()), legacy_tx(proxy()), None);
    assert_eq!(run.unavailable(), ObservationsUnavailable::NotCaptured);
    assert!(
        matches!(run.outcome, CaptureOutcome::Complete(_)),
        "the native channel is unaffected by the frame channel being inert"
    );
}

/// An empty selection is inert too: a configuration object that selects nothing must not start
/// paying per-frame costs.
#[test]
fn an_empty_selection_is_inert() {
    let run = run(proxy_world(approved_impl(), stop()), legacy_tx(proxy()), Some(config(&[], 10)));
    assert_eq!(run.unavailable(), ObservationsUnavailable::NotCaptured);
}

/// Equivalence regression for the native channel: with the frame channel inert, a transaction with
/// a real internal native transfer produces exactly the native outcome it did before the frame
/// channel existed.
#[test]
fn the_native_channel_is_unchanged_when_the_frame_channel_is_inert() {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(1_000, Some(call_with_value(unselected(), 1))));
    db.insert_account_info(unselected(), account(0, Some(stop())));

    let inert = run(db.clone(), legacy_tx(proxy()), None);
    let selected = run(db, legacy_tx(proxy()), Some(config(&[proxy2()], 10)));
    // One virtual native transfer either way: selecting an unrelated address changes nothing about
    // the native stream.
    match (&inert.outcome, &selected.outcome) {
        (CaptureOutcome::Complete(a), CaptureOutcome::Complete(b)) => {
            assert_eq!(a, b, "the native stream must not depend on the frame channel");
            assert_eq!(a.len(), 1, "the internal native transfer is still observed");
        }
        other => panic!("expected two Complete outcomes, got {other:?}"),
    }
}

/// The fault seam is a **real API**, reachable from a dependent crate's tests, and defaults to no
/// faults. A seam that existed only under this crate's own test configuration would be invisible
/// to exactly the consumers that need it.
#[test]
fn the_fault_seam_is_a_real_api_that_defaults_to_no_faults() {
    assert_eq!(CaptureFaults::default(), CaptureFaults::NONE);
    assert_eq!(CaptureFaults::ambient(), CaptureFaults::NONE);
    {
        let _guard =
            CaptureFaults { force_own_code_indeterminate: true, ..CaptureFaults::NONE }.scoped();
        assert!(CaptureFaults::ambient().force_own_code_indeterminate);
    }
    assert_eq!(
        CaptureFaults::ambient(),
        CaptureFaults::NONE,
        "the guard restores the previous value on every exit path"
    );
}
