//! Equal identification across the two real execution paths that identify executed code: the
//! payload builder's candidate loop and the audit RPC's simulation. Both drive the production
//! code — the candidate through `execute_best_transactions`, the audit path through the complete
//! production batch behind `execute_audit_batch`.

use alloy_consensus::TxEip1559;
use alloy_eips::Encodable2718;
use alloy_evm::EvmEnv;
use alloy_primitives::{Address, TxKind, B256, U256};
use op_alloy_consensus::OpTypedTransaction;
use op_revm::OpSpecId;
use rcs_filter::callstack::{CallObservation, CallObservationKind};
use rcs_filter::rules::{load_rules, RawRule};
use rcs_filter::Screen;
use reth_optimism_txpool::OpPooledTransaction;
use reth_payload_util::PayloadTransactionsFixed;
use reth_revm::State;
use reth_transaction_pool::test_utils::testing_pool;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::{CacheDB, EmptyDB, InMemoryDB};
use revm::state::{AccountInfo, Bytecode};
use revm::Database as _;
use xlayer_rpc::audit::deposit::build_deposit_tx;
use xlayer_rpc::audit::rpc::observation_recorder;
use xlayer_rpc::audit::rpc::test_seams::execute_audit_batch;
use xlayer_rpc::audit::rpc::AuditTransactionsRequest;
use xlayer_rpc::audit::verdict::{CallstackIdentification, Verdict};

use xlayer_builder::flashblocks::test_seams::{
    clear_last_candidate_observations, clear_last_screen_decision, last_candidate_observations,
    last_screen_decision, ExecutionInfo, LoopSeams, TransactionLimits,
};

use crate::fixtures::*;

/// The fixture chain state, shared by both paths: the delegating (or plain) proxy, both
/// implementations, a funded depositor and — for the candidate path — a funded pool sender.
fn fixture_state(extra_funded: Option<Address>, proxy_code: Vec<u8>) -> State<CacheDB<EmptyDB>> {
    let mut cache = CacheDB::new(EmptyDB::default());
    for address in [DEPOSITOR].into_iter().chain(extra_funded) {
        cache.insert_account_info(
            address,
            AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
        );
    }
    for (address, code) in
        [(PROXY, proxy_code), (APPROVED_IMPL, impl_code(0x01)), (UNAPPROVED_IMPL, impl_code(0x02))]
    {
        cache.insert_account_info(
            address,
            AccountInfo { code: Some(Bytecode::new_raw(code.into())), ..Default::default() },
        );
    }
    State::builder().with_database(cache).with_bundle_update().build()
}

/// The same accounts in the in-memory database shape the audit path's own EVM is built over.
fn audit_path_db(proxy_code: Vec<u8>) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    for (address, code) in
        [(PROXY, proxy_code), (APPROVED_IMPL, impl_code(0x01)), (UNAPPROVED_IMPL, impl_code(0x02))]
    {
        db.insert_account_info(
            address,
            AccountInfo { code: Some(Bytecode::new_raw(code.into())), ..Default::default() },
        );
    }
    db
}

fn audit_env() -> EvmEnv<OpSpecId> {
    EvmEnv::new(
        CfgEnv::new_with_spec(OpSpecId::JOVIAN),
        BlockEnv { gas_limit: 30_000_000, ..Default::default() },
    )
}

/// What the candidate simulation loop observed and decided. Driven through the real
/// `execute_best_transactions`, not a helper: the EVM, the inspector, the capture window
/// and the screening call are all the production ones.
fn candidate_path(raw: Vec<RawRule>, proxy_code: Vec<u8>) -> (Vec<CallObservation>, Screen) {
    let filter = filter_with(raw, test_config());
    let ctx = ctx_with(Some(filter), Vec::new());
    let signer = funded_signer();
    let candidate = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 1_000_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(PROXY),
        value: U256::ZERO,
        ..Default::default()
    });
    let recovered = signer.sign_tx(candidate).expect("sign the candidate");
    let encoded_len = recovered.encode_2718_len();
    let pooled: OpPooledTransaction = OpPooledTransaction::new(recovered, encoded_len);

    let mut state = fixture_state(Some(signer.address), proxy_code);
    let pool = testing_pool();
    let mut best = PayloadTransactionsFixed::new(vec![pooled]);
    let mut info = ExecutionInfo::with_capacity(1);
    let limits =
        TransactionLimits { block_gas: 30_000_000, block_da: None, block_da_footprint: None };

    clear_last_screen_decision();
    clear_last_candidate_observations();
    ctx.execute_best_transactions(&mut info, &mut state, &mut best, &pool, limits)
        .expect("the candidate loop runs");

    (
        last_candidate_observations()
            .expect("the candidate's capture completed, so its observations are recorded"),
        last_screen_decision().expect("the candidate reached the screening decision"),
    )
}

