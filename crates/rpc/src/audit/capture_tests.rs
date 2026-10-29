//! Deposit-path acceptance tests: real execution-frame capture feeding the deposit classifier.
//!
//! These exercise the deposit path's own half of the contract — that the inspector this handler
//! installs really produces observations, that the classifier turns them into the documented
//! verdicts, that the ordered state effects across a batch are preserved, and that the two
//! screening paths reach the **same** verdict from the same frames.
//!
//! Two things make that evidence real rather than self-referential:
//!
//! * the transactions are built by [`build_deposit_tx`], so every one of them is an actual OP
//!   **Deposit** envelope — the shape this handler receives — and not a signed legacy transaction
//!   standing in for one; and
//! * the per-deposit sequence is **not** restated here. Capture-window arming, execution,
//!   `finish`/`take`, the ordered commit and `verdict_for` are driven through
//!   [`classify_ordered_deposits`], the same function `xlayer_auditTransactions` calls. A test that
//!   reproduced that loop could stay green while the handler diverged from it.
//!
//! What they deliberately do not do is stand up a provider backend and go through the JSON-RPC
//! method itself; the request-contract tests cover that layer, and state anchoring and task
//! placement are reth's own.

use std::collections::HashSet;

use alloy_evm::{Evm, EvmEnv, EvmFactory};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use op_alloy_consensus::OpTxEnvelope;
use op_revm::OpSpecId;
use rcs_filter::callstack::{
    alias_set_digest, Observations, ObservationsUnavailable, SnapshotStamp,
};
use rcs_filter::rules::{load_rules_from_raw, RuleSet};
use revm::context::{BlockEnv, CfgEnv};
use revm::database::{DatabaseRef, InMemoryDB};
use revm::state::{AccountInfo, Bytecode};
use xlayer_rcs_capture::{CaptureFaults, CaptureMode, DirectDelegatecallConfig, RcsInspector};

use super::deposit::{build_deposit_tx, DepositTxRequest};
use super::rpc::classify_ordered_deposits;
use super::verdict::{verdict_for, AuditResult, SimulatedDeposit, Verdict};

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
/// The upgrader implementation's address, reached through the storage proxy's slot 0.
fn upgrader() -> Address {
    Address::from([0xD0; 20])
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

/// The `source_hash` the nth deposit of a batch carries.
fn source_hash(index: usize) -> String {
    format!("0x{:064x}", 0xdeadbeef00u64 + index as u64)
}

/// One deposit request addressed to the selected proxy.
fn request(index: usize) -> DepositTxRequest {
    DepositTxRequest {
        source_hash: source_hash(index),
        from: lower_address(sender()),
        to: Some(lower_address(proxy())),
        mint: "0".to_string(),
        value: "0".to_string(),
        gas_limit: 3_000_000,
        is_system_transaction: false,
        data: "0x".to_string(),
    }
}

/// Runs a batch through the **production** per-deposit loop and returns its results together with
/// the database the loop committed into, so an ordered state effect can be asserted on real state.
///
/// Every transaction is built by `build_deposit_tx`, and the assertion below pins that: these are
/// OP `Deposit` envelopes, which is the whole point of exercising this path.
fn run_batch(
    db: InMemoryDB,
    requests: &[DepositTxRequest],
    rules: &RuleSet,
    mode: CaptureMode,
    faults: CaptureFaults,
    known_allowed: &[&str],
) -> (Vec<Option<AuditResult>>, InMemoryDB) {
    let txs: Vec<_> = requests
        .iter()
        .map(|req| build_deposit_tx(req).expect("the request builds a deposit tx"))
        .collect();
    for (index, tx) in txs.iter().enumerate() {
        match tx.inner() {
            OpTxEnvelope::Deposit(deposit) => {
                assert_eq!(
                    deposit.source_hash,
                    requests[index].source_hash.parse::<B256>().expect("source hash"),
                    "deposit {index} must carry its own source hash"
                );
                assert_eq!(deposit.from, sender());
            }
            other => panic!("deposit {index} must be an OP Deposit envelope, got {other:?}"),
        }
        assert_eq!(tx.signer(), sender(), "a deposit's recovered signer is its `from`");
    }

    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::with_mode_and_faults(10_000, mode, faults),
    );
    let capture_config = config_for(rules, 10_000);
    let known: HashSet<String> = known_allowed.iter().map(|s| (*s).to_string()).collect();
    let results =
        classify_ordered_deposits(&mut evm, &txs, requests, rules, &capture_config, &known)
            .expect("the batch executes");
    (results, evm.into_db())
}

/// `run_batch` with no faults and nothing pre-approved — the ordinary case.
fn plain_batch(
    db: InMemoryDB,
    requests: &[DepositTxRequest],
    rules: &RuleSet,
    mode: CaptureMode,
) -> (Vec<Option<AuditResult>>, InMemoryDB) {
    run_batch(db, requests, rules, mode, CaptureFaults::NONE, &[])
}

