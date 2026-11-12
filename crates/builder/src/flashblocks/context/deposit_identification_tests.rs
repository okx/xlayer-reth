//! Path 2 — deposit / sequencer-path call-frame identification.
//!
//! Every test here drives the REAL `execute_sequencer_transactions` entry point against a real
//! EVM and a real rule snapshot. The two properties under test pull in opposite directions and
//! are therefore asserted separately on the same run: the deposit's executed implementation IS
//! identified and reported, and the produced block is byte-for-byte what the pre-change code
//! produced.

use super::*;
use alloy_consensus::Sealable;
use alloy_genesis::Genesis;
use alloy_primitives::{keccak256, TxKind};
use alloy_rpc_types_engine::PayloadAttributes;
use op_alloy_consensus::{OpTxEnvelope, TxDeposit};
use op_alloy_rpc_types_engine::OpPayloadAttributes;
use rcs_filter::rules::{load_rules, RawRule};
use rcs_filter::{FilterConfig, SystemClock};
use revm::context::{BlockEnv, CfgEnv};
use revm::database::{CacheDB, EmptyDB};
use revm::state::{AccountInfo, Bytecode};

use super::capture_decision_tests::capture_tracing;

/// The proxy a rule observes. Its own code performs a first-level `DELEGATECALL`.
const PROXY: Address = Address::repeat_byte(0xAA);
/// The implementation the proxy delegates to in the "approved" fixture.
const APPROVED_IMPL: Address = Address::repeat_byte(0xB1);
/// The implementation the proxy delegates to in the "unapproved" fixture.
const UNAPPROVED_IMPL: Address = Address::repeat_byte(0xB2);
/// Depositor.
const DEPOSITOR: Address = Address::repeat_byte(0xC1);

/// `DELEGATECALL(gas, target, 0, 0, 0, 0); STOP` — the proxy's own first-level delegatecall,
/// which is exactly the frame shape a `callstack` selector on the proxy observes.
fn delegating_proxy_code(target: Address, times: usize) -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 0..times {
        // retSize, retOffset, argsSize, argsOffset (all zero), then PUSH20 target, GAS.
        code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73]);
        code.extend_from_slice(target.as_slice());
        code.extend_from_slice(&[0x5a, 0xf4, 0x50]); // GAS; DELEGATECALL; POP
    }
    code.push(0x00); // STOP
    code
}

/// Implementation runtime code: a bare `STOP`. Non-empty, so the frame really executes and
/// therefore really produces an observation; an empty account would return without ever
/// creating an interpreter.
fn impl_code(tag: u8) -> Vec<u8> {
    vec![0x60, tag, 0x50, 0x00] // PUSH1 tag; POP; STOP — distinct code per implementation
}

fn code_hash(code: &[u8]) -> B256 {
    keccak256(code)
}

/// The normative rule pattern, verbatim in shape: observe by **proxy address
/// only**, then deny when the observed implementation code hash is NOT in the approved set.
/// The explicit non-null guard is what keeps an ordinary non-delegatecall call to the proxy
/// from being denied, because `implCodeHash` is null for a plain call observation.
fn proxy_impl_allowlist_rule(approved: &[B256]) -> RawRule {
    serde_json::from_value(proxy_impl_allowlist_rule_json(approved))
        .expect("the normative sample must deserialize")
}

/// The same sample as wire JSON. The audit path receives a real request rather than an
/// already-parsed rule, so both forms are built from THIS one source and cannot drift.
fn proxy_impl_allowlist_rule_json(approved: &[B256]) -> serde_json::Value {
    let approved: Vec<String> = approved.iter().map(|h| format!("{h:#x}")).collect();
    serde_json::json!({
        "id": "proxy-impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "in": [ { "var": "entry.delegateCodeHash" }, approved ] } },
        "action": "deny"
    })
}

/// A rule that observes nothing: no `callstack` section at all, so the snapshot declares no
/// selectors and this path must stay on its pre-existing code.
fn event_only_rule() -> RawRule {
    serde_json::from_str(rcs_filter::test_support::golden::RULE_SCENARIO_A).expect("golden fixture")
}

fn filter_with(rules: Vec<RawRule>, config: FilterConfig) -> Arc<FilterHandle> {
    // One protocol version covers the whole rule schema, `callstack` included, so the version
    // no longer varies with what a rule declares.
    let set = load_rules(rcs_filter::config::MIN_PROTOCOL_VERSION, 1, rules);
    assert!(set.rejected.is_empty(), "test rules must load clean: {:?}", set.rejected);
    Arc::new(FilterHandle::for_test(config, set, Arc::new(SystemClock)))
}

/// One unsigned L1→L2 deposit targeting `to`, in the shape the attributes carry.
fn deposit_to(to: Address) -> WithEncoded<OpTransactionSigned> {
    let deposit = TxDeposit {
        source_hash: B256::repeat_byte(0x33),
        from: DEPOSITOR,
        to: TxKind::Call(to),
        mint: 0,
        value: U256::ZERO,
        gas_limit: 1_000_000,
        is_system_transaction: false,
        input: Bytes::new(),
    };
    let tx = OpTransactionSigned::from(OpTxEnvelope::Deposit(deposit.seal_slow()));
    let encoded = Bytes::from(tx.encoded_2718());
    WithEncoded::new(encoded, tx)
}

/// A real `FlashblocksBuilderCtx` carrying `deposits` in its attributes. `filter` is `None`
/// for the reference run that reproduces the pre-change code path exactly.
fn ctx_with(
    filter: Option<Arc<FilterHandle>>,
    deposits: Vec<WithEncoded<OpTransactionSigned>>,
) -> FlashblocksBuilderCtx {
    let mut genesis: Genesis =
        serde_json::from_str(include_str!("../../tests/framework/artifacts/genesis.json.tmpl"))
            .expect("valid genesis template JSON");
    genesis.config.chain_id = 1;
    genesis.base_fee_per_gas = Some(0u64.into());
    let chain_spec = Arc::new(OpChainSpec::from_genesis(genesis));
    let evm_config = OpEvmConfig::optimism(chain_spec.clone());
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.chain_id = 1;
    let evm_env = EvmEnv::new(cfg, BlockEnv { gas_limit: 30_000_000, ..Default::default() });
    let payload_id = PayloadId::new([0x42; 8]);
    let rpc_attrs = OpPayloadAttributes {
        payload_attributes: PayloadAttributes { timestamp: 1_751_000_002, ..Default::default() },
        gas_limit: Some(30_000_000),
        ..Default::default()
    };
    let mut attributes =
        OpPayloadBuilderAttributes::from_rpc_attrs(B256::ZERO, payload_id, rpc_attrs)
            .expect("attrs decode");
    attributes.transactions = deposits;
    let config = PayloadConfig {
        parent_header: Arc::new(SealedHeader::seal_slow(alloy_consensus::Header::default())),
        parent_block_info: None,
        payload_id,
        attributes,
    };
    let block_env_attributes = OpNextBlockEnvAttributes {
        timestamp: 1_751_000_002,
        suggested_fee_recipient: Address::ZERO,
        prev_randao: B256::ZERO,
        gas_limit: 30_000_000,
        parent_beacon_block_root: None,
        extra_data: Bytes::new(),
    };
    let rcs_rule_snapshot = filter.as_ref().map(|f| f.rule_snapshot());
    FlashblocksBuilderCtx {
        evm_config,
        da_config: OpDAConfig::default(),
        gas_limit_config: OpGasLimitConfig::default(),
        chain_spec,
        config,
        evm_env,
        block_env_attributes,
        cancel: CancellationToken::new(),
        builder_signer: None,
        metrics: Arc::new(BuilderMetrics::default()),
        max_gas_per_txn: None,
        bridge_intercept_config: Default::default(),
        gasless_contract: None,
        gasless_block_gas_limit: None,
        filter,
        rcs_rule_snapshot,
    }
}

