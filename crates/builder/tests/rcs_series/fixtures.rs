//! Fixtures shared by the `rcs_series` integration tests: the funded signer, the builder
//! context and the sequencer run helper, plus the observed-proxy accounts and rule shapes used
//! by the candidate, cross-path and sequencer-regression cases.

use std::sync::Arc;

use alloy_consensus::Sealable;
use alloy_eips::eip2718::WithEncoded;
use alloy_eips::Encodable2718;
use alloy_genesis::Genesis;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rpc_types_engine::PayloadAttributes;
use op_alloy_consensus::{OpTxEnvelope, TxDeposit};
use op_alloy_rpc_types_engine::OpPayloadAttributes;
use op_revm::OpSpecId;
use rcs_filter::rules::{load_rules, RawRule};
use rcs_filter::{FilterConfig, FilterHandle, SystemClock};
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::EvmEnv;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpEvmConfig, OpNextBlockEnvAttributes};
use reth_optimism_node::OpPayloadBuilderAttributes;
use reth_optimism_payload_builder::config::{OpDAConfig, OpGasLimitConfig};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_payload_builder::PayloadId;
use reth_primitives_traits::SealedHeader;
use reth_revm::State;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::{CacheDB, EmptyDB};
use revm::state::{AccountInfo, Bytecode};
use tokio_util::sync::CancellationToken;
use xlayer_builder::flashblocks::test_seams::{LoopSeams, Signer};
use xlayer_builder::flashblocks::FlashblocksBuilderCtx;
use xlayer_builder::metrics::BuilderMetrics;

/// The proxy a rule observes. Its own code performs a first-level `DELEGATECALL`.
pub(crate) const PROXY: Address = Address::repeat_byte(0xAA);
/// The implementation the proxy delegates to in the "approved" fixture.
pub(crate) const APPROVED_IMPL: Address = Address::repeat_byte(0xB1);
/// The implementation the proxy delegates to in the "unapproved" fixture.
pub(crate) const UNAPPROVED_IMPL: Address = Address::repeat_byte(0xB2);
/// Depositor.
pub(crate) const DEPOSITOR: Address = Address::repeat_byte(0xC1);

// anvil default key[0]
const FUNDED_PRIVATE_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

/// The funded signer the moved cases use, from the crate's private test harness.
pub(crate) fn funded_signer() -> Signer {
    Signer::try_from_secret(
        FUNDED_PRIVATE_KEY.parse().expect("invalid hardcoded funded private key"),
    )
    .expect("Failed to create signer from hardcoded funded private key")
}