/// What the audit path observed and decided, driven through its PRODUCTION request pipeline:
/// the real wire request, the real request-level validation and rule compilation, the real
/// inspector construction and the real batch body — which is what opens the capture window,
/// maps the capture outcome to an observation set, adjudicates and decides the commit.
///
/// The returned observations are the ones the production batch itself fed to screening,
/// recorded inside that batch rather than re-derived here. A production path that discarded its
/// capture, or mapped a verdict wrongly, therefore cannot satisfy this.
///
/// The handler's own `audit_transactions` is not used only because it needs a node
/// backend to anchor state; everything after that anchoring is the production path.
fn audit_path_with_rule(
    rule: serde_json::Value,
    proxy_code: Vec<u8>,
) -> (Vec<CallObservation>, Verdict, Option<CallstackIdentification>, u64) {
    let source_hash = format!("{:#x}", B256::repeat_byte(0x33));
    // A real request, parsed through the strict wire schema the RPC exposes. The protocol
    // version is deliberately OMITTED, which is the shape the caller actually sends and
    // which must mean version 1.
    let request: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
        "rules": [rule],
        "txs": [{
            "source_hash": source_hash,
            "from": format!("{DEPOSITOR:#x}"),
            "to": format!("{PROXY:#x}"),
            "mint": "0",
            "value": "0",
            "gas_limit": 1_000_000,
            "is_system_transaction": false,
            "data": "0x",
        }],
    }))
    .expect("the fixture request satisfies the audit RPC's own wire schema");
    // The deposit builds from the same request the production batch decodes.
    assert!(
        request.txs.iter().all(|req| build_deposit_tx(req).is_ok()),
        "the fixture request builds"
    );

    // Drop anything an earlier leg recorded, so what comes back is this batch's own.
    let _ = observation_recorder::take();
    let mut db = audit_path_db(proxy_code);
    let results = execute_audit_batch(&request, &mut db, audit_env())
        .expect("the production batch executes the fixture transaction");
    let recorded = observation_recorder::take();
    assert_eq!(recorded.len(), 1, "one transaction, one recorded observation set");
    let observed = recorded[0]
        .clone()
        .expect("the production batch fed screening an AVAILABLE observation set");
    assert_eq!(results.len(), 1, "the production batch classified the tx");
    let result = &results[0];
    assert_eq!(result.source_hash, source_hash, "the result belongs to this request");
    // The production batch also decides whether to commit. The depositor's nonce is the
    // observable: executing the deposit bumps it, so a committed batch leaves 1 and a
    // discarded one leaves 0 — independent of what the implementation's code does, which
    // this shared fixture deliberately keeps side-effect free. Measured, not assumed: the
    // same fixture through the same production batch gives nonce 1 on allow and 0 on deny.
    let depositor_nonce = db
        .basic(DEPOSITOR)
        .expect("the depositor account is readable")
        .expect("the fixture seeds the depositor")
        .nonce;
    (observed, result.verdict, result.callstack_identification, depositor_nonce)
}

fn audit_path(
    approved: &[B256],
    proxy_code: Vec<u8>,
) -> (Vec<CallObservation>, Verdict, Option<CallstackIdentification>, u64) {
    audit_path_with_rule(proxy_impl_allowlist_rule_json(approved), proxy_code)
}

/// ONE fixture — the same observed proxy, the same implementation, the same (empty)
/// calldata and the same seeded accounts — driven through both real execution paths
/// yields the SAME observed call frames and the SAME decision, in both directions of the
/// rule pattern.
///
/// The observed frames are compared, not just the decisions: two paths can agree on
/// a decision while resting on different observations, and it is the observation equality
/// that "equal identification" actually asserts. The snapshot STAMP is deliberately not
/// part of the comparison — each path stamps its observations with its own rule-set
/// identity (the builder's installed snapshot, the audit path's per-request set), and
/// requiring those to be equal would assert something the design never claims.
#[tokio::test]
async fn candidate_and_audit_paths_observe_the_same_frames_and_reach_the_same_decision() {
    let approved = code_hash(&impl_code(0x01));
    for executed_impl in [UNAPPROVED_IMPL, APPROVED_IMPL] {
        let objected = executed_impl == UNAPPROVED_IMPL;
        let proxy_code = delegating_proxy_code(executed_impl, 1);
        let raw = vec![proxy_impl_allowlist_rule(&[approved])];
        // The audit path compiles the rules carried in its own request; this only pins that
        // the shared rule source really is a clean rule, so a silent rejection could not make
        // the comparison vacuous on either path.
        assert!(
            load_rules(rcs_filter::config::MIN_PROTOCOL_VERSION, 0, raw.clone())
                .rejected
                .is_empty(),
            "the fixture rule must load clean"
        );

        let (candidate_observed, screen) = candidate_path(raw.clone(), proxy_code.clone());
        let (audit_observed, verdict, audit_identification, audit_depositor_nonce) =
            audit_path(&[approved], proxy_code.clone());

        // The fixture really produced the frames the rule is about — the proxy running
        // its own code, then its first-level delegatecall — so the equality below cannot
        // be satisfied by two empty sets.
        assert_eq!(
            candidate_observed.len(),
            2,
            "the fixture observes the proxy's own frame and its delegatecall: \
                 {candidate_observed:?}"
        );
        assert_eq!(candidate_observed[0].kind, CallObservationKind::Call);
        assert_eq!(candidate_observed[0].address, PROXY);
        assert!(
            candidate_observed[0].impl_address.is_none(),
            "the proxy's own frame carries no implementation: {candidate_observed:?}"
        );
        assert_eq!(candidate_observed[1].kind, CallObservationKind::DirectDelegatecall);
        assert_eq!(candidate_observed[1].address, PROXY);
        assert_eq!(candidate_observed[1].impl_address, Some(executed_impl));
        assert_eq!(
            candidate_observed[1].impl_code_hash,
            Some(code_hash(&impl_code(if objected { 0x02 } else { 0x01 }))),
            "the observed implementation code hash is the one that really executed"
        );

        assert_eq!(
            candidate_observed, audit_observed,
            "the candidate loop and the audit path must observe the same frames"
        );

        if objected {
            assert_eq!(screen, Screen::Deny, "the candidate loop rejects");
            assert_eq!(verdict, Verdict::Deny, "the audit path denies");
            assert_eq!(
                audit_identification,
                Some(CallstackIdentification::Objected),
                "the audit path reports the objection it acted on"
            );
            assert_eq!(
                audit_depositor_nonce, 0,
                "the audit path discards the state of the transaction it denied"
            );
        } else {
            assert_eq!(screen, Screen::Allow, "the candidate loop admits");
            assert_eq!(verdict, Verdict::Allow, "the audit path allows");
            assert_eq!(
                audit_identification,
                Some(CallstackIdentification::Clean),
                "the audit path identified the frames it allowed on"
            );
            assert_eq!(
                audit_depositor_nonce, 1,
                "the audit path commits the state of the transaction it allowed"
            );
        }
    }
}