/// The chain state every fixture shares: a funded depositor, the delegating proxy, and both
/// implementations.
fn seeded_state(proxy_code: Vec<u8>) -> State<CacheDB<EmptyDB>> {
    let mut cache = CacheDB::new(EmptyDB::default());
    cache.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
    );
    for (address, code) in
        [(PROXY, proxy_code), (APPROVED_IMPL, impl_code(0x01)), (UNAPPROVED_IMPL, impl_code(0x02))]
    {
        cache.insert_account_info(
            address,
            AccountInfo { code: Some(Bytecode::new_raw(Bytes::from(code))), ..Default::default() },
        );
    }
    State::builder().with_database(cache).with_bundle_update().build()
}

/// Everything about the produced block that consensus depends on.
#[derive(Debug, PartialEq, Eq)]
struct BlockFacts {
    /// Included transactions, in order.
    included: Vec<B256>,
    cumulative_gas_used: u64,
    receipts: Vec<OpReceipt>,
    /// Canonical digest of the merged post-state bundle.
    ///
    /// Deliberately the bundle rather than the state root: the state root is a function of
    /// the post-state, so two runs with equal bundles necessarily produce the same root —
    /// comparing the bundle is strictly stronger and needs no trie fixture.
    post_state: B256,
}

/// Runs the real `execute_sequencer_transactions` and takes the consensus-visible facts of
/// the produced block.
fn run(ctx: &FlashblocksBuilderCtx, state: &mut State<CacheDB<EmptyDB>>) -> BlockFacts {
    let info = ctx.execute_sequencer_transactions(state).expect("sequencer txs execute");
    state.merge_transitions(reth_revm::db::states::bundle_state::BundleRetention::Reverts);
    let bundle = state.take_bundle();
    let mut accounts: Vec<String> = bundle
        .state
        .iter()
        .map(|(address, account)| {
            let mut storage: Vec<_> = account
                .storage
                .iter()
                .map(|(slot, value)| (*slot, value.present_value, value.original_value()))
                .collect();
            storage.sort_unstable_by_key(|(slot, _, _)| *slot);
            format!(
                "{address:?}|{:?}|{:?}|{:?}|{storage:?}",
                account.original_info, account.info, account.status
            )
        })
        .collect();
    accounts.sort_unstable();
    BlockFacts {
        included: info.executed_transactions.iter().map(|tx| tx.tx_hash()).collect(),
        cumulative_gas_used: info.cumulative_gas_used,
        receipts: info.receipts.clone(),
        post_state: keccak256(accounts.join(";").as_bytes()),
    }
}

/// Resets the per-thread test seams. `cargo test` reuses threads across tests, so a stale
/// value from an earlier test could otherwise make a later assertion pass for a wrong reason.
fn reset_seams() {
    DEPOSIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.set(0));
    LAST_DEPOSIT_IDENTIFICATION.with(|d| d.set(None));
}

/// a deposit whose execution delegatecalls an UNAPPROVED implementation produces an
/// observation carrying the real executed `implCodeHash`, and the shared classifier yields
/// the deny-worthy verdict, which is reported.
#[test]
fn deposit_delegatecalling_unapproved_impl_is_deny_worthy() {
    reset_seams();
    let approved = code_hash(&impl_code(0x01));
    let filter = filter_with(vec![proxy_impl_allowlist_rule(&[approved])], test_config());
    let ctx = ctx_with(Some(filter), vec![deposit_to(PROXY)]);
    let mut state = seeded_state(delegating_proxy_code(UNAPPROVED_IMPL, 1));

    let facts = run(&ctx, &mut state);

    assert_eq!(
        DEPOSIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.get()),
        1,
        "a selector-bearing snapshot must attach exactly one inspector"
    );
    assert_eq!(
        LAST_DEPOSIT_IDENTIFICATION.with(|d| d.get()),
        Some(DepositIdentification::DenyWorthy)
    );
    // The deny-worthy outcome changed nothing about inclusion.
    assert_eq!(facts.included.len(), 1, "the deposit is still included");
}

/// contrast: the same deposit against the APPROVED implementation is identified and
/// yields allow, so it is not reported as a violation. Without this the previous test could
/// pass for the wrong reason (e.g. everything being denied).
#[test]
fn deposit_delegatecalling_approved_impl_is_allow() {
    reset_seams();
    let approved = code_hash(&impl_code(0x01));
    let filter = filter_with(vec![proxy_impl_allowlist_rule(&[approved])], test_config());
    let ctx = ctx_with(Some(filter), vec![deposit_to(PROXY)]);
    let mut state = seeded_state(delegating_proxy_code(APPROVED_IMPL, 1));

    let facts = run(&ctx, &mut state);

    assert_eq!(DEPOSIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.get()), 1);
    assert_eq!(LAST_DEPOSIT_IDENTIFICATION.with(|d| d.get()), Some(DepositIdentification::Allow));
    assert_eq!(facts.included.len(), 1);
}

/// third direction of the same sample: an ORDINARY (non-delegatecall) call to the observed
/// proxy is identified and ALLOWED. The frame is genuinely observed, but a plain call carries
/// no implementation, so the implementation code hash binds to null and the explicit non-null
/// guard stops the deny branch from being reached. Without this direction the pattern could
/// deny every ordinary caller of the proxy and both other directions would still pass.
#[test]
fn deposit_calling_the_proxy_without_delegatecall_is_allow() {
    reset_seams();
    let approved = code_hash(&impl_code(0x01));
    let filter = filter_with(vec![proxy_impl_allowlist_rule(&[approved])], test_config());
    let ctx = ctx_with(Some(filter), vec![deposit_to(PROXY)]);
    // The proxy's own code performs NO delegatecall here: it is a bare `STOP`.
    let mut state = seeded_state(delegating_proxy_code(UNAPPROVED_IMPL, 0));

    let facts = run(&ctx, &mut state);

    assert_eq!(
        DEPOSIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.get()),
        1,
        "a selector-bearing snapshot still attaches exactly one inspector"
    );
    assert_eq!(
        LAST_DEPOSIT_IDENTIFICATION.with(|d| d.get()),
        Some(DepositIdentification::Allow),
        "a plain call to the proxy has no implementation to compare and must not be denied"
    );
    // The frame really was observed — a plain call frame with no implementation fields — so
    // the allow above is "identified and clean", not "nothing was captured".
    let observed = LAST_DEPOSIT_OBSERVATIONS
        .with(|o| o.borrow().clone())
        .expect("capture completed, so the observation set is recorded");
    assert_eq!(observed.len(), 1, "exactly the proxy frame was observed: {observed:?}");
    assert_eq!(observed[0].kind, CallObservationKind::Call);
    assert_eq!(observed[0].address, PROXY);
    assert!(
        observed[0].impl_address.is_none() && observed[0].impl_code_hash.is_none(),
        "a plain call carries no implementation: {observed:?}"
    );
    assert_eq!(facts.included.len(), 1, "the deposit is still included");
}

