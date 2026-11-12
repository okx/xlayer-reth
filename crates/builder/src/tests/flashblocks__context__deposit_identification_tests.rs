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
        serde_json::from_str(include_str!("framework/artifacts/genesis.json.tmpl"))
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
#[path = "flashblocks__context__deposit_identification_tests__cross_path_identification_tests.rs"]
mod cross_path_identification_tests;