/// "Deny unless all four fields are approved": the proxy itself, the code its own frame ran, the
/// delegate target and the code that ran there.
fn four_field_rule_json(proxy_hash: B256, target: Address, target_hash: B256) -> serde_json::Value {
    serde_json::json!({
        "id": "four-field-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "and": [
            { "==": [ { "var": "entry.proxyAddress" }, format!("{PROXY:#x}") ] },
            { "in": [ { "var": "entry.proxyCodeHash" }, [format!("{proxy_hash:#x}")] ] },
            { "in": [ { "var": "entry.delegateTo" }, [format!("{target:#x}")] ] },
            { "in": [ { "var": "entry.delegateCodeHash" }, [format!("{target_hash:#x}")] ] }
        ]}},
        "action": "deny"
    })
}

/// The four direct-delegatecall condition variables decide identically on both paths.
#[tokio::test]
async fn the_four_field_rule_reaches_the_same_decision_on_both_paths() {
    let proxy_code = delegating_proxy_code(APPROVED_IMPL, 1);
    let proxy_hash = code_hash(&proxy_code);
    let target_hash = code_hash(&impl_code(0x01));
    let cases = [
        ("all four approved", four_field_rule_json(proxy_hash, APPROVED_IMPL, target_hash), false),
        (
            "proxy code hash not approved",
            four_field_rule_json(B256::repeat_byte(0x5e), APPROVED_IMPL, target_hash),
            true,
        ),
        (
            "target address not approved",
            four_field_rule_json(proxy_hash, UNAPPROVED_IMPL, target_hash),
            true,
        ),
        (
            "target code hash not approved",
            four_field_rule_json(proxy_hash, APPROVED_IMPL, B256::repeat_byte(0x5f)),
            true,
        ),
    ];
    for (case, rule, objected) in cases {
        let raw: RawRule = serde_json::from_value(rule.clone()).expect("the rule deserializes");
        let (candidate_observed, screen) = candidate_path(vec![raw], proxy_code.clone());
        let (audit_observed, verdict, audit_identification, audit_depositor_nonce) =
            audit_path_with_rule(rule, proxy_code.clone());

        let delegatecall = candidate_observed
            .iter()
            .find(|o| o.kind == CallObservationKind::DirectDelegatecall)
            .unwrap_or_else(|| panic!("{case}: the proxy's delegatecall is observed"));
        assert_eq!(delegatecall.address, PROXY, "{case}");
        assert_eq!(delegatecall.code_hash, proxy_hash, "{case}: the proxy's executed code hash");
        assert_eq!(delegatecall.impl_address, Some(APPROVED_IMPL), "{case}");
        assert_eq!(delegatecall.impl_code_hash, Some(target_hash), "{case}");
        assert_eq!(candidate_observed, audit_observed, "{case}: candidate vs audit frames");

        if objected {
            assert_eq!(screen, Screen::Deny, "{case}: candidate loop");
            assert_eq!(verdict, Verdict::Deny, "{case}: audit path");
            assert_eq!(audit_identification, Some(CallstackIdentification::Objected), "{case}");
            assert_eq!(audit_depositor_nonce, 0, "{case}: a denied audit tx commits nothing");
        } else {
            assert_eq!(screen, Screen::Allow, "{case}: candidate loop");
            assert_eq!(verdict, Verdict::Allow, "{case}: audit path");
            assert_eq!(audit_identification, Some(CallstackIdentification::Clean), "{case}");
            assert_eq!(audit_depositor_nonce, 1, "{case}: an allowed audit tx commits");
        }
    }
}