/// The consensus-invariance test. In BOTH the deny-worthy and the allow
/// case the block's included transactions, their order, gas used, receipts and resulting
/// state root are identical to the reference run with no filter at all, which is exactly the
/// pre-change code path (`evm_with_env`, capture outcome discarded).
#[test]
fn identification_never_changes_the_produced_block() {
    let approved = code_hash(&impl_code(0x01));
    for proxy_target in [UNAPPROVED_IMPL, APPROVED_IMPL] {
        let proxy_code = delegating_proxy_code(proxy_target, 1);

        reset_seams();
        let reference = {
            let ctx = ctx_with(None, vec![deposit_to(PROXY)]);
            let mut state = seeded_state(proxy_code.clone());
            run(&ctx, &mut state)
        };
        assert_eq!(
            DEPOSIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.get()),
            0,
            "the reference run must take the pre-change path"
        );

        reset_seams();
        let identified = {
            let filter = filter_with(vec![proxy_impl_allowlist_rule(&[approved])], test_config());
            let ctx = ctx_with(Some(filter), vec![deposit_to(PROXY)]);
            let mut state = seeded_state(proxy_code.clone());
            run(&ctx, &mut state)
        };
        assert_eq!(DEPOSIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.get()), 1);
        // The identification actually happened — otherwise equality would be vacuous.
        assert!(LAST_DEPOSIT_IDENTIFICATION.with(|d| d.get()).is_some());

        assert_eq!(
            identified, reference,
            "identification must not change inclusion, order, gas, receipts or state root \
                 (proxy target {proxy_target:?})"
        );
    }
}

/// with no selectors declared, this path constructs NO inspector (asserted at the
/// construction site, not derived from a counter) and the produced block is identical to the
/// reference run.
#[test]
fn no_selectors_constructs_no_inspector_and_changes_nothing() {
    let proxy_code = delegating_proxy_code(UNAPPROVED_IMPL, 1);

    reset_seams();
    let reference = {
        let ctx = ctx_with(None, vec![deposit_to(PROXY)]);
        let mut state = seeded_state(proxy_code.clone());
        run(&ctx, &mut state)
    };

    reset_seams();
    // Screening ENABLED, but the snapshot declares no callstack selectors at all.
    let filter = filter_with(vec![event_only_rule()], test_config());
    assert!(!filter.rule_snapshot().has_callstack_rules());
    let ctx = ctx_with(Some(filter), vec![deposit_to(PROXY)]);
    let mut state = seeded_state(proxy_code);
    let inert = run(&ctx, &mut state);

    assert_eq!(
        DEPOSIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.get()),
        0,
        "an inert snapshot must not construct an inspector at all"
    );
    assert_eq!(
        LAST_DEPOSIT_IDENTIFICATION.with(|d| d.get()),
        None,
        "an inert snapshot must not run identification"
    );
    assert_eq!(inert, reference);
}

/// a capture failure on the deposit path reports an EXPLICIT unavailable reason
/// (never `Available { items: [] }`, which would read as "identified, nothing matched"), and
/// the produced block is still identical to the reference run.
#[test]
fn callstack_budget_overflow_is_not_identified_and_block_is_unchanged() {
    let approved = code_hash(&impl_code(0x01));
    // Two first-level delegatecalls from the proxy ⇒ two observations, under a budget of 1.
    let proxy_code = delegating_proxy_code(UNAPPROVED_IMPL, 2);

    reset_seams();
    let reference = {
        let ctx = ctx_with(None, vec![deposit_to(PROXY)]);
        let mut state = seeded_state(proxy_code.clone());
        run(&ctx, &mut state)
    };

    reset_seams();
    let config = FilterConfig { max_callstack_observations_per_tx: 1, ..test_config() };
    let filter = filter_with(vec![proxy_impl_allowlist_rule(&[approved])], config);
    let ctx = ctx_with(Some(filter), vec![deposit_to(PROXY)]);
    let mut state = seeded_state(proxy_code);
    let overflowed = run(&ctx, &mut state);

    assert_eq!(
        LAST_DEPOSIT_IDENTIFICATION.with(|d| d.get()),
        Some(DepositIdentification::NotIdentified(DepositNotIdentified::BudgetExceeded)),
        "an over-budget capture must be an explicit not-identified, never allow"
    );
    assert_eq!(
        overflowed, reference,
        "a capture failure must not change inclusion, order, gas, receipts or state root"
    );
}

/// a deposit is never excluded, deferred or reordered by any rule outcome. Asserted
/// across all three outcome classes in ONE batch, so ordering is observable rather than
/// implied by a single-element list.
#[test]
fn no_rule_outcome_ever_excludes_defers_or_reorders_a_deposit() {
    reset_seams();
    let approved = code_hash(&impl_code(0x01));
    let filter = filter_with(vec![proxy_impl_allowlist_rule(&[approved])], test_config());
    // Three deposits: one to the proxy (deny-worthy), one to the approved impl directly
    // (no observed proxy frame ⇒ allow), one to the proxy again.
    let deposits = vec![deposit_to(PROXY), deposit_to(APPROVED_IMPL), deposit_to(PROXY)];
    let expected: Vec<B256> = deposits.iter().map(|tx| tx.value().tx_hash()).collect();
    let ctx = ctx_with(Some(filter), deposits);
    let mut state = seeded_state(delegating_proxy_code(UNAPPROVED_IMPL, 1));

    let facts = run(&ctx, &mut state);

    assert_eq!(
        facts.included, expected,
        "every deposit stays included, in its original order, whatever the rule said"
    );
}

/// the bounded diagnostic emits NO rule id and no transfer/observation contents. The
/// rule id matters specifically because `MatchError`'s `Display` does carry one, which is why
/// the implementation reports `error.kind()` and never formats the error.
#[test]
fn bounded_diagnostic_carries_no_rule_id_and_no_content() {
    reset_seams();
    let approved = code_hash(&impl_code(0x01));
    let filter = filter_with(vec![proxy_impl_allowlist_rule(&[approved])], test_config());
    let ctx = ctx_with(Some(filter), vec![deposit_to(PROXY)]);
    let mut state = seeded_state(delegating_proxy_code(UNAPPROVED_IMPL, 1));

    let (_facts, out) = capture_tracing(|| run(&ctx, &mut state));

    assert!(out.contains("rcs_filter::deposit_identification"), "expected target: {out}");
    assert!(out.contains("deny_worthy"), "expected the outcome class: {out}");
    assert!(out.contains("selector_bearing_rules"), "expected the bounded count: {out}");
    assert!(
        !out.contains("proxy-impl-allowlist"),
        "the diagnostic must never carry a rule id: {out}"
    );
    let unapproved_hash = format!("{:#x}", code_hash(&impl_code(0x02)));
    assert!(
        !out.contains(&unapproved_hash),
        "the diagnostic must never carry observation content: {out}"
    );
    assert!(
        !out.to_lowercase().contains(&format!("{UNAPPROVED_IMPL:#x}").to_lowercase()),
        "the diagnostic must never carry an observed address: {out}"
    );
}

