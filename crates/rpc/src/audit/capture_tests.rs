//! Deposit-path acceptance tests: real execution-frame capture feeding the deposit classifier.
//!
//! These exercise the deposit path's own half of the contract — that the inspector this handler
//! installs really produces observations, that the classifier turns them into the documented
//! verdicts, that the ordered state effects across a batch are preserved, and that the two
//! screening paths reach the **same** verdict from the same frames.
//!
//! What they deliberately do not do is stand up a provider backend and go through the JSON-RPC
//! method itself; the request-contract tests cover that layer, and the per-deposit loop's
//! execution, capture and classification steps are reproduced here against a real EVM.

use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{keccak256, Address, Bytes, Signature, TxKind, B256, U256};
use op_revm::OpSpecId;
use rcs_filter::callstack::{
    alias_set_digest, Observations, ObservationsUnavailable, SnapshotStamp,
};
use rcs_filter::rules::{load_rules_from_raw, RuleSet};
use revm::context::{BlockEnv, CfgEnv};
use revm::database::{DatabaseCommit, InMemoryDB};
use revm::state::{AccountInfo, Bytecode};
use xlayer_rcs_capture::{CaptureMode, DirectDelegatecallConfig, RcsCaptureControl, RcsInspector};

use super::deposit::DepositTxRequest;
use super::verdict::{verdict_for, SimulatedDeposit, Verdict};

fn sender() -> Address {
    Address::from([0xAA; 20])
}
/// The selected proxy. Lowercase `0x` 20-byte form, as a rule must declare it.
fn proxy() -> Address {
    Address::from([0x11; 20])
}
fn approved_impl() -> Address {
    Address::from([0xC1; 20])
}
fn unapproved_impl() -> Address {
    Address::from([0xC2; 20])
}

fn stop() -> Vec<u8> {
    vec![0x00]
}

/// `DELEGATECALL(gas, dst, 0, 0, 0, 0); POP; STOP`.
fn delegatecall(dst: Address) -> Vec<u8> {
    let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[0x5A, 0xF4, 0x50, 0x00]);
    code
}

/// `DELEGATECALL(gas, SLOAD(0), 0, 0, 0, 0); POP; STOP` — an upgradeable proxy whose delegate
/// target lives in storage, so a committed state change really moves it.
fn storage_proxy() -> Vec<u8> {
    vec![
        0x60, 0x00, // PUSH1 0 (retSize)
        0x60, 0x00, // PUSH1 0 (retOffset)
        0x60, 0x00, // PUSH1 0 (argsSize)
        0x60, 0x00, // PUSH1 0 (argsOffset)
        0x60, 0x00, // PUSH1 0 (slot)
        0x54, // SLOAD -> the delegate address
        0x5A, // GAS
        0xF4, // DELEGATECALL
        0x50, // POP
        0x00, // STOP
    ]
}

/// Runtime code that stores `next` into slot 0. Run through a DELEGATECALL it writes the **proxy's**
/// slot, which is how an upgrade happens in one transaction.
fn upgrader_to(next: Address) -> Vec<u8> {
    let mut code = vec![0x73];
    code.extend_from_slice(next.as_slice());
    code.extend_from_slice(&[0x60, 0x00, 0x55, 0x00]); // PUSH1 0 SSTORE STOP
    code
}

fn account(balance: u64, code: Option<Vec<u8>>) -> AccountInfo {
    AccountInfo {
        balance: U256::from(balance),
        code: code.map(|c| Bytecode::new_raw(Bytes::from(c))),
        ..Default::default()
    }
}

fn lower_hash(hash: B256) -> String {
    format!("0x{}", alloy_primitives::hex::encode(hash.as_slice()))
}

fn lower_address(address: Address) -> String {
    format!("0x{}", alloy_primitives::hex::encode(address.as_slice()))
}

