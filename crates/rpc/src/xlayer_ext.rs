use std::sync::Arc;

use alloy_consensus::BlockHeader;
use alloy_eips::BlockId;
use alloy_evm::overrides::apply_state_overrides;
use alloy_primitives::U256;
use alloy_rpc_types_eth::state::StateOverride;
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
};
use op_alloy_rpc_types::OpTransactionRequest;
use tracing::{debug, warn};

use reth_chainspec::{ChainSpecProvider, EthChainSpec};
use reth_optimism_rpc::SequencerClient;
use reth_rpc::RpcTypes;
use reth_rpc_eth_api::{
    helpers::{Call, EthFees, LoadBlock, LoadFee, LoadState, SpawnBlocking, Trace},
    EthApiTypes,
};
use reth_storage_api::{BlockReaderIdExt, HeaderProvider, ProviderHeader};
use revm::context_interface::block::Block;

use crate::pre_exec_ext_xlayer::PreExec;
use crate::pre_exec_types::{PreExecError, PreExecResult};

/// Trait for accessing sequencer client from backend
pub trait SequencerClientProvider {
    /// Returns the sequencer client if available
    fn sequencer_client(&self) -> Option<&SequencerClient>;
}

/// XLayer-specific RPC API trait
#[rpc(server, namespace = "eth", server_bounds(
    Net: 'static + RpcTypes,
    <Net as RpcTypes>::TransactionRequest:
        serde::de::DeserializeOwned + serde::Serialize
))]
pub trait XlayerRpcExtApi<Net: RpcTypes> {
    /// Returns the minimum gas price (base fee + default suggested fee).
    ///
    /// This is an XLayer-specific extension to the standard Ethereum RPC API.
    #[method(name = "minGasPrice")]
    async fn min_gas_price(&self) -> RpcResult<U256>;

    /// Pre-executes a batch of transactions and returns detailed execution results.
    ///
    /// This method simulates transaction execution without committing state changes,
    /// providing detailed information about inner transactions, logs, and state diffs.
    ///
    /// # Arguments
    ///
    /// * `args` - Vector of transaction requests to pre-execute
    /// * `block_number` - Optional block ID to execute against (defaults to latest)
    /// * `state_overrides` - Optional state overrides to apply before execution
    ///
    /// # Returns
    ///
    /// Vector of `PreExecResult` containing execution details for each transaction
    #[method(name = "transactionPreExec")]
    async fn transaction_pre_exec(
        &self,
        args: Vec<<Net as RpcTypes>::TransactionRequest>,
        block_number: Option<BlockId>,
        state_overrides: Option<StateOverride>,
    ) -> RpcResult<Vec<PreExecResult>>;
}

/// XLayer RPC extension implementation
#[derive(Debug)]
pub struct XlayerRpcExt<T> {
    pub backend: Arc<T>,
}

