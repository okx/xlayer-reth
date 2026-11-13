//! Wrapper for the series' `flashblocks::context` cases: the names their `use super::*;` saw in
//! `src/flashblocks/context.rs`, reached through public paths and the `test-utils` seam.

use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use alloy_eips::eip2718::WithEncoded;
use alloy_eips::Encodable2718;
use alloy_primitives::{Address, Bytes, B256, U256};
use op_revm::OpSpecId;
use rcs_filter::callstack::{CallObservation, CallObservationKind};
use rcs_filter::capture::CallstackCaptureConfig;
use rcs_filter::{FilterHandle, Screen};
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::{Evm, EvmEnv};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpEvmConfig, OpNextBlockEnvAttributes};
use reth_optimism_node::OpPayloadBuilderAttributes;
use reth_optimism_payload_builder::config::{OpDAConfig, OpGasLimitConfig};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_payload_builder::PayloadId;
use reth_primitives_traits::SealedHeader;
use reth_revm::State;
use reth_transaction_pool::TransactionPool;
use xlayer_builder::flashblocks::test_seams::*;
use xlayer_builder::flashblocks::FlashblocksBuilderCtx;
use xlayer_builder::metrics::BuilderMetrics;

mod capture_decision_tests {
    use super::*;

    /// Runs `f` under a capturing `tracing` subscriber and returns its result plus everything it
    /// logged, so a test can assert the level, target, and exact bounded fields of a diagnostic and
    /// prove no content leaks. One helper shared by the capture-diagnostic tests (R9 non-blocking
    /// cleanup: previously each test hand-rolled its own writer).
    pub(super) fn capture_tracing<R>(f: impl FnOnce() -> R) -> (R, String) {
        use std::io::Write;
        use std::sync::Mutex;

        #[derive(Clone)]
        struct BufWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for BufWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl tracing_subscriber::fmt::MakeWriter<'_> for BufWriter {
            type Writer = BufWriter;
            fn make_writer(&self) -> BufWriter {
                self.clone()
            }
        }

        let buf = BufWriter(Arc::new(Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_target(true)
            .with_writer(buf.clone())
            .finish();
        let out = tracing::subscriber::with_default(subscriber, f);
        let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        (out, logged)
    }
}

mod block_snapshot_pinning_tests;

mod deposit_identification_tests;

mod loop_level_fail_closed_tests {
    use super::*;
    use alloy_consensus::TxEip1559;
    use alloy_genesis::Genesis;
    use alloy_primitives::{keccak256, Address, TxKind};
    use alloy_rpc_types_engine::PayloadAttributes;
    use op_alloy_consensus::OpTypedTransaction;
    use op_alloy_rpc_types_engine::OpPayloadAttributes;
    use rcs_filter::rules::{load_rules, RawRule};
    use rcs_filter::test_support::golden;
    use rcs_filter::{BufferStatus, FilterConfig, SystemClock};
    use reth_optimism_txpool::OpPooledTransaction;
    use reth_payload_util::PayloadTransactions;
    use reth_transaction_pool::test_utils::{testing_pool, MockTransaction};
    use reth_transaction_pool::TransactionOrigin;
    use revm::context::{BlockEnv, CfgEnv};
    use revm::database::{CacheDB, EmptyDB};
    use revm::state::{AccountInfo, Bytecode};

    /// A one-shot [`PayloadTransactions`] that yields a single candidate and RECORDS every
    /// `mark_invalid(sender, nonce)` call. `PayloadTransactionsFixed::mark_invalid` is a silent
    /// no-op; this wrapper makes the loop's post-decision `best_txs.mark_invalid` step observable as
    /// SUPPORTING evidence (the Deny-vs-Drop decision itself is asserted via [`LAST_SCREEN_DECISION`]).
    struct RecordingTxs {
        tx: Option<OpPooledTransaction>,
        marked: Vec<(Address, u64)>,
    }
    impl PayloadTransactions for RecordingTxs {
        type Transaction = OpPooledTransaction;
        fn next(&mut self, _ctx: ()) -> Option<OpPooledTransaction> {
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
    fn erc20_transfer_emitter_code() -> Vec<u8> {
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

    /// Builds a real [`FlashblocksBuilderCtx`] with the RCS Filter enabled by `filter`: a JOVIAN EVM
    /// env at base fee 0 (hand-built, so no parent-header/fork coordination is needed) over a chain
    /// spec from the shared test genesis template pinned to chain id 1.
    fn build_ctx(filter: Arc<FilterHandle>) -> FlashblocksBuilderCtx {
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
            payload_attributes: PayloadAttributes {
                timestamp: 1_751_000_002,
                ..Default::default()
            },
            gas_limit: Some(30_000_000),
            ..Default::default()
        };
        let attributes =
            OpPayloadBuilderAttributes::from_rpc_attrs(B256::ZERO, payload_id, rpc_attrs)
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

    mod snapshot_binding_tests;
}