/// A direct-delegatecall rule that denies anything whose delegate code hash is outside
/// `approved_hashes` — the canonical negated-allowlist shape.
fn guard_rules(approved_hashes: &[B256]) -> RuleSet {
    let allowlist = approved_hashes
        .iter()
        .map(|h| format!("\"{}\"", lower_hash(*h)))
        .collect::<Vec<_>>()
        .join(",");
    let rule = format!(
        r#"{{"id":"proxy-codehash-guard","event_abis":{{}},
            "callstack":{{"direct_delegatecalls":{{"proxy1":"{}"}}}},
            "condition":{{"!":{{"in":[{{"var":"proxy1.delegateCodeHash"}},[{allowlist}]]}}}},
            "action":"deny"}}"#,
        lower_address(proxy())
    );
    let raw: Box<serde_json::value::RawValue> =
        serde_json::from_str(&rule).expect("the rule is valid JSON");
    let set = load_rules_from_raw(1, 0, std::slice::from_ref(&raw));
    assert!(set.rejected.is_empty(), "fixture rule must compile: {:?}", set.rejected);
    set
}

fn config_for(rules: &RuleSet, budget: usize) -> Option<DirectDelegatecallConfig> {
    rules.declares_any_alias().then(|| DirectDelegatecallConfig {
        aliases: rules.alias_addresses().into_iter().collect(),
        stamp: rules.snapshot_stamp(),
        max_observations: budget,
    })
}

fn deposit_tx(to: Address, nonce: u64) -> OpTx {
    let tx = TxLegacy {
        nonce,
        gas_limit: 3_000_000,
        to: TxKind::Call(to),
        value: U256::ZERO,
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    OpTx::from_recovered_tx(&tx, sender())
}

fn request(source_hash: &str) -> DepositTxRequest {
    DepositTxRequest {
        source_hash: source_hash.to_string(),
        from: lower_address(sender()),
        to: Some(lower_address(proxy())),
        mint: "0".to_string(),
        value: "0".to_string(),
        gas_limit: 3_000_000,
        is_system_transaction: false,
        data: "0x".to_string(),
    }
}

/// Reproduces the handler's per-deposit steps against a real EVM: arm the window, execute, finish,
/// take, classify. Returns each deposit's verdict in request order, committing exactly where the
/// handler commits so a prior deposit's state really reaches the next one.
fn classify_batch(
    db: InMemoryDB,
    txs: Vec<OpTx>,
    rules: &RuleSet,
    mode: CaptureMode,
    known_allowed: &[&str],
) -> Vec<(Verdict, Observations)> {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::with_mode_and_faults(10_000, mode, Default::default()),
    );
    let capture_config = config_for(rules, 10_000);
    let mut out = Vec::new();
    for (index, tx) in txs.iter().enumerate() {
        {
            let inspector = evm.components_mut().1;
            inspector.configure_direct_delegatecalls(capture_config.clone());
            inspector.start_capture();
        }
        let executed = evm.transact(tx.clone()).expect("tx executes");
        let observations = {
            let inspector = evm.components_mut().1;
            inspector.finish_capture(executed.result.logs());
            inspector.take_direct_delegatecalls()
        };
        let source_hash = format!("0xdeadbeef{index:02x}");
        if known_allowed.contains(&source_hash.as_str()) {
            // The already-approved row: committed and allowed, with the capture result carried only
            // as a diagnostic.
            evm.db_mut().commit(executed.state);
            out.push((Verdict::Allow, observations));
            continue;
        }
        let req = request(&source_hash);
        let result = verdict_for(
            &req,
            &SimulatedDeposit {
                from: sender(),
                to: Some(proxy()),
                tx_hash: B256::ZERO,
                value: U256::ZERO,
                logs: executed.result.logs(),
                observations: &observations,
            },
            rules,
        );
        if matches!(result.verdict, Verdict::Allow) {
            evm.db_mut().commit(executed.state);
        }
        out.push((result.verdict, observations));
    }
    out
}

/// A world whose proxy delegatecalls `implementation` directly.
fn fixed_proxy_world(implementation: Address) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(0, Some(delegatecall(implementation))));
    db.insert_account_info(implementation, account(0, Some(stop())));
    db
}

/// A deposit whose proxy delegatecalls an **approved** implementation is allowed.
#[test]
fn a_deposit_executing_an_approved_implementation_is_allowed() {
    let rules = guard_rules(&[keccak256(stop())]);
    let verdicts = classify_batch(
        fixed_proxy_world(approved_impl()),
        vec![deposit_tx(proxy(), 0)],
        &rules,
        CaptureMode::CallstackOnly,
        &[],
    );
    assert_eq!(verdicts[0].0, Verdict::Allow);
    assert!(
        matches!(&verdicts[0].1, Observations::Available { items, .. } if items.len() == 1),
        "the deposit's own simulation produced the observation: {:?}",
        verdicts[0].1
    );
}