/// `DELEGATECALL(gas, target, 0, 0, 0, 0); STOP` — the proxy's own first-level delegatecall,
/// which is exactly the frame shape a `callstack` selector on the proxy observes.
pub(crate) fn delegating_proxy_code(target: Address, times: usize) -> Vec<u8> {
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
pub(crate) fn impl_code(tag: u8) -> Vec<u8> {
    vec![0x60, tag, 0x50, 0x00] // PUSH1 tag; POP; STOP — distinct code per implementation
}

pub(crate) fn code_hash(code: &[u8]) -> B256 {
    keccak256(code)
}

/// The normative rule pattern, verbatim in shape: observe by **proxy address
/// only**, then deny when the observed implementation code hash is NOT in the approved set.
/// The explicit non-null guard is what keeps an ordinary non-delegatecall call to the proxy
/// from being denied, because `implCodeHash` is null for a plain call observation.
pub(crate) fn proxy_impl_allowlist_rule(approved: &[B256]) -> RawRule {
    serde_json::from_value(proxy_impl_allowlist_rule_json(approved))
        .expect("the normative sample must deserialize")
}

/// The same sample as wire JSON. The audit path receives a real request rather than an
/// already-parsed rule, so both forms are built from THIS one source and cannot drift.
pub(crate) fn proxy_impl_allowlist_rule_json(approved: &[B256]) -> serde_json::Value {
    let approved: Vec<String> = approved.iter().map(|h| format!("{h:#x}")).collect();
    serde_json::json!({
        "id": "proxy-impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "in": [ { "var": "entry.delegateCodeHash" }, approved ] } },
        "action": "deny"
    })
}

pub(crate) fn filter_with(rules: Vec<RawRule>, config: FilterConfig) -> Arc<FilterHandle> {
    // One protocol version covers the whole rule schema, `callstack` included, so the version
    // no longer varies with what a rule declares.
    let set = load_rules(rcs_filter::config::MIN_PROTOCOL_VERSION, 1, rules);
    assert!(set.rejected.is_empty(), "test rules must load clean: {:?}", set.rejected);
    Arc::new(FilterHandle::for_test(config, set, Arc::new(SystemClock)))
}

/// A `FilterConfig` with screening enabled and generous budgets, so a test that is not about
/// a budget cannot trip one by accident.
pub(crate) fn test_config() -> FilterConfig {
    FilterConfig { enabled: true, ..FilterConfig::default() }
}

/// One unsigned L1→L2 deposit targeting `to`, in the shape the attributes carry.
pub(crate) fn deposit_to(to: Address) -> WithEncoded<OpTransactionSigned> {
    let deposit = TxDeposit {
        source_hash: B256::repeat_byte(0x33),
        from: DEPOSITOR,
        to: alloy_primitives::TxKind::Call(to),
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
pub(crate) fn ctx_with(
    filter: Option<Arc<FilterHandle>>,
    deposits: Vec<WithEncoded<OpTransactionSigned>>,
) -> FlashblocksBuilderCtx {
    let mut genesis: Genesis = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/tests/framework/artifacts/genesis.json.tmpl"
    )))
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

/// Builds a real [`FlashblocksBuilderCtx`] with the RCS Filter enabled by `filter`: a JOVIAN EVM
/// env at base fee 0 (hand-built, so no parent-header/fork coordination is needed) over a chain
/// spec from the shared test genesis template pinned to chain id 1.
pub(crate) fn build_ctx(filter: Arc<FilterHandle>) -> FlashblocksBuilderCtx {
    let mut genesis: Genesis = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/tests/framework/artifacts/genesis.json.tmpl"
    )))
    .expect("valid genesis template JSON");
    genesis.config.chain_id = 1;
    genesis.base_fee_per_gas = Some(0u64.into());
    let chain_spec = Arc::new(OpChainSpec::from_genesis(genesis));
    let evm_config = OpEvmConfig::optimism(chain_spec.clone());
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.chain_id = 1;
    let evm_env = EvmEnv::new(cfg, BlockEnv { gas_limit: 30_000_000, ..Default::default() });
    let payload_id = PayloadId::new([0x35; 8]);
    let rpc_attrs = OpPayloadAttributes {
        payload_attributes: PayloadAttributes { timestamp: 1_751_000_002, ..Default::default() },
        gas_limit: Some(30_000_000),
        ..Default::default()
    };
    let attributes = OpPayloadBuilderAttributes::from_rpc_attrs(B256::ZERO, payload_id, rpc_attrs)
        .expect("attrs decode");
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
        filter: Some(Arc::clone(&filter)),
        // The test context pins the snapshot exactly as the real block payload job does.
        rcs_rule_snapshot: Some(filter.rule_snapshot()),
    }
}