/// A `FilterConfig` with screening enabled and generous budgets, so a test that is not about
/// a budget cannot trip one by accident.
fn test_config() -> FilterConfig {
    FilterConfig { enabled: true, ..FilterConfig::default() }
}

/// Equal identification across the three real execution paths.
///
/// Nested inside this module so it reuses the very same fixture builders the single-path
/// tests above use — the same proxy, the same two implementations, the same seeded accounts
/// and the same rule pattern. A separate copy of those fixtures could drift and would make
/// the comparison meaningless.
mod cross_path_identification_tests {
    use super::*;
    use alloy_consensus::TxEip1559;
    use alloy_evm::EvmFactory as _;
    use alloy_op_evm::{OpEvmFactory, OpTx};
    use op_alloy_consensus::OpTypedTransaction;
    use reth_optimism_txpool::OpPooledTransaction;
    use reth_payload_util::PayloadTransactionsFixed;
    use reth_transaction_pool::test_utils::testing_pool;
    use revm::database::InMemoryDB;
    // revm's `Database` trait, anonymously, so the audit path's own EVM database can be read
    // back for the commit observation without shadowing `alloy_evm::Database`.
    use revm::Database as _;
    use xlayer_rpc::audit::deposit::build_deposit_tx;
    use xlayer_rpc::audit::rpc::observation_recorder;
    use xlayer_rpc::audit::rpc::test_seams::{
        audit_capture_inspector, prepare_audit_batch_plan, run_audit_batch,
    };
    use xlayer_rpc::audit::rpc::AuditTransactionsRequest;
    use xlayer_rpc::audit::verdict::{AuditResult, CallstackIdentification, Verdict};
    use xlayer_rpc::AuditCaptureConfig;

    /// The per-transaction observation budget every path is given here. Large enough that no
    /// path can differ from another by tripping a budget.
    const GENEROUS_BUDGET: usize = 10_000;

    /// The fixture chain state, shared by all three paths: the delegating (or plain) proxy,
    /// both implementations, a funded depositor and — for the candidate path — a funded
    /// pool sender.
    fn fixture_state(
        extra_funded: Option<Address>,
        proxy_code: Vec<u8>,
    ) -> State<CacheDB<EmptyDB>> {
        let mut cache = CacheDB::new(EmptyDB::default());
        for address in [DEPOSITOR].into_iter().chain(extra_funded) {
            cache.insert_account_info(
                address,
                AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
            );
        }
        for (address, code) in [
            (PROXY, proxy_code),
            (APPROVED_IMPL, impl_code(0x01)),
            (UNAPPROVED_IMPL, impl_code(0x02)),
        ] {
            cache.insert_account_info(
                address,
                AccountInfo {
                    code: Some(Bytecode::new_raw(Bytes::from(code))),
                    ..Default::default()
                },
            );
        }
        State::builder().with_database(cache).with_bundle_update().build()
    }