/// A deposit whose proxy delegatecalls an **unapproved** implementation is denied, and the deny
/// comes from the real frame rather than from anything read out of storage.
#[test]
fn a_deposit_executing_an_unapproved_implementation_is_denied() {
    // The allowlist holds a hash nothing in this world executes.
    let rules = guard_rules(&[keccak256([0xFEu8])]);
    let verdicts = classify_batch(
        fixed_proxy_world(unapproved_impl()),
        vec![deposit_tx(proxy(), 0)],
        &rules,
        CaptureMode::CallstackOnly,
        &[],
    );
    assert_eq!(verdicts[0].0, Verdict::Deny);
}

/// Ordered state effects across a batch: an earlier deposit upgrades the proxy's delegate target,
/// and the later deposit is judged against the **new**, real implementation.
///
/// This is the property that makes the batch ordered rather than independent — remove the commit
/// between deposits and the second one resolves the old target and comes back allowed.
#[test]
fn an_earlier_deposits_upgrade_is_visible_to_a_later_deposit_in_the_same_batch() {
    let upgrader_code = upgrader_to(unapproved_impl());
    // Only the upgrader's code is approved, so the first deposit is allowed and committed and the
    // second — which now reaches the unapproved implementation — is denied.
    let rules = guard_rules(&[keccak256(&upgrader_code)]);

    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(0, Some(storage_proxy())));
    db.insert_account_info(Address::from([0xD0; 20]), account(0, Some(upgrader_code)));
    db.insert_account_info(unapproved_impl(), account(0, Some(stop())));
    // Slot 0 initially points at the upgrader.
    db.insert_account_storage(
        proxy(),
        U256::ZERO,
        U256::from_be_slice(Address::from([0xD0; 20]).as_slice()),
    )
    .expect("storage insert");

    let verdicts = classify_batch(
        db,
        vec![deposit_tx(proxy(), 0), deposit_tx(proxy(), 1)],
        &rules,
        CaptureMode::CallstackOnly,
        &[],
    );
    assert_eq!(verdicts[0].0, Verdict::Allow, "the upgrade itself runs approved code");
    assert_eq!(
        verdicts[1].0,
        Verdict::Deny,
        "the later deposit must be judged against the implementation the upgrade installed"
    );
    // And the second deposit's observation names the new target, read from the real frame.
    match &verdicts[1].1 {
        Observations::Available { items, .. } => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].delegate_address, unapproved_impl());
        }
        other => panic!("expected an available set, got {other:?}"),
    }
}

/// The same rule against the same frames yields the **same** verdict on both paths. The two modes
/// are the only difference between them, and frame identification is shared code, so a divergence
/// here would mean the two paths had stopped agreeing.
#[test]
fn both_paths_reach_the_same_verdict_for_the_same_frames() {
    let rules = guard_rules(&[keccak256([0xFEu8])]);
    for implementation in [approved_impl(), unapproved_impl()] {
        let deposit_path = classify_batch(
            fixed_proxy_world(implementation),
            vec![deposit_tx(proxy(), 0)],
            &rules,
            CaptureMode::CallstackOnly,
            &[],
        );
        let builder_mode = classify_batch(
            fixed_proxy_world(implementation),
            vec![deposit_tx(proxy(), 0)],
            &rules,
            CaptureMode::NativeAndCallstack,
            &[],
        );
        assert_eq!(
            deposit_path[0].0, builder_mode[0].0,
            "the two paths must agree for implementation {implementation}"
        );
        assert_eq!(
            deposit_path[0].1, builder_mode[0].1,
            "and they must agree on the observations themselves"
        );
    }
}