#[track_caller]
fn verdict_at(results: &[Option<AuditResult>], index: usize) -> Verdict {
    results[index].as_ref().expect("the deposit was executed").verdict
}

/// The proxy's slot 0 as actually committed — the real ordered state effect.
fn committed_delegate_target(db: &InMemoryDB) -> Address {
    let slot = db.storage_ref(proxy(), U256::ZERO).expect("the proxy's slot 0 is readable");
    Address::from_slice(&slot.to_be_bytes::<32>()[12..])
}

/// A world whose proxy delegatecalls `implementation` directly.
fn fixed_proxy_world(implementation: Address) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(0, Some(delegatecall(implementation))));
    db.insert_account_info(implementation, account(0, Some(stop())));
    db
}

/// A world whose proxy resolves its delegate target from slot 0, which initially points at an
/// upgrader that rewrites that very slot.
fn upgradeable_proxy_world(upgrader_code: &[u8]) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(sender(), account(1_000_000_000_000u64, None));
    db.insert_account_info(proxy(), account(0, Some(storage_proxy())));
    db.insert_account_info(upgrader(), account(0, Some(upgrader_code.to_vec())));
    db.insert_account_info(unapproved_impl(), account(0, Some(stop())));
    db.insert_account_storage(proxy(), U256::ZERO, U256::from_be_slice(upgrader().as_slice()))
        .expect("storage insert");
    db
}

/// A deposit whose proxy delegatecalls an **approved** implementation is allowed.
#[test]
fn a_deposit_executing_an_approved_implementation_is_allowed() {
    let rules = guard_rules(&[keccak256(stop())]);
    let (results, _db) = plain_batch(
        fixed_proxy_world(approved_impl()),
        &[request(0)],
        &rules,
        CaptureMode::CallstackOnly,
    );
    assert_eq!(verdict_at(&results, 0), Verdict::Allow);
    // The result is reported against this deposit's own source hash.
    assert_eq!(results[0].as_ref().unwrap().source_hash, source_hash(0));
}

/// A deposit whose proxy delegatecalls an **unapproved** implementation is denied, and the deny
/// comes from the real frame rather than from anything read out of storage.
#[test]
fn a_deposit_executing_an_unapproved_implementation_is_denied() {
    // The allowlist holds a hash nothing in this world executes.
    let rules = guard_rules(&[keccak256([0xFEu8])]);
    let (results, _db) = plain_batch(
        fixed_proxy_world(unapproved_impl()),
        &[request(0)],
        &rules,
        CaptureMode::CallstackOnly,
    );
    assert_eq!(verdict_at(&results, 0), Verdict::Deny);
}

/// Ordered state effects across a batch: an earlier deposit upgrades the proxy's delegate target,
/// and the later deposit is judged against the **new**, real implementation.
///
/// This is the property that makes the batch ordered rather than independent — remove the commit
/// between deposits and the second one resolves the old target and comes back allowed. The
/// committed state is asserted too, so the test fails if the allow stopped committing.
#[test]
fn an_earlier_deposits_upgrade_is_visible_to_a_later_deposit_in_the_same_batch() {
    let upgrader_code = upgrader_to(unapproved_impl());
    // Only the upgrader's code is approved, so the first deposit is allowed and committed and the
    // second — which now reaches the unapproved implementation — is denied.
    let rules = guard_rules(&[keccak256(&upgrader_code)]);

    let (results, db) = plain_batch(
        upgradeable_proxy_world(&upgrader_code),
        &[request(0), request(1)],
        &rules,
        CaptureMode::CallstackOnly,
    );
    assert_eq!(verdict_at(&results, 0), Verdict::Allow, "the upgrade itself runs approved code");
    assert_eq!(
        verdict_at(&results, 1),
        Verdict::Deny,
        "the later deposit must be judged against the implementation the upgrade installed"
    );
    assert_eq!(
        committed_delegate_target(&db),
        unapproved_impl(),
        "the allowed deposit's state effect must be committed, in order, before the next deposit"
    );
}

/// A three-deposit batch keeps every earlier effect: the first upgrade is committed, the second
/// deposit is denied and commits nothing, and the third is still judged against the first
/// upgrade's target.
#[test]
fn a_denied_deposit_commits_nothing_and_does_not_disturb_the_batch_order() {
    let upgrader_code = upgrader_to(unapproved_impl());
    let rules = guard_rules(&[keccak256(&upgrader_code)]);

    let (results, db) = plain_batch(
        upgradeable_proxy_world(&upgrader_code),
        &[request(0), request(1), request(2)],
        &rules,
        CaptureMode::CallstackOnly,
    );
    assert_eq!(verdict_at(&results, 0), Verdict::Allow);
    assert_eq!(verdict_at(&results, 1), Verdict::Deny);
    assert_eq!(verdict_at(&results, 2), Verdict::Deny, "the denied state was never committed");
    assert_eq!(
        committed_delegate_target(&db),
        unapproved_impl(),
        "only the allowed deposit's effect is present"
    );
    // Every slot is filled, in request order, each against its own source hash.
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result.as_ref().unwrap().source_hash, source_hash(index));
    }
}

