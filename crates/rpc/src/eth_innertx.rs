use std::collections::HashMap;

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{
    hex::{FromHex, FromHexError},
    FixedBytes, B256,
};
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
    types::{error::INVALID_PARAMS_CODE, ErrorObjectOwned},
};

use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_provider::TransactionsProvider;
use reth_rpc_convert::RpcConvert;
use reth_rpc_eth_api::{helpers::SpawnBlocking, EthApiTypes, RpcNodeCore};
use reth_storage_api::BlockIdReader;

use xlayer_flashblocks::FlashblockStateCache;
use xlayer_innertx::cache::FlashblocksInnerTxCache;
use xlayer_innertx::{
    db_utils::{read_table_block, read_table_tx},
    innertx_inspector::InternalTransaction,
};

fn string_to_b256(hex_str: &str) -> Result<B256, FromHexError> {
    let hex = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let fb: FixedBytes<32> = FixedBytes::from_hex(hex)?;
    Ok(B256::from(fb))
}

const TX_HASH_LENGTH: usize = 66;

#[rpc(server, namespace = "eth")]
pub trait FlashblocksInnerTxApi {
    #[method(name = "getInternalTransactions")]
    async fn get_internal_transactions(
        &self,
        tx_hash: String,
    ) -> RpcResult<Vec<InternalTransaction>>;

    #[method(name = "getBlockInternalTransactions")]
    async fn get_block_internal_transactions(
        &self,
        block_number: BlockNumberOrTag,
    ) -> RpcResult<HashMap<String, Vec<InternalTransaction>>>;
}

/// Innertx RPC handler with flashblocks cache overlay.
///
/// Checks the flashblocks innertx cache first (for pending/confirmed blocks
/// ahead of the canonical chain), then falls back to the MDBX innertx
/// database using `spawn_blocking_io_fut` for proper I/O scheduling.
///
/// Uses [`FlashblockStateCache`] to resolve `Pending`/`Latest` block tags
/// to flashblock hashes before cache lookup.
#[derive(Debug)]
pub struct FlashblocksInnerTxExt<N: RpcNodeCore, Rpc: RpcConvert> {
    eth_api: OpEthApi<N, Rpc>,
    innertx_cache: Option<FlashblocksInnerTxCache>,
    flashblocks_state: Option<FlashblockStateCache<OpPrimitives>>,
}

impl<N: RpcNodeCore, Rpc: RpcConvert> FlashblocksInnerTxExt<N, Rpc> {
    /// Creates a new [`FlashblocksInnerTxExt`].
    pub fn new(
        eth_api: OpEthApi<N, Rpc>,
        innertx_cache: Option<FlashblocksInnerTxCache>,
        flashblocks_state: Option<FlashblockStateCache<OpPrimitives>>,
    ) -> Self {
        Self { eth_api, innertx_cache, flashblocks_state }
    }

    /// Resolves a [`BlockNumberOrTag`] to a block hash using the flashblocks
    /// state cache. Handles `Pending`, `Latest`, and numeric tags.
    fn resolve_flashblock_hash(&self, tag: BlockNumberOrTag) -> Option<B256> {
        let bar = self.flashblocks_state.as_ref()?.get_rpc_block(tag)?;
        Some(bar.block.hash())
    }
}

#[async_trait]
impl<N, Rpc> FlashblocksInnerTxApiServer for FlashblocksInnerTxExt<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError>,
    OpEthApi<N, Rpc>: SpawnBlocking
        + EthApiTypes<Error = OpEthApiError>
        + RpcNodeCore<Primitives = OpPrimitives>
        + Send
        + Sync
        + 'static,
{
    async fn get_internal_transactions(
        &self,
        tx_hash: String,
    ) -> RpcResult<Vec<InternalTransaction>> {
        if tx_hash.len() != TX_HASH_LENGTH || !tx_hash.starts_with("0x") {
            return Err(ErrorObjectOwned::owned(
                INVALID_PARAMS_CODE,
                "Invalid transaction hash format",
                None::<()>,
            ));
        }

        let hash = string_to_b256(&tx_hash).map_err(|_| {
            ErrorObjectOwned::owned(INVALID_PARAMS_CODE, "Invalid transaction hash", None::<()>)
        })?;

        // Check flashblocks innertx cache first (pre-canonical data).
        if let Some(traces) = self.innertx_cache.as_ref().and_then(|c| c.get_tx_traces(&hash)) {
            return Ok(traces);
        }

        // Provider reads + MDBX innertx lookup on blocking I/O pool.
        self.eth_api
            .spawn_blocking_io_fut(move |this| async move {
                // Verify the transaction exists on-chain.
                match this.provider().transaction_by_hash(hash) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return Err(OpEthApiError::from(
                            reth_rpc_eth_types::EthApiError::TransactionNotFound,
                        ))
                    }
                    Err(_) => {
                        return Err(OpEthApiError::from(
                            reth_rpc_eth_types::EthApiError::InternalEthError,
                        ))
                    }
                }

                read_table_tx(hash).map_err(|_| {
                    OpEthApiError::from(reth_rpc_eth_types::EthApiError::InternalEthError)
                })
            })
            .await
            .map_err(|e| -> jsonrpsee_types::error::ErrorObject<'static> { e.into() })
    }

    async fn get_block_internal_transactions(
        &self,
        block_number: BlockNumberOrTag,
    ) -> RpcResult<HashMap<String, Vec<InternalTransaction>>> {
        // Resolve block tag via flashblocks state cache (handles Pending/Latest/Number)
        // and check innertx cache by the resolved hash.
        if let Some(block_hash) = self.resolve_flashblock_hash(block_number)
            && let Some(traces) =
                self.innertx_cache.as_ref().and_then(|c| c.get_block_traces_by_hash(&block_hash))
        {
            return Ok(traces);
        }

        // Clone cache ref for use inside the blocking closure.
        let innertx_cache = self.innertx_cache.clone();

        // Provider reads + MDBX innertx lookup on blocking I/O pool.
        self.eth_api
            .spawn_blocking_io_fut(move |this| async move {
                // Resolve block hash from canonical chain.
                let block_hash: B256 = this
                    .provider()
                    .block_hash_for_id(block_number.into())
                    .map_err(|_| {
                        OpEthApiError::from(reth_rpc_eth_types::EthApiError::InternalEthError)
                    })?
                    .ok_or_else(|| {
                        OpEthApiError::from(reth_rpc_eth_types::EthApiError::HeaderNotFound(
                            block_number.into(),
                        ))
                    })?;

                // Check cache by canonical hash (block may have been confirmed
                // between the tag resolution above and now).
                if let Some(traces) =
                    innertx_cache.as_ref().and_then(|c| c.get_block_traces_by_hash(&block_hash))
                {
                    return Ok(traces);
                }

                // Fall back to MDBX innertx database.
                let block_txs = read_table_block(block_hash).map_err(|_| {
                    OpEthApiError::from(reth_rpc_eth_types::EthApiError::InternalEthError)
                })?;

                let mut result = HashMap::<String, Vec<InternalTransaction>>::default();
                for tx_hash in block_txs {
                    let traces = read_table_tx(tx_hash).map_err(|_| {
                        OpEthApiError::from(reth_rpc_eth_types::EthApiError::InternalEthError)
                    })?;
                    result.insert(tx_hash.to_string(), traces);
                }
                Ok(result)
            })
            .await
            .map_err(|e| -> jsonrpsee_types::error::ErrorObject<'static> { e.into() })
    }
}