    /// The same accounts in the in-memory database shape the audit path's own EVM is built
    /// over.
    fn audit_path_db(proxy_code: Vec<u8>) -> InMemoryDB {
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            DEPOSITOR,
            AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
        );
        for (address, code) in [
            (PROXY, proxy_code),
            (APPROVED_IMPL, impl_code(0x01)),
            (UNAPPROVED_IMPL, impl_code(0x02)),
        ] {
            db.insert_account_info(
                address,
                AccountInfo {
                    code: Some(Bytecode::new_raw(Bytes::from(code))),
                    ..Default::default()
                },
            );
        }
        db
    }

    /// What the candidate simulation loop observed and decided. Driven through the real
    /// `execute_best_transactions`, not a helper: the EVM, the inspector, the capture window
    /// and the screening call are all the production ones.
    fn candidate_path(raw: Vec<RawRule>, proxy_code: Vec<u8>) -> (Vec<CallObservation>, Screen) {
        let filter = filter_with(raw, test_config());
        let ctx = ctx_with(Some(filter), Vec::new());
        let signer = crate::tests::funded_signer();
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

        LAST_SCREEN_DECISION.with(|d| d.set(None));
        LAST_CANDIDATE_OBSERVATIONS.with(|o| *o.borrow_mut() = None);
        ctx.execute_best_transactions(&mut info, &mut state, &mut best, &pool, limits)
            .expect("the candidate loop runs");

        (
            LAST_CANDIDATE_OBSERVATIONS
                .with(|o| o.borrow().clone())
                .expect("the candidate's capture completed, so its observations are recorded"),
            LAST_SCREEN_DECISION
                .with(|d| d.get())
                .expect("the candidate reached the screening decision"),
        )
    }

    /// What the sequencer/deposit loop observed and decided, through the real
    /// `execute_sequencer_transactions`.
    fn deposit_path(
        raw: Vec<RawRule>,
        proxy_code: Vec<u8>,
    ) -> (Vec<CallObservation>, DepositIdentification) {
        reset_seams();
        LAST_DEPOSIT_OBSERVATIONS.with(|o| *o.borrow_mut() = None);
        let filter = filter_with(raw, test_config());
        let ctx = ctx_with(Some(filter), vec![deposit_to(PROXY)]);
        let mut state = fixture_state(None, proxy_code);
        let facts = run(&ctx, &mut state);
        assert_eq!(facts.included.len(), 1, "the fixture deposit is included");
        (
            LAST_DEPOSIT_OBSERVATIONS
                .with(|o| o.borrow().clone())
                .expect("the deposit's capture completed, so its observations are recorded"),
            LAST_DEPOSIT_IDENTIFICATION
                .with(|d| d.get())
                .expect("the deposit reached the identification step"),
        )
    }

    /// What the audit path observed and decided, driven through its PRODUCTION request
    /// pipeline: the real wire request, the real request-level validation and rule
    /// compilation (`prepare_audit_batch_plan`), the real inspector construction and the real
    /// batch body (`run_audit_batch`) — which is what opens the capture window, maps the
    /// capture outcome to an observation set, adjudicates and decides the commit.
    ///
    /// The returned observations are the ones the production batch itself fed to screening,
    /// recorded inside that batch rather than re-derived here. A production path that
    /// discarded its capture, or mapped a verdict wrongly, therefore cannot satisfy this.
    ///
    /// The handler's own `audit_transactions` is not used only because it needs a node
    /// backend to anchor state; everything after that anchoring is the production path.
    fn audit_path(
        approved: &[B256],
        proxy_code: Vec<u8>,
    ) -> (Vec<CallObservation>, Verdict, Option<CallstackIdentification>, u64) {
        audit_path_with_rule(proxy_impl_allowlist_rule_json(approved), proxy_code)
    }

    /// [`audit_path`] for an arbitrary wire rule, so a test can drive a rule of another shape
    /// through the same production pipeline.
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

        let plan = prepare_audit_batch_plan(&request)
            .expect("the fixture request passes every request-level check");
        let txs: Vec<_> = request
            .txs
            .iter()
            .map(|req| build_deposit_tx(req).expect("the fixture request builds"))
            .collect();
        let inspector = audit_capture_inspector(
            plan.rules(),
            AuditCaptureConfig { max_callstack_observations_per_tx: GENEROUS_BUDGET },
        )
        .expect("the fixture rule declares an alias");
        let env = EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        );
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            audit_path_db(proxy_code),
            env,
            inspector,
        );

        // Drop anything an earlier leg recorded, so what comes back is this batch's own.
        let _ = observation_recorder::take();
        let mut executed: Vec<Option<AuditResult>> = vec![None];
        run_audit_batch(&mut evm, &plan, &txs, &request.txs, &mut executed)
            .expect("the production batch executes the fixture transaction");
        let recorded = observation_recorder::take();
        assert_eq!(recorded.len(), 1, "one transaction, one recorded observation set");
        let observed = recorded[0]
            .clone()
            .expect("the production batch fed screening an AVAILABLE observation set");
        let result = executed[0].as_ref().expect("the production batch classified the tx");
        assert_eq!(result.source_hash, source_hash, "the result belongs to this request");
        // The production batch also decides whether to commit. The depositor's nonce is the
        // observable: executing the deposit bumps it, so a committed batch leaves 1 and a
        // discarded one leaves 0 — independent of what the implementation's code does, which
        // this shared fixture deliberately keeps side-effect free. Measured, not assumed: the
        // same fixture through the same production batch gives nonce 1 on allow and 0 on deny.
        let (verdict, identification) = (result.verdict, result.callstack_identification);
        let depositor_nonce = evm
            .db_mut()
            .basic(DEPOSITOR)
            .expect("the depositor account is readable")
            .expect("the fixture seeds the depositor")
            .nonce;
        (observed, verdict, identification, depositor_nonce)
    }

    /// ONE fixture — the same observed proxy, the same implementation, the same (empty)
    /// calldata and the same seeded accounts — driven through all three real execution paths
    /// yields the SAME observed call frames and the SAME decision, in both directions of the
    /// rule pattern.
    ///
    /// The observed frames are compared, not just the three decisions: two paths can agree on
    /// a decision while resting on different observations, and it is the observation equality
    /// that "equal identification" actually asserts. The snapshot STAMP is deliberately not
    /// part of the comparison — each path stamps its observations with its own rule-set
    /// identity (the builder's installed snapshot, the audit path's per-request set), and
    /// requiring those to be equal would assert something the design never claims.
    #[tokio::test]
    async fn all_three_paths_observe_the_same_frames_and_reach_the_same_decision() {
        let approved = code_hash(&impl_code(0x01));
        for executed_impl in [UNAPPROVED_IMPL, APPROVED_IMPL] {
            let objected = executed_impl == UNAPPROVED_IMPL;
            let proxy_code = delegating_proxy_code(executed_impl, 1);
            let raw = vec![proxy_impl_allowlist_rule(&[approved])];
            // The audit path compiles the rules carried in its own request, inside
            // `prepare_audit_batch_plan`; this only pins that the shared rule source really
            // is a clean rule, so a silent rejection could not make the comparison vacuous
            // on any path.
            assert!(
                load_rules(rcs_filter::config::MIN_PROTOCOL_VERSION, 0, raw.clone())
                    .rejected
                    .is_empty(),
                "the fixture rule must load clean"
            );

            let (candidate_observed, screen) = candidate_path(raw.clone(), proxy_code.clone());
            let (deposit_observed, identification) = deposit_path(raw.clone(), proxy_code.clone());
            let (audit_observed, verdict, audit_identification, audit_depositor_nonce) =
                audit_path(&[approved], proxy_code.clone());

            // The fixture really produced the frames the rule is about — the proxy running
            // its own code, then its first-level delegatecall — so the equality below cannot
            // be satisfied by three empty sets.
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
                candidate_observed, deposit_observed,
                "the candidate loop and the sequencer loop must observe the same frames"
            );
            assert_eq!(
                deposit_observed, audit_observed,
                "the sequencer loop and the audit path must observe the same frames"
            );

            if objected {
                assert_eq!(screen, Screen::Deny, "the candidate loop rejects");
                assert_eq!(
                    identification,
                    DepositIdentification::DenyWorthy,
                    "the sequencer loop reports a deny-worthy deposit"
                );
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
                assert_eq!(
                    identification,
                    DepositIdentification::Allow,
                    "the sequencer loop reports an allowed deposit"
                );
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

    mod four_field_cross_path_tests;

    /// An **ordered** deposit batch in which an earlier deposit upgrades the proxy's
    /// implementation and a later deposit is judged against the **new** one.
    ///
    /// Nested here so it reuses the very same real batch body, real capture window, real
    /// verdict mapping and real commit decision the comparison above drives. What it adds is
    /// an upgradeable proxy: the fixtures above hard-code the delegate target into the
    /// proxy's bytecode, so they cannot express an upgrade at all — "the later deposit is
    /// judged against the new implementation" is only a real claim if the pointer the proxy
    /// reads can actually change, and change because an earlier transaction in the same
    /// batch committed that change.
    ///
    /// Two ordinary batch tests cannot stand in for this. A batch of independent deposits
    /// shows ordering but no carried state; a pair of separately executed frames shows two
    /// verdicts but not that the second one saw the first one's write.
    mod ordered_upgrade_batch_tests {
        use super::*;

        /// The storage slot the upgradeable proxy keeps its implementation address in.
        const IMPL_SLOT: U256 = U256::ZERO;

        /// An upgradeable proxy, in runtime bytecode.
        ///
        /// With calldata it performs the upgrade and nothing else:
        /// `SSTORE(IMPL_SLOT, calldataload(0))` — no delegatecall, so it produces no
        /// direct-delegatecall observation at all. With empty calldata it delegатecalls into
        /// whatever address the slot currently holds, which is the frame a `callstack` alias
        /// on this proxy observes. That is what makes the second deposit's verdict a function
        /// of the first deposit's committed write rather than of the fixture.
        fn upgradeable_proxy_code() -> Vec<u8> {
            vec![
                0x36, // CALLDATASIZE
                0x60, 0x13, // PUSH1 19 -> the upgrade JUMPDEST
                0x57, // JUMPI  (calldata present => upgrade)
                // --- delegate branch: retSize, retOffset, argsSize, argsOffset ---
                0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, //
                0x60, 0x00, // PUSH1 IMPL_SLOT
                0x54, // SLOAD -> implementation address
                0x5a, // GAS
                0xf4, // DELEGATECALL
                0x50, // POP
                0x00, // STOP
                // --- upgrade branch (offset 19) ---
                0x5b, // JUMPDEST
                0x60, 0x00, // PUSH1 0 (calldata offset)
                0x35, // CALLDATALOAD -> the new implementation address, word-aligned
                0x60, 0x00, // PUSH1 IMPL_SLOT
                0x55, // SSTORE
                0x00, // STOP
            ]
        }

        /// The audit path's own database, with the upgradeable proxy and its initial pointer.
        fn upgradeable_db(initial_impl: Address) -> InMemoryDB {
            let mut db = audit_path_db(upgradeable_proxy_code());
            db.insert_account_storage(PROXY, IMPL_SLOT, initial_impl.into_word().into())
                .expect("the fixture proxy's slot is writable");
            db
        }

        /// One deposit request body. `data` empty means "call the proxy"; `data` carrying a
        /// word-aligned address means "upgrade the proxy to it".
        fn deposit_request(source_hash: B256, data: &str) -> serde_json::Value {
            serde_json::json!({
                "source_hash": format!("{source_hash:#x}"),
                "from": format!("{DEPOSITOR:#x}"),
                "to": format!("{PROXY:#x}"),
                "mint": "0",
                "value": "0",
                "gas_limit": 1_000_000,
                "is_system_transaction": false,
                "data": data,
            })
        }

        /// The 32-byte, right-aligned calldata word that upgrades the proxy to `target`.
        fn upgrade_calldata(target: Address) -> String {
            format!("0x{}", alloy_primitives::hex::encode(target.into_word()))
        }

        /// What the real ordered batch did: per-transaction verdicts, the observation set each
        /// execution actually saw, and the proxy's implementation slot afterwards.
        struct BatchOutcome {
            verdicts: Vec<Verdict>,
            observed: Vec<Option<Vec<CallObservation>>>,
            implementation_after: Address,
            depositor_nonce: u64,
        }

        /// Drives ONE ordered batch of deposit requests through the production audit batch
        /// body, against a proxy whose pointer starts at `initial_impl`.
        fn run_ordered_batch(
            approved: &[B256],
            initial_impl: Address,
            deposits: Vec<serde_json::Value>,
        ) -> BatchOutcome {
            let count = deposits.len();
            let request: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
                "rules": [proxy_impl_allowlist_rule_json(approved)],
                "txs": deposits,
            }))
            .expect("the fixture request satisfies the audit RPC's own wire schema");
            assert_eq!(request.txs.len(), count, "every deposit reached the request");

            let plan = prepare_audit_batch_plan(&request)
                .expect("the fixture request passes every request-level check");
            let txs: Vec<_> = request
                .txs
                .iter()
                .map(|req| build_deposit_tx(req).expect("the fixture request builds"))
                .collect();
            let inspector = audit_capture_inspector(
                plan.rules(),
                AuditCaptureConfig { max_callstack_observations_per_tx: GENEROUS_BUDGET },
            )
            .expect("the fixture rule declares an alias");
            let env = EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            );
            let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
                upgradeable_db(initial_impl),
                env,
                inspector,
            );

            let _ = observation_recorder::take();
            let mut executed: Vec<Option<AuditResult>> = vec![None; count];
            run_audit_batch(&mut evm, &plan, &txs, &request.txs, &mut executed)
                .expect("the production batch executes the fixture batch");
            let observed = observation_recorder::take();
            assert_eq!(observed.len(), count, "one recorded observation set per transaction");

            let verdicts = executed
                .iter()
                .enumerate()
                .map(|(idx, result)| {
                    result
                        .as_ref()
                        .unwrap_or_else(|| panic!("transaction {idx} was classified"))
                        .verdict
                })
                .collect();
            // Read the pointer back out of the batch's OWN database, so the state assertion is
            // about what the batch committed rather than about what it reported.
            let slot = evm
                .db_mut()
                .storage(PROXY, IMPL_SLOT)
                .expect("the proxy's implementation slot is readable");
            let depositor_nonce = evm
                .db_mut()
                .basic(DEPOSITOR)
                .expect("the depositor account is readable")
                .expect("the fixture seeds the depositor")
                .nonce;
            BatchOutcome {
                verdicts,
                observed,
                implementation_after: Address::from_word(slot.into()),
                depositor_nonce,
            }
        }

        /// The one direct-delegatecall observation of the proxy in `set`, if any.
        fn delegatecall_of_proxy(set: &Option<Vec<CallObservation>>) -> Option<&CallObservation> {
            set.as_ref()?.iter().find(|observation| {
                observation.kind == CallObservationKind::DirectDelegatecall
                    && observation.address == PROXY
            })
        }

        /// The direction that matters: the batch starts with an **approved**
        /// implementation installed, the first deposit upgrades it to an **unapproved** one,
        /// and the second deposit — which merely calls the proxy — is denied.
        ///
        /// Three independent things are asserted, because any one of them alone could be
        /// satisfied for the wrong reason:
        ///   1. the state update really happened and really was committed (the slot the batch's
        ///      own database holds afterwards),
        ///   2. the later execution's own observation set names the NEW implementation and its
        ///      executed code hash — this is what "judged against the new implementation"
        ///      means, and it comes from the batch's own recording, not from re-deriving it,
        ///   3. the verdicts.
        #[test]
        fn a_later_deposit_is_judged_against_the_implementation_an_earlier_one_installed() {
            let approved = code_hash(&impl_code(0x01)); // APPROVED_IMPL's code
            let upgrade = B256::repeat_byte(0x41);
            let call = B256::repeat_byte(0x42);

            let outcome = run_ordered_batch(
                &[approved],
                APPROVED_IMPL,
                vec![
                    deposit_request(upgrade, &upgrade_calldata(UNAPPROVED_IMPL)),
                    deposit_request(call, "0x"),
                ],
            );

            // 1. The earlier deposit's write survived into the state the batch carries.
            assert_eq!(
                outcome.implementation_after, UNAPPROVED_IMPL,
                "the earlier deposit's upgrade must be committed, or the later deposit would \
                     be judged against the old implementation for the wrong reason"
            );

            // 2. The upgrade itself delegatecalls nothing, so it produces no candidate frame;
            //    the later execution's own observation names the newly installed one.
            assert!(
                delegatecall_of_proxy(&outcome.observed[0]).is_none(),
                "the upgrade transaction performs no delegatecall: {:?}",
                outcome.observed[0]
            );
            let later = delegatecall_of_proxy(&outcome.observed[1])
                .expect("the later deposit's execution delegatecalled the proxy");
            assert_eq!(
                later.impl_address,
                Some(UNAPPROVED_IMPL),
                "the later execution observed the NEW implementation"
            );
            assert_eq!(
                later.impl_code_hash,
                Some(code_hash(&impl_code(0x02))),
                "and the executed bytecode hash of that new implementation"
            );

            // 3. The verdicts. The upgrade observes no frame at all, so the alias-bearing rule
            //    is suppressed for it and it is allowed (which is what lets it commit); the
            //    later deposit is denied on the implementation the upgrade installed.
            assert_eq!(
                outcome.verdicts,
                vec![Verdict::Allow, Verdict::Deny],
                "the upgrade is allowed and commits; the later deposit is denied"
            );
            assert_eq!(
                outcome.depositor_nonce, 1,
                "exactly one of the two deposits committed its state: the upgrade"
            );
        }

        /// The mirror, and the reason the test above cannot pass for the wrong reason: run the
        /// SAME batch with the pointer starting on the unapproved implementation and the
        /// earlier deposit upgrading it to the approved one, and the later deposit is
        /// **allowed**.
        ///
        /// The two cases differ only in which implementation the earlier deposit installs, so
        /// a later deposit judged against the *initial* pointer — or against the fixture, or
        /// against nothing — would give the same verdict in both and fail here.
        #[test]
        fn the_same_batch_allows_when_the_earlier_deposit_installs_the_approved_one() {
            let approved = code_hash(&impl_code(0x01));
            let outcome = run_ordered_batch(
                &[approved],
                UNAPPROVED_IMPL,
                vec![
                    deposit_request(B256::repeat_byte(0x43), &upgrade_calldata(APPROVED_IMPL)),
                    deposit_request(B256::repeat_byte(0x44), "0x"),
                ],
            );

            assert_eq!(outcome.implementation_after, APPROVED_IMPL);
            let later = delegatecall_of_proxy(&outcome.observed[1])
                .expect("the later deposit delegatecalled the proxy");
            assert_eq!(later.impl_address, Some(APPROVED_IMPL));
            assert_eq!(later.impl_code_hash, Some(code_hash(&impl_code(0x01))));
            assert_eq!(
                outcome.verdicts,
                vec![Verdict::Allow, Verdict::Allow],
                "both deposits are allowed once the approved implementation is installed"
            );
            assert_eq!(outcome.depositor_nonce, 2, "both allowed deposits committed, in order");
        }

        /// Ordering is load-bearing, not incidental: put the call BEFORE the upgrade and the
        /// verdicts swap. Same two transactions, same rule, same initial state — only the
        /// order differs.
        #[test]
        fn reversing_the_order_moves_the_denial_to_the_other_deposit() {
            let approved = code_hash(&impl_code(0x01));
            let outcome = run_ordered_batch(
                &[approved],
                UNAPPROVED_IMPL,
                vec![
                    deposit_request(B256::repeat_byte(0x45), "0x"),
                    deposit_request(B256::repeat_byte(0x46), &upgrade_calldata(APPROVED_IMPL)),
                ],
            );

            assert_eq!(
                outcome.verdicts,
                vec![Verdict::Deny, Verdict::Allow],
                "the call is judged against the pointer in force when IT ran"
            );
            let earlier = delegatecall_of_proxy(&outcome.observed[0])
                .expect("the earlier deposit delegatecalled the proxy");
            assert_eq!(earlier.impl_address, Some(UNAPPROVED_IMPL));
            assert_eq!(outcome.implementation_after, APPROVED_IMPL);
            assert_eq!(
                outcome.depositor_nonce, 1,
                "the denied deposit's state was discarded; the upgrade's was committed"
            );
        }

        /// The duplicate-key cases, completing their "nothing is executed or committed" half on the entry
        /// point that can actually execute something.
        ///
        /// The rule-layer and request-decode cases prove the duplicate key is refused. This
        /// proves what that refusal means in state terms: the identical batch, with one
        /// duplicate key added to the rule text, never reaches execution — so the depositor's
        /// nonce and the proxy's implementation pointer are exactly what they were, while the
        /// same batch without the duplicate does execute and does commit.
        #[test]
        fn a_duplicate_key_in_the_request_executes_and_commits_nothing() {
            let approved = format!("{:#x}", code_hash(&impl_code(0x01)));
            let deposit = serde_json::to_string(&deposit_request(
                B256::repeat_byte(0x47),
                &upgrade_calldata(UNAPPROVED_IMPL),
            ))
            .expect("the fixture deposit serialises");
            let rule = format!(
                r#"{{"id":"proxy-impl-allowlist","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"entry":"{PROXY:#x}"}}}},"condition":{{"!":{{"in":[{{"var":"entry.delegateCodeHash"}},["{approved}"]]}}}},"action":"deny"}}"#
            );

            // Raw request text, so the duplicate key is still present when the decode runs.
            for (tail, label) in [
                (r#","action":"allow""#, "duplicate modelled root key"),
                (
                    r#","callstack":{"direct_delegatecalls":{"entry":"0x00000000000000000000000000000000000000cc"}}"#,
                    "duplicate callstack section",
                ),
            ] {
                let with_duplicate = format!("{}{tail}}}", rule.trim_end_matches('}'));
                let body = format!(r#"{{"rules":[{with_duplicate}],"txs":[{deposit}]}}"#);
                assert!(
                    serde_json::from_str::<AuditTransactionsRequest>(&body).is_err(),
                    "{label}: the request carrying it must not decode, so nothing can run"
                );
            }

            // The alias-duplicate shape, likewise.
            let alias_dup = format!(
                r#"{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"entry":"{PROXY:#x}","entry":"0x00000000000000000000000000000000000000cc"}}}},"condition":true,"action":"deny"}}"#
            );
            assert!(
                serde_json::from_str::<AuditTransactionsRequest>(&format!(
                    r#"{{"rules":[{alias_dup}],"txs":[{deposit}]}}"#
                ))
                .is_err(),
                "a duplicate alias must not decode either"
            );

            // The control: the same batch WITHOUT the duplicate decodes, executes and commits
            // — so the refusals above really did stop something that would otherwise have run.
            let control = run_ordered_batch(
                &[code_hash(&impl_code(0x01))],
                APPROVED_IMPL,
                vec![deposit_request(B256::repeat_byte(0x47), &upgrade_calldata(UNAPPROVED_IMPL))],
            );
            assert_eq!(control.verdicts, vec![Verdict::Allow]);
            assert_eq!(control.depositor_nonce, 1, "the control really did execute and commit");
            assert_eq!(control.implementation_after, UNAPPROVED_IMPL);
        }
    }

    /// Joint acceptance against the locked producer's **genuinely rendered** `GET /rules`
    /// response, driven end to end through the audit RPC's real request decode, real plan,
    /// real EVM execution, real capture window and real verdict mapping — with a NON-EMPTY
    /// `txs`, so the conditions are reached by an execution rather than merely compiled.
    ///
    /// The rendered body is the one the producer's own renderer emitted at
    /// `8944b24f00c058cb5e65090a5c210335d1f186ed`; its provenance, the exact command and its
    /// `sha256` are recorded where it is vendored
    /// (`crates/rcs-filter/tests/data/producer-rendered-rules-response.json`) and in the
    /// rule-layer tests that pin it. This module takes the rendered `rules` array as raw
    /// TEXT — never re-serialised — so the bytes the consumer decodes here are the producer's
    /// own.
    mod producer_rendered_joint_acceptance_tests {
        use super::*;

        /// The producer's own `vaultProxy` address, from its rendered rules.
        const PRODUCER_VAULT_PROXY: Address =
            Address::new(alloy_primitives::hex!("00000000000000000000000000000000000ab001"));
        /// An address no rendered rule declares, for the suppression control.
        const UNDECLARED_TARGET: Address =
            Address::new(alloy_primitives::hex!("00000000000000000000000000000000000f00f0"));

        /// The producer's rendered `GET /rules` body, verbatim.
        const RENDERED_RESPONSE: &str =
            include_str!("../../../../rcs-filter/tests/data/producer-rendered-rules-response.json");

        /// The rendered `rules` array as raw text, sliced out without parsing so the rule
        /// bytes the consumer decodes are byte-for-byte the producer's.
        fn rendered_rules_array() -> &'static str {
            const KEY: &str = "\"rules\":";
            let start =
                RENDERED_RESPONSE.find(KEY).expect("the rendered body carries rules") + KEY.len();
            let end = RENDERED_RESPONSE.rfind(']').expect("the rules array is closed") + 1;
            let slice = RENDERED_RESPONSE[start..end].trim();
            assert!(
                slice.starts_with('[') && slice.ends_with(']'),
                "the slice must be exactly the rules array: {slice:.40}"
            );
            slice
        }

        /// The audit path's database with the producer's OWN proxy address delegating into
        /// `target`, plus that target's code.
        fn producer_proxy_db(target: Address, target_code: Vec<u8>) -> InMemoryDB {
            let mut db = InMemoryDB::default();
            db.insert_account_info(
                DEPOSITOR,
                AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
            );
            for (address, code) in [
                (PRODUCER_VAULT_PROXY, delegating_proxy_code(target, 1)),
                (target, target_code),
                (UNDECLARED_TARGET, impl_code(0x09)),
            ] {
                db.insert_account_info(
                    address,
                    AccountInfo {
                        code: Some(Bytecode::new_raw(Bytes::from(code))),
                        ..Default::default()
                    },
                );
            }
            db
        }

        /// Decodes the producer's rendered rules plus one deposit to `to` through the audit
        /// RPC's real request decode, then runs the production batch against a real EVM.
        ///
        /// Returns the number of accepted rules, the verdict, the observation set the batch
        /// itself fed to screening, and the depositor nonce afterwards (the commit
        /// observable).
        fn adjudicate_rendered(
            to: Address,
            target: Address,
            target_code: Vec<u8>,
        ) -> (usize, Verdict, Option<Vec<CallObservation>>, u64) {
            let body = format!(
                r#"{{"rules":{rules},"txs":[{{"source_hash":"{source:#x}","from":"{from:#x}","to":"{to:#x}","mint":"0","value":"0","gas_limit":1000000,"is_system_transaction":false,"data":"0x"}}]}}"#,
                rules = rendered_rules_array(),
                source = B256::repeat_byte(0x51),
                from = DEPOSITOR,
                to = to,
            );
            // The production decode, straight from the request text — no Value hop, and the
            // rendered rule bytes still exactly as the producer wrote them.
            let request: AuditTransactionsRequest = serde_json::from_str(&body)
                .expect("the producer's rendered rules must decode at this entry point");
            assert_eq!(request.txs.len(), 1, "the batch really carries a transaction");

            let plan = prepare_audit_batch_plan(&request)
                .expect("every rendered rule must pass every request-level check");
            assert!(
                plan.rules().rejected.is_empty(),
                "no rendered rule may be rejected: {:?}",
                plan.rules().rejected
            );
            let accepted = plan.rules().rules.len();

            let txs: Vec<_> = request
                .txs
                .iter()
                .map(|req| build_deposit_tx(req).expect("the deposit builds"))
                .collect();
            let inspector = audit_capture_inspector(
                plan.rules(),
                AuditCaptureConfig { max_callstack_observations_per_tx: GENEROUS_BUDGET },
            )
            .expect("the rendered set declares aliases, so an inspector is built");
            let env = EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            );
            let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
                producer_proxy_db(target, target_code),
                env,
                inspector,
            );

            let _ = observation_recorder::take();
            let mut executed: Vec<Option<AuditResult>> = vec![None];
            run_audit_batch(&mut evm, &plan, &txs, &request.txs, &mut executed)
                .expect("the production batch executes");
            let recorded = observation_recorder::take();
            assert_eq!(recorded.len(), 1);
            let verdict = executed[0].as_ref().expect("the tx was classified").verdict;
            let nonce = evm
                .db_mut()
                .basic(DEPOSITOR)
                .expect("the depositor is readable")
                .expect("the fixture seeds the depositor")
                .nonce;
            (accepted, verdict, recorded[0].clone(), nonce)
        }

        /// All 7 rendered rules are accepted at the audit RPC entry point, and the condition
        /// of the producer's own `deny-proxy-delegatecall-to-unapproved-implementation` really
        /// fires on a real execution: the proxy delegatecalls into an implementation whose
        /// executed bytecode hash is not on the producer's allowlist, and the deposit is
        /// denied with its state discarded.
        #[test]
        fn the_rendered_rules_are_accepted_and_their_condition_fires_on_a_real_execution() {
            let target = APPROVED_IMPL; // address is irrelevant to this rule; the HASH is not
            let (accepted, verdict, observed, nonce) =
                adjudicate_rendered(PRODUCER_VAULT_PROXY, target, impl_code(0x01));

            assert_eq!(accepted, 7, "all seven rendered rules are active");

            // The execution really produced the frame the rule is about.
            let items = observed.expect("the batch fed screening an AVAILABLE observation set");
            let frame = items
                .iter()
                .find(|o| {
                    o.kind == CallObservationKind::DirectDelegatecall
                        && o.address == PRODUCER_VAULT_PROXY
                })
                .expect("the proxy's direct delegatecall was observed");
            assert_eq!(frame.impl_address, Some(target));
            assert_eq!(
                frame.impl_code_hash,
                Some(code_hash(&impl_code(0x01))),
                "the observed hash is the bytecode that really executed"
            );

            assert_eq!(
                verdict,
                Verdict::Deny,
                "the producer's own allowlist rule must object to an implementation that is \
                     not on it"
            );
            assert_eq!(nonce, 0, "a denied deposit's state is discarded");
        }

        /// The suppression control on the same rendered set: a deposit that touches none of
        /// the three declared proxies is allowed and commits, even though every alias-bearing
        /// rendered rule is a NEGATED allowlist. Without this, the denial above could be an
        /// unconditional objection rather than an evaluated one.
        #[test]
        fn a_deposit_touching_no_declared_proxy_is_allowed_and_commits() {
            let (accepted, verdict, observed, nonce) =
                adjudicate_rendered(UNDECLARED_TARGET, APPROVED_IMPL, impl_code(0x01));
            assert_eq!(accepted, 7);
            let items = observed.expect("the batch fed screening an observation set");
            assert!(
                !items.iter().any(|o| {
                    o.kind == CallObservationKind::DirectDelegatecall
                        && o.address == PRODUCER_VAULT_PROXY
                }),
                "no declared proxy frame was observed: {items:?}"
            );
            assert_eq!(verdict, Verdict::Allow);
            assert_eq!(nonce, 1, "an allowed deposit commits its state");
        }
    }
}