/// The same rule against the same frames yields the **same** verdict on both paths. The two modes
/// are the only difference between them, and frame identification is shared code, so a divergence
/// here would mean the two paths had stopped agreeing.
#[test]
fn both_paths_reach_the_same_verdict_for_the_same_frames() {
    let rules = guard_rules(&[keccak256([0xFEu8])]);
    for implementation in [approved_impl(), unapproved_impl()] {
        let (deposit_path, _) = plain_batch(
            fixed_proxy_world(implementation),
            &[request(0)],
            &rules,
            CaptureMode::CallstackOnly,
        );
        let (builder_mode, _) = plain_batch(
            fixed_proxy_world(implementation),
            &[request(0)],
            &rules,
            CaptureMode::NativeAndCallstack,
        );
        assert_eq!(
            verdict_at(&deposit_path, 0),
            verdict_at(&builder_mode, 0),
            "the two paths must agree for implementation {implementation}"
        );
    }
}

/// A real capture fault makes the production path fail closed: the observation set is unavailable,
/// so the deposit gets a **non-verdict** and — the part that matters — commits nothing.
///
/// Driven through the public `CaptureFaults` API rather than a synthetic `Observations` value, so
/// the fail-closed behaviour is observed where it has to hold: in the handler's own loop.
#[test]
fn a_capture_fault_makes_the_production_path_fail_closed_without_committing() {
    let upgrader_code = upgrader_to(unapproved_impl());
    // Everything this world executes is approved, so without the fault the deposit would be
    // allowed and its upgrade committed. The fault is the only difference.
    let rules = guard_rules(&[keccak256(&upgrader_code)]);

    let (allowed, allowed_db) = plain_batch(
        upgradeable_proxy_world(&upgrader_code),
        &[request(0)],
        &rules,
        CaptureMode::CallstackOnly,
    );
    assert_eq!(verdict_at(&allowed, 0), Verdict::Allow, "control: no fault means allow");
    assert_eq!(committed_delegate_target(&allowed_db), unapproved_impl());

    for faults in [
        CaptureFaults { force_own_code_indeterminate: true, ..CaptureFaults::NONE },
        CaptureFaults { force_frames_unbalanced: true, ..CaptureFaults::NONE },
    ] {
        let (results, db) = run_batch(
            upgradeable_proxy_world(&upgrader_code),
            &[request(0)],
            &rules,
            CaptureMode::CallstackOnly,
            faults,
            &[],
        );
        assert_eq!(
            verdict_at(&results, 0),
            Verdict::Malformed,
            "{faults:?} must produce a non-verdict, never an allow"
        );
        assert_eq!(
            committed_delegate_target(&db),
            upgrader(),
            "{faults:?}: a non-verdict must commit nothing"
        );
    }
}

/// An unavailable observation set is a **non-verdict** on this path, never an allow — for every
/// documented reason, including the ones no injectable fault can produce.
#[test]
fn an_unavailable_observation_set_is_a_non_verdict_never_an_allow() {
    let rules = guard_rules(&[keccak256(stop())]);
    let req = request(0);
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
    let req = request(0);
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
    let upgrader_code = upgrader_to(unapproved_impl());
    // Nothing in this world is approved, so an ordinary deposit here would be denied.
    let rules = guard_rules(&[keccak256([0xFEu8])]);

    let (denied, denied_db) = plain_batch(
        upgradeable_proxy_world(&upgrader_code),
        &[request(0)],
        &rules,
        CaptureMode::CallstackOnly,
    );
    assert_eq!(verdict_at(&denied, 0), Verdict::Deny, "control: unapproved code is denied");
    assert_eq!(committed_delegate_target(&denied_db), upgrader(), "and commits nothing");

    // The same deposit, pre-approved by the caller: allowed and committed regardless.
    let (results, db) = run_batch(
        upgradeable_proxy_world(&upgrader_code),
        &[request(0)],
        &rules,
        CaptureMode::CallstackOnly,
        CaptureFaults::NONE,
        &[&source_hash(0)],
    );
    assert_eq!(
        verdict_at(&results, 0),
        Verdict::Allow,
        "an already-approved deposit keeps its allow and its commit, unchanged"
    );
    assert_eq!(
        committed_delegate_target(&db),
        unapproved_impl(),
        "its state effect is committed unconditionally"
    );
    assert!(
        results[0].as_ref().unwrap().actions.is_none(),
        "the forced allow carries no rule actions"
    );

    // And the diagnostic recorder itself is exercised on both shapes of result.
    super::rpc::record_known_allowed_diagnostic(
        &source_hash(0),
        &Observations::Available {
            stamp: SnapshotStamp {
                protocol_version: 1,
                content_version: 0,
                alias_set_digest: alias_set_digest([proxy()]),
            },
            items: Vec::new(),
        },
    );
    super::rpc::record_known_allowed_diagnostic(
        &source_hash(0),
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