/// The chain state every sequencer fixture shares: a funded depositor, the delegating proxy,
/// and both implementations.
pub(crate) fn seeded_state(proxy_code: Vec<u8>) -> State<CacheDB<EmptyDB>> {
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
pub(crate) struct BlockFacts {
    /// Included transactions, in order.
    pub(crate) included: Vec<B256>,
    pub(crate) cumulative_gas_used: u64,
    pub(crate) receipts: Vec<OpReceipt>,
    /// Canonical digest of the merged post-state bundle.
    ///
    /// Deliberately the bundle rather than the state root: the state root is a function of
    /// the post-state, so two runs with equal bundles necessarily produce the same root —
    /// comparing the bundle is strictly stronger and needs no trie fixture.
    pub(crate) post_state: B256,
    /// The depositor's nonce after the run: a committed deposit bumps it to 1.
    pub(crate) depositor_nonce: u64,
}

/// Runs the real `execute_sequencer_transactions` and takes the consensus-visible facts of
/// the produced block.
pub(crate) fn run(ctx: &FlashblocksBuilderCtx, state: &mut State<CacheDB<EmptyDB>>) -> BlockFacts {
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
    let depositor_nonce = bundle
        .state
        .get(&DEPOSITOR)
        .and_then(|account| account.info.as_ref())
        .map_or(0, |i| i.nonce);
    BlockFacts {
        included: info.executed_transactions.iter().map(|tx| tx.tx_hash()).collect(),
        cumulative_gas_used: info.cumulative_gas_used,
        receipts: info.receipts.clone(),
        post_state: keccak256(accounts.join(";").as_bytes()),
        depositor_nonce,
    }
}

/// A one-shot [`PayloadTransactions`] that yields a single candidate and RECORDS every
/// `mark_invalid(sender, nonce)` call. `PayloadTransactionsFixed::mark_invalid` is a silent
/// no-op; this wrapper makes the loop's post-decision `best_txs.mark_invalid` step observable as
/// SUPPORTING evidence (the Deny-vs-Drop decision itself is asserted via [`LAST_SCREEN_DECISION`]).
pub(crate) struct RecordingTxs {
    pub(crate) tx: Option<reth_optimism_txpool::OpPooledTransaction>,
    pub(crate) marked: Vec<(Address, u64)>,
}

impl reth_payload_util::PayloadTransactions for RecordingTxs {
    type Transaction = reth_optimism_txpool::OpPooledTransaction;
    fn next(&mut self, _ctx: ()) -> Option<Self::Transaction> {
        self.tx.take()
    }
    fn mark_invalid(&mut self, sender: Address, nonce: u64) {
        self.marked.push((sender, nonce));
    }
}

/// Runtime bytecode that emits ONE real ERC20
/// `Transfer(from = BRIDGE_ERC20, to = RECIPIENT, value = ONE_TOKEN)` log then `STOP`s. Deployed at
/// `TOKEN_X`, so `log.address == TOKEN_X` and the log matches `RULE_SCENARIO_A`
/// (`transfer.address == TOKEN_X && transfer.from == BRIDGE_ERC20`). `LOG3` pops
/// `offset, size, topic0, topic1, topic2` (offset on top), so the topics are pushed in reverse.
pub(crate) fn erc20_transfer_emitter_code() -> Vec<u8> {
    use rcs_filter::test_support::golden;
    let transfer_sig = keccak256("Transfer(address,address,uint256)"); // topic0
    let from_word = golden::bridge_erc20().into_word(); // topic1 (from)
    let to_word = golden::recipient().into_word(); // topic2 (to)
    let value_bytes = golden::one_token().to_be_bytes::<32>();
    let mut code = Vec::new();
    code.push(0x7f); // PUSH32 value
    code.extend_from_slice(&value_bytes);
    code.extend_from_slice(&[0x60, 0x00, 0x52]); // PUSH1 0x00; MSTORE  → mem[0..32] = value
    code.push(0x7f); // PUSH32 topic2 (to)
    code.extend_from_slice(to_word.as_slice());
    code.push(0x7f); // PUSH32 topic1 (from)
    code.extend_from_slice(from_word.as_slice());
    code.push(0x7f); // PUSH32 topic0 (Transfer signature)
    code.extend_from_slice(transfer_sig.as_slice());
    code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0xa3, 0x00]); // PUSH1 32; PUSH1 0; LOG3; STOP
    code
}