/// An unavailable observation set is a **non-verdict** on this path, never an allow, and no state
/// is committed for it.
#[test]
fn an_unavailable_observation_set_is_a_non_verdict_never_an_allow() {
    let rules = guard_rules(&[keccak256(stop())]);
    let req = request("0xdeadbeef00");
    for reason in [
        ObservationsUnavailable::NotCaptured,
        ObservationsUnavailable::IncompleteFrames,
        ObservationsUnavailable::BudgetExceeded { attempted: 2, limit: 1 },
        ObservationsUnavailable::SnapshotMismatch,
        ObservationsUnavailable::OwnCodeIndeterminate,
        ObservationsUnavailable::ResultAlreadyTaken,
        ObservationsUnavailable::EngineError,
    ] {
        let observations = Observations::Unavailable(reason);
        let result = verdict_for(
            &req,
            &SimulatedDeposit {
                from: sender(),
                to: Some(proxy()),
                tx_hash: B256::ZERO,
                value: U256::ZERO,
                logs: &[],
                observations: &observations,
            },
            &rules,
        );
        assert_eq!(
            result.verdict,
            Verdict::Malformed,
            "{reason:?} must produce a non-verdict, never an allow"
        );
    }
}

/// A wrongly-stamped set is refused the same way: a set captured under one selection must never be
/// judged against another.
#[test]
fn a_wrongly_stamped_observation_set_is_refused() {
    let rules = guard_rules(&[keccak256(stop())]);
    let observations = Observations::Available {
        stamp: SnapshotStamp {
            protocol_version: 1,
            content_version: 0,
            // A digest of a different selection.
            alias_set_digest: alias_set_digest([unapproved_impl()]),
        },
        items: Vec::new(),
    };
    let req = request("0xdeadbeef00");
    let result = verdict_for(
        &req,
        &SimulatedDeposit {
            from: sender(),
            to: Some(proxy()),
            tx_hash: B256::ZERO,
            value: U256::ZERO,
            logs: &[],
            observations: &observations,
        },
        &rules,
    );
    assert_eq!(result.verdict, Verdict::Malformed);
}

/// The already-approved row, including its disclosed limitation.
///
/// An already-approved deposit is executed, allowed and committed **unchanged**, and the capture
/// result is carried as a diagnostic only. The consequence is stated rather than hidden: such a
/// deposit executing unapproved implementation code is **not** denied in this revision. The row
/// below asserts exactly that, so the limitation cannot be quietly "fixed" into a deny without a
/// deliberate decision — acting on it needs a caller-supplied class label this path does not have.
#[test]
fn an_already_approved_deposit_is_allowed_and_its_capture_result_is_only_a_diagnostic() {
    let rules = guard_rules(&[keccak256([0xFEu8])]);
    let verdicts = classify_batch(
        fixed_proxy_world(unapproved_impl()),
        vec![deposit_tx(proxy(), 0)],
        &rules,
        CaptureMode::CallstackOnly,
        // The same source hash `classify_batch` assigns to deposit 0.
        &["0xdeadbeef00"],
    );
    assert_eq!(
        verdicts[0].0,
        Verdict::Allow,
        "an already-approved deposit keeps its allow and its commit, unchanged"
    );
    // The observation WAS produced — it is available for the diagnostic — and names the unapproved
    // implementation. No verdict reads it.
    match &verdicts[0].1 {
        Observations::Available { items, .. } => {
            assert_eq!(items.len(), 1);
            assert_eq!(
                items[0].delegate_address,
                unapproved_impl(),
                "the unapproved implementation is observed, and knowingly not acted on"
            );
        }
        other => panic!("expected an available set for the diagnostic, got {other:?}"),
    }
    // And the diagnostic recorder itself is exercised on both shapes of result.
    super::rpc::record_known_allowed_diagnostic("0xdeadbeef00", &verdicts[0].1);
    super::rpc::record_known_allowed_diagnostic(
        "0xdeadbeef00",
        &Observations::Unavailable(ObservationsUnavailable::NotCaptured),
    );
}

/// An event-only rule set selects nothing, so no configuration is installed, the frame channel does
/// no work, and the result is the unchanged behaviour of this path.
#[test]
fn an_event_only_rule_set_leaves_the_deposit_path_unchanged() {
    let rule = r#"{"id":"event-only","event_abis":{},"condition":true,"action":"deny"}"#;
    let raw: Box<serde_json::value::RawValue> = serde_json::from_str(rule).unwrap();
    let set = load_rules_from_raw(1, 0, std::slice::from_ref(&raw));
    // An empty `event_abis` with no callstack is rejected outright, which is itself the unchanged
    // behaviour; what matters here is that such a set selects nothing.
    assert!(!set.declares_any_alias());
    assert!(config_for(&set, 10).is_none(), "nothing selected means nothing configured");
}