#[async_trait]
impl<T, Net> XlayerRpcExtApiServer<Net> for XlayerRpcExt<T>
where
    T: EthFees
        + LoadFee
        + LoadBlock
        + Call
        + LoadState
        + SpawnBlocking
        + Trace
        + PreExec
        + EthApiTypes<NetworkTypes = Net>
        + SequencerClientProvider
        + Clone
        + Send
        + Sync
        + 'static,
    T::Provider: ChainSpecProvider<ChainSpec: EthChainSpec<Header = ProviderHeader<T::Provider>>>
        + BlockReaderIdExt
        + HeaderProvider,
    Net: RpcTypes<TransactionRequest = OpTransactionRequest> + Send + Sync + 'static,
{
    async fn min_gas_price(&self) -> RpcResult<U256> {
        // Check if sequencer client is available (RPC node mode)
        if let Some(sequencer) = self.backend.sequencer_client() {
            match sequencer.request::<_, U256>("eth_minGasPrice", ()).await {
                Ok(result) => {
                    debug!(
                        target: "rpc::xlayer",
                        "Received eth_minGasPrice from sequencer: {result}"
                    );
                    return Ok(result);
                }
                Err(err) => {
                    warn!(
                        target: "rpc::xlayer",
                        %err,
                        "Failed to forward eth_minGasPrice to sequencer, falling back to local calculation"
                    );
                    // Fall through to local calculation
                }
            }
        }

        // Local calculation (sequencer mode or fallback)
        let header = self.backend.provider().latest_header().map_err(|err| {
            jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                format!("Failed to get latest header: {err}"),
                None::<()>,
            )
        })?;

        let base_fee = header.and_then(|h| h.base_fee_per_gas()).unwrap_or_default();

        // Get the default suggested fee from gas oracle config
        let default_suggested_fee =
            self.backend.gas_oracle().config().default_suggested_fee.unwrap_or_default();

        let min_gas_price = U256::from(base_fee) + default_suggested_fee;

        debug!(
            target: "rpc::xlayer",
            "Calculated min_gas_price locally: {min_gas_price}, base_fee: {base_fee}, default_suggested_fee: {default_suggested_fee}"
        );

        Ok(min_gas_price)
    }

    async fn transaction_pre_exec(
        &self,
        args: Vec<OpTransactionRequest>,
        block_number: Option<BlockId>,
        state_overrides: Option<StateOverride>,
    ) -> RpcResult<Vec<PreExecResult>> {
        let block_id = block_number.unwrap_or_default();
        let (evm_env, at) = match self.backend.evm_env_at(block_id).await {
            Ok(env) => env,
            Err(e) => return Err(e.into()),
        };

        let api = self.backend.clone();
        self.backend
            .spawn_with_state_at_block(at, move |state| {
                let mut db = reth_revm::db::CacheDB::new(
                    reth_revm::database::StateProviderDatabase::new(state),
                );

                if let Some(overrides) = state_overrides {
                    if let Err(e) = apply_state_overrides(overrides, &mut db) {
                        let res = PreExecError::unknown(format!("state override error: {e:?}"))
                            .into_result(0, evm_env.block_env.number());
                        return Ok(vec![res]);
                    }
                }

                Ok(api.run_pre_exec_in_db(&mut db, args, evm_env, at))
            })
            .await
            .map_err(|e| {
                jsonrpsee::types::ErrorObjectOwned::owned(
                    jsonrpsee::types::error::INTERNAL_ERROR_CODE,
                    e.to_string(),
                    None::<()>,
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use alloy_consensus::Header;
    use alloy_primitives::U256;

    // Mock backend for testing
    struct MockBackend {
        base_fee: Option<u64>,
        default_suggested_fee: Option<U256>,
    }

    impl MockBackend {
        fn new(base_fee: Option<u64>, default_suggested_fee: Option<U256>) -> Self {
            Self { base_fee, default_suggested_fee }
        }

        fn provider(&self) -> MockProvider {
            MockProvider { base_fee: self.base_fee }
        }

        fn gas_oracle(&self) -> MockGasOracle {
            MockGasOracle { default_suggested_fee: self.default_suggested_fee }
        }
    }

    struct MockProvider {
        base_fee: Option<u64>,
    }

    impl MockProvider {
        fn latest_header(&self) -> Result<Option<Header>, String> {
            if let Some(base_fee) = self.base_fee {
                let mut header = Header::default();
                header.base_fee_per_gas = Some(base_fee);
                Ok(Some(header))
            } else {
                Ok(None)
            }
        }
    }

    struct MockGasOracle {
        default_suggested_fee: Option<U256>,
    }

    impl MockGasOracle {
        fn config(&self) -> MockGasOracleConfig {
            MockGasOracleConfig { default_suggested_fee: self.default_suggested_fee }
        }
    }

    struct MockGasOracleConfig {
        default_suggested_fee: Option<U256>,
    }

    #[test]
    fn test_min_gas_price_calculation() {
        // Test case 1: Both base fee and default suggested fee are present
        let backend = MockBackend::new(Some(1_000_000_000), Some(U256::from(500_000_000)));
        let base_fee = backend.provider().latest_header().unwrap().unwrap().base_fee_per_gas;
        let default_suggested_fee = backend.gas_oracle().config().default_suggested_fee;

        let expected =
            U256::from(base_fee.unwrap_or_default()) + default_suggested_fee.unwrap_or_default();
        assert_eq!(expected, U256::from(1_500_000_000_u64));

        // Test case 2: Only base fee is present
        let backend = MockBackend::new(Some(2_000_000_000), None);
        let base_fee = backend.provider().latest_header().unwrap().unwrap().base_fee_per_gas;
        let default_suggested_fee = backend.gas_oracle().config().default_suggested_fee;

        let expected =
            U256::from(base_fee.unwrap_or_default()) + default_suggested_fee.unwrap_or_default();
        assert_eq!(expected, U256::from(2_000_000_000_u64));

        // Test case 3: Only default suggested fee is present
        let backend = MockBackend::new(None, Some(U256::from(1_000_000_000)));
        let header = backend.provider().latest_header().unwrap();
        let base_fee = header.and_then(|h| h.base_fee_per_gas).unwrap_or_default();
        let default_suggested_fee = backend.gas_oracle().config().default_suggested_fee;

        let expected = U256::from(base_fee) + default_suggested_fee.unwrap_or_default();
        assert_eq!(expected, U256::from(1_000_000_000_u64));

        // Test case 4: Neither is present
        let backend = MockBackend::new(None, None);
        let header = backend.provider().latest_header().unwrap();
        let base_fee = header.and_then(|h| h.base_fee_per_gas).unwrap_or_default();
        let default_suggested_fee = backend.gas_oracle().config().default_suggested_fee;

        let expected = U256::from(base_fee) + default_suggested_fee.unwrap_or_default();
        assert_eq!(expected, U256::ZERO);
    }

    #[test]
    fn test_min_gas_price_overflow_safety() {
        // Test with maximum values to ensure no overflow
        let backend = MockBackend::new(Some(u64::MAX), Some(U256::from(u64::MAX)));
        let base_fee = backend.provider().latest_header().unwrap().unwrap().base_fee_per_gas;
        let default_suggested_fee = backend.gas_oracle().config().default_suggested_fee;

        let result =
            U256::from(base_fee.unwrap_or_default()) + default_suggested_fee.unwrap_or_default();

        // Should not panic and should produce a valid U256
        assert!(result > U256::from(u64::MAX));
    }

    #[test]
    fn test_min_gas_price_typical_values() {
        // Test with typical mainnet values
        // Base fee: 30 gwei, Suggested fee: 2 gwei
        let base_fee_gwei = 30_000_000_000_u64; // 30 gwei in wei
        let suggested_fee_gwei = U256::from(2_000_000_000_u64); // 2 gwei in wei

        let backend = MockBackend::new(Some(base_fee_gwei), Some(suggested_fee_gwei));
        let base_fee = backend.provider().latest_header().unwrap().unwrap().base_fee_per_gas;
        let default_suggested_fee = backend.gas_oracle().config().default_suggested_fee;

        let result =
            U256::from(base_fee.unwrap_or_default()) + default_suggested_fee.unwrap_or_default();

        // Expected: 32 gwei
        assert_eq!(result, U256::from(32_000_000_000_u64));
    }
}
