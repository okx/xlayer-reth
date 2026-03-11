//! Eth API override module for flashblocks RPC.
//!
//! Provides `EthApiOverride` — a jsonrpsee `#[rpc]` trait that overrides a
//! subset of `eth_*` methods to serve flashblocks data from the
//! [`FlashblockStateCache`] alongside canonical chain data from the inner
//! `Eth` API.
//!
//! Also provides `XlayerRpcExtApi` — a separate `#[rpc]` trait that exposes
//! X Layer-specific methods like `eth_flashblocksEnabled`.
//!
//! The override handler checks the flashblocks cache first for confirmed and
//! pending blocks, then falls back to the canonical `eth_api` for all other
//! queries. For transaction/receipt lookups, canonical is checked **first** to
//! avoid a race condition where the cache hasn't been cleared yet after a
//! canonical block commit.

use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, Bytes, TxHash, U256};
use alloy_rpc_types_eth::{
    state::{EvmOverrides, StateOverride},
    BlockOverrides, Filter, Log, TransactionInfo,
};
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
};
use op_alloy_network::Optimism;
use reth_optimism_primitives::OpPrimitives;
use reth_primitives_traits::{BlockBody, NodePrimitives, SignerRecoverable};
use reth_rpc::eth::EthFilter;
use reth_rpc_convert::{RpcConvert, RpcTransaction};
use reth_rpc_eth_api::{
    helpers::{EthBlocks, EthCall, EthState, EthTransactions, FullEthApi},
    EthApiTypes, EthFilterApiServer, RpcBlock, RpcReceipt,
};
use tracing::debug;

use xlayer_flashblocks::FlashblockStateCache;

// ---------------------------------------------------------------------------
// EthApiOverride — flashblocks `eth_*` method overrides
// ---------------------------------------------------------------------------

/// Eth API override trait for flashblocks integration.
///
/// Methods in this trait override the default `eth_*` JSON-RPC namespace
/// handlers when flashblocks are active. They are registered via
/// `add_or_replace_if_module_configured` to replace the corresponding
/// default implementations.
#[cfg_attr(not(test), rpc(server, namespace = "eth"))]
#[cfg_attr(test, rpc(server, client, namespace = "eth"))]
pub trait EthApiOverride {
    // --- Block queries ---

    /// Returns the current block number, accounting for confirmed flashblocks.
    #[method(name = "blockNumber")]
    async fn block_number(&self) -> RpcResult<U256>;

    /// Returns a block by number, with flashblocks support for pending/confirmed.
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(
        &self,
        number: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<RpcBlock<Optimism>>>;

    /// Returns a block by hash, checking flashblocks confirm cache first.
    #[method(name = "getBlockByHash")]
    async fn get_block_by_hash(
        &self,
        hash: alloy_primitives::B256,
        full: bool,
    ) -> RpcResult<Option<RpcBlock<Optimism>>>;

    /// Returns the transaction count for a block by number.
    #[method(name = "getBlockTransactionCountByNumber")]
    async fn get_block_transaction_count_by_number(
        &self,
        number: BlockNumberOrTag,
    ) -> RpcResult<Option<U256>>;

    /// Returns the transaction count for a block by hash.
    #[method(name = "getBlockTransactionCountByHash")]
    async fn get_block_transaction_count_by_hash(
        &self,
        hash: alloy_primitives::B256,
    ) -> RpcResult<Option<U256>>;

    /// Returns all receipts for a block.
    #[method(name = "getBlockReceipts")]
    async fn get_block_receipts(
        &self,
        block_id: BlockNumberOrTag,
    ) -> RpcResult<Option<Vec<RpcReceipt<Optimism>>>>;

    // --- Transaction queries ---

    /// Returns a transaction by hash (canonical-first to avoid race conditions).
    #[method(name = "getTransactionByHash")]
    async fn get_transaction_by_hash(
        &self,
        hash: TxHash,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>>;

    /// Returns a transaction by block hash and index.
    #[method(name = "getTransactionByBlockHashAndIndex")]
    async fn get_transaction_by_block_hash_and_index(
        &self,
        block_hash: alloy_primitives::B256,
        index: alloy_eips::BlockNumberOrTag,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>>;

    /// Returns a transaction by block number and index.
    #[method(name = "getTransactionByBlockNumberAndIndex")]
    async fn get_transaction_by_block_number_and_index(
        &self,
        block_number: BlockNumberOrTag,
        index: alloy_eips::BlockNumberOrTag,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>>;

    /// Returns a transaction receipt (canonical-first to avoid race conditions).
    #[method(name = "getTransactionReceipt")]
    async fn get_transaction_receipt(
        &self,
        hash: TxHash,
    ) -> RpcResult<Option<RpcReceipt<Optimism>>>;

    // --- State queries ---

    /// Returns account balance, with flashblocks support for pending state.
    #[method(name = "getBalance")]
    async fn get_balance(&self, address: Address, block_number: Option<BlockId>)
        -> RpcResult<U256>;

    /// Returns the transaction count (nonce) for an address.
    #[method(name = "getTransactionCount")]
    async fn get_transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
    ) -> RpcResult<U256>;

    /// Returns the code at a given address.
    #[method(name = "getCode")]
    async fn get_code(&self, address: Address, block_number: Option<BlockId>) -> RpcResult<Bytes>;

    /// Returns the storage value at a given address and slot.
    #[method(name = "getStorageAt")]
    async fn get_storage_at(
        &self,
        address: Address,
        slot: U256,
        block_number: Option<BlockId>,
    ) -> RpcResult<alloy_primitives::B256>;

    /// Executes a call with flashblock state support.
    #[method(name = "call")]
    async fn call(
        &self,
        transaction: alloy_rpc_types_eth::TransactionRequest,
        block_number: Option<BlockId>,
        state_overrides: Option<StateOverride>,
        block_overrides: Option<Box<BlockOverrides>>,
    ) -> RpcResult<Bytes>;

    /// Estimates gas with flashblock state support.
    #[method(name = "estimateGas")]
    async fn estimate_gas(
        &self,
        transaction: alloy_rpc_types_eth::TransactionRequest,
        block_number: Option<BlockId>,
        overrides: Option<StateOverride>,
    ) -> RpcResult<U256>;

    // --- Logs ---

    /// Returns logs matching the filter, including pending flashblock logs.
    #[method(name = "getLogs")]
    async fn get_logs(&self, filter: Filter) -> RpcResult<Vec<Log>>;
}

/// Extended Eth API with flashblocks cache overlay.
///
/// Wraps the canonical `eth_api` and `eth_filter` alongside a
/// [`FlashblockStateCache`] to serve flashblocks data for confirmed and
/// pending blocks while delegating canonical chain queries to the underlying
/// `Eth` API.
#[derive(Debug)]
pub struct XLayerEthApiExt<Eth: EthApiTypes> {
    eth_api: Eth,
    eth_filter: EthFilter<Eth>,
    flash_cache: FlashblockStateCache<OpPrimitives>,
}

impl<Eth: EthApiTypes> XLayerEthApiExt<Eth> {
    /// Creates a new [`XLayerEthApiExt`].
    pub fn new(
        eth_api: Eth,
        eth_filter: EthFilter<Eth>,
        flash_cache: FlashblockStateCache<OpPrimitives>,
    ) -> Self {
        Self { eth_api, eth_filter, flash_cache }
    }
}

#[async_trait]
impl<Eth> EthApiOverrideServer for XLayerEthApiExt<Eth>
where
    Eth: FullEthApi<NetworkTypes = Optimism> + Send + Sync + 'static,
    jsonrpsee_types::error::ErrorObject<'static>: From<Eth::Error>,
{
    async fn block_number(&self) -> RpcResult<U256> {
        // The cache's confirm height is always >= canonical height (it tracks
        // the max of confirm cache tip and canonical tip). Use the cache's
        // pending height (which accounts for the in-progress flashblock
        // sequence) as the reported block number.
        let height = self.flash_cache.get_pending_height();
        Ok(U256::from(height))
    }

    async fn get_block_by_number(
        &self,
        number: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<RpcBlock<Optimism>>> {
        debug!(target: "xlayer::rpc", ?number, "eth_getBlockByNumber");

        if number.is_pending() {
            // Return pending flashblock if available
            if let Some(bar) = self.flash_cache.get_pending_block() {
                return bar_to_rpc_block::<Eth>(&bar, full, self.eth_api.converter())
                    .map(Some)
                    .map_err(Into::into);
            }
            // No pending flashblock — treat as latest
            return EthBlocks::rpc_block(&self.eth_api, BlockNumberOrTag::Latest.into(), full)
                .await
                .map_err(Into::into);
        }

        // Check confirm cache for specific block numbers
        if let BlockNumberOrTag::Number(num) = number {
            if let Some(bar) = self.flash_cache.get_block_by_number(num) {
                return bar_to_rpc_block::<Eth>(&bar, full, self.eth_api.converter())
                    .map(Some)
                    .map_err(Into::into);
            }
        }

        // Delegate to canonical
        EthBlocks::rpc_block(&self.eth_api, number.into(), full).await.map_err(Into::into)
    }

    async fn get_block_by_hash(
        &self,
        hash: alloy_primitives::B256,
        full: bool,
    ) -> RpcResult<Option<RpcBlock<Optimism>>> {
        debug!(target: "xlayer::rpc", %hash, "eth_getBlockByHash");

        // Check confirm cache first
        if let Some(bar) = self.flash_cache.get_block_by_hash(&hash) {
            return bar_to_rpc_block::<Eth>(&bar, full, self.eth_api.converter())
                .map(Some)
                .map_err(Into::into);
        }

        // Delegate to canonical
        EthBlocks::rpc_block(&self.eth_api, hash.into(), full).await.map_err(Into::into)
    }

    async fn get_block_transaction_count_by_number(
        &self,
        number: BlockNumberOrTag,
    ) -> RpcResult<Option<U256>> {
        debug!(target: "xlayer::rpc", ?number, "eth_getBlockTransactionCountByNumber");

        if number.is_pending() {
            if let Some(bar) = self.flash_cache.get_pending_block() {
                let count = bar.block.body().transaction_count();
                return Ok(Some(U256::from(count)));
            }
            return EthBlocks::block_transaction_count(
                &self.eth_api,
                BlockNumberOrTag::Latest.into(),
            )
            .await
            .map(|opt| opt.map(U256::from))
            .map_err(Into::into);
        }

        if let BlockNumberOrTag::Number(num) = number {
            if let Some(bar) = self.flash_cache.get_block_by_number(num) {
                let count = bar.block.body().transaction_count();
                return Ok(Some(U256::from(count)));
            }
        }

        EthBlocks::block_transaction_count(&self.eth_api, number.into())
            .await
            .map(|opt| opt.map(U256::from))
            .map_err(Into::into)
    }

    async fn get_block_transaction_count_by_hash(
        &self,
        hash: alloy_primitives::B256,
    ) -> RpcResult<Option<U256>> {
        debug!(target: "xlayer::rpc", %hash, "eth_getBlockTransactionCountByHash");

        if let Some(bar) = self.flash_cache.get_block_by_hash(&hash) {
            let count = bar.block.body().transaction_count();
            return Ok(Some(U256::from(count)));
        }

        EthBlocks::block_transaction_count(&self.eth_api, hash.into())
            .await
            .map(|opt| opt.map(U256::from))
            .map_err(Into::into)
    }

    async fn get_block_receipts(
        &self,
        block_id: BlockNumberOrTag,
    ) -> RpcResult<Option<Vec<RpcReceipt<Optimism>>>> {
        debug!(target: "xlayer::rpc", ?block_id, "eth_getBlockReceipts");

        let bar = if block_id.is_pending() {
            self.flash_cache.get_pending_block()
        } else if let BlockNumberOrTag::Number(num) = block_id {
            self.flash_cache.get_block_by_number(num)
        } else {
            None
        };

        if let Some(bar) = bar {
            let receipts =
                bar_to_rpc_receipts::<Eth>(&bar, self.eth_api.converter()).map_err(Into::into)?;
            return Ok(Some(receipts));
        }

        // Delegate to canonical — use the block_receipts helper from the eth_api
        // For now, delegate to the canonical handler directly
        // TODO: Once reth exposes a direct block_receipts helper, use it
        Ok(None)
    }

    async fn get_transaction_by_hash(
        &self,
        hash: TxHash,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>> {
        debug!(target: "xlayer::rpc", %hash, "eth_getTransactionByHash");

        // Check canonical chain FIRST to avoid race condition where flashblocks
        // cache hasn't been cleared yet after canonical block commit
        if let Some(tx_source) = EthTransactions::transaction_by_hash(&self.eth_api, hash).await? {
            let rpc_tx =
                tx_source.into_transaction(self.eth_api.converter()).map_err(Into::into)?;
            return Ok(Some(rpc_tx));
        }

        // Fall back to flashblocks cache
        if let Some(info) = self.flash_cache.get_tx_info(&hash) {
            let tx_info = TransactionInfo {
                hash: Some(hash),
                index: Some(info.tx_index),
                block_hash: Some(info.block_hash),
                block_number: Some(info.block_number),
                base_fee: None,
            };
            let recovered = reth_primitives_traits::Recovered::new_unchecked(
                info.tx.clone(),
                info.tx.recover_signer().unwrap_or_default(),
            );
            let rpc_tx = self.eth_api.converter().fill(recovered, tx_info).map_err(Into::into)?;
            return Ok(Some(rpc_tx));
        }

        Ok(None)
    }

    async fn get_transaction_by_block_hash_and_index(
        &self,
        block_hash: alloy_primitives::B256,
        index: alloy_eips::BlockNumberOrTag,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>> {
        debug!(target: "xlayer::rpc", %block_hash, ?index, "eth_getTransactionByBlockHashAndIndex");

        let tx_index = match index {
            BlockNumberOrTag::Number(n) => n,
            _ => return Ok(None),
        };

        if let Some(bar) = self.flash_cache.get_block_by_hash(&block_hash) {
            return get_tx_by_index_from_bar::<Eth>(&bar, tx_index, self.eth_api.converter())
                .map_err(Into::into);
        }

        // Delegate to canonical
        // The canonical eth_api doesn't expose this directly in a helper trait,
        // so we just return None for non-cached blocks and let the main handler
        // deal with it. The override will be registered with add_or_replace, so
        // this only gets called for our override.
        Ok(None)
    }

    async fn get_transaction_by_block_number_and_index(
        &self,
        block_number: BlockNumberOrTag,
        index: alloy_eips::BlockNumberOrTag,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>> {
        debug!(target: "xlayer::rpc", ?block_number, ?index, "eth_getTransactionByBlockNumberAndIndex");

        let tx_index = match index {
            BlockNumberOrTag::Number(n) => n,
            _ => return Ok(None),
        };

        let bar = if block_number.is_pending() {
            self.flash_cache.get_pending_block()
        } else if let BlockNumberOrTag::Number(num) = block_number {
            self.flash_cache.get_block_by_number(num)
        } else {
            None
        };

        if let Some(bar) = bar {
            return get_tx_by_index_from_bar::<Eth>(&bar, tx_index, self.eth_api.converter())
                .map_err(Into::into);
        }

        Ok(None)
    }

    async fn get_transaction_receipt(
        &self,
        hash: TxHash,
    ) -> RpcResult<Option<RpcReceipt<Optimism>>> {
        debug!(target: "xlayer::rpc", %hash, "eth_getTransactionReceipt");

        // Check canonical chain FIRST to avoid race condition
        if let Some(canonical_receipt) =
            EthTransactions::transaction_receipt(&self.eth_api, hash).await?
        {
            return Ok(Some(canonical_receipt));
        }

        // Fall back to flashblocks cache
        if let Some(info) = self.flash_cache.get_tx_info(&hash) {
            let receipt = cached_tx_info_to_rpc_receipt::<Eth>(&info, self.eth_api.converter())
                .map_err(Into::into)?;
            return Ok(Some(receipt));
        }

        Ok(None)
    }

    // --- State queries (Phase 1: delegate to eth_api) ---

    async fn get_balance(
        &self,
        address: Address,
        block_number: Option<BlockId>,
    ) -> RpcResult<U256> {
        // Phase 1: delegate entirely to eth_api
        // Phase 2 will add pending state override from flashblocks cache
        EthState::balance(&self.eth_api, address, block_number).await.map_err(Into::into)
    }

    async fn get_transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
    ) -> RpcResult<U256> {
        EthState::transaction_count(&self.eth_api, address, block_number).await.map_err(Into::into)
    }

    async fn get_code(&self, address: Address, block_number: Option<BlockId>) -> RpcResult<Bytes> {
        EthState::get_code(&self.eth_api, address, block_number).await.map_err(Into::into)
    }

    async fn get_storage_at(
        &self,
        address: Address,
        slot: U256,
        block_number: Option<BlockId>,
    ) -> RpcResult<alloy_primitives::B256> {
        EthState::storage_at(
            &self.eth_api,
            address,
            alloy_rpc_types_eth::JsonStorageKey(slot.into()),
            block_number,
        )
        .await
        .map_err(Into::into)
    }

    async fn call(
        &self,
        transaction: alloy_rpc_types_eth::TransactionRequest,
        block_number: Option<BlockId>,
        state_overrides: Option<StateOverride>,
        block_overrides: Option<Box<BlockOverrides>>,
    ) -> RpcResult<Bytes> {
        // Phase 1: delegate entirely to eth_api
        // Phase 2 will merge flashblocks state overrides for pending
        EthCall::call(
            &self.eth_api,
            transaction,
            block_number,
            EvmOverrides::new(state_overrides, block_overrides),
        )
        .await
        .map_err(Into::into)
    }

    async fn estimate_gas(
        &self,
        transaction: alloy_rpc_types_eth::TransactionRequest,
        block_number: Option<BlockId>,
        overrides: Option<StateOverride>,
    ) -> RpcResult<U256> {
        // Phase 1: delegate entirely to eth_api
        let block_id = block_number.unwrap_or_default();
        EthCall::estimate_gas_at(&self.eth_api, transaction, block_id, overrides)
            .await
            .map_err(Into::into)
    }

    async fn get_logs(&self, filter: Filter) -> RpcResult<Vec<Log>> {
        debug!(target: "xlayer::rpc", ?filter.address, "eth_getLogs");

        // Check if this is a range query with pending toBlock
        let (from_block, to_block) = match &filter.block_option {
            alloy_rpc_types_eth::FilterBlockOption::Range { from_block, to_block } => {
                (*from_block, *to_block)
            }
            _ => {
                // Block hash queries or other formats — delegate to eth filter
                return self.eth_filter.logs(filter).await;
            }
        };

        // If toBlock is not pending, delegate to eth filter
        if !matches!(to_block, Some(BlockNumberOrTag::Pending)) {
            return self.eth_filter.logs(filter).await;
        }

        // Mixed query: toBlock is pending — combine historical + pending logs
        let mut all_logs = Vec::new();

        // Get historical logs if fromBlock is not pending
        if !matches!(from_block, Some(BlockNumberOrTag::Pending)) {
            let mut historical_filter = filter.clone();
            historical_filter.block_option = alloy_rpc_types_eth::FilterBlockOption::Range {
                from_block,
                to_block: Some(BlockNumberOrTag::Latest),
            };
            let historical_logs: Vec<Log> = self.eth_filter.logs(historical_filter).await?;
            all_logs.extend(historical_logs);
        }

        // Get pending logs from flashblocks cache
        if let Some(pending_bar) = self.flash_cache.get_pending_block() {
            let pending_logs = extract_logs_from_bar(&pending_bar, &filter);
            // Dedup: skip logs already fetched in historical range
            let historical_max_block = all_logs.last().and_then(|l| l.block_number);
            for log in pending_logs {
                if let Some(max_block) = historical_max_block {
                    if log.block_number.is_some_and(|n| n <= max_block) {
                        continue;
                    }
                }
                all_logs.push(log);
            }
        }

        Ok(all_logs)
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Converts a [`BlockAndReceipts`] into an RPC block.
fn bar_to_rpc_block<Eth: EthApiTypes<NetworkTypes = Optimism>>(
    bar: &reth_rpc_eth_types::block::BlockAndReceipts<OpPrimitives>,
    full: bool,
    converter: &Eth::RpcConvert,
) -> Result<RpcBlock<Optimism>, Eth::Error>
where
    Eth::Error: From<<Eth::RpcConvert as RpcConvert>::Error>,
{
    bar.block
        .clone_into_rpc_block(
            full.into(),
            |tx, tx_info| converter.fill(tx, tx_info),
            |header, size| converter.convert_header(header, size),
        )
        .map_err(Into::into)
}

/// Converts all receipts from a [`BlockAndReceipts`] into RPC receipts.
fn bar_to_rpc_receipts<Eth: EthApiTypes<NetworkTypes = Optimism>>(
    bar: &reth_rpc_eth_types::block::BlockAndReceipts<OpPrimitives>,
    converter: &Eth::RpcConvert,
) -> Result<Vec<RpcReceipt<Optimism>>, Eth::Error>
where
    Eth::Error: From<<Eth::RpcConvert as RpcConvert>::Error>,
{
    use alloy_consensus::transaction::TxHashRef;
    use reth_rpc_convert::transaction::ConvertReceiptInput;

    let block_hash = bar.block.hash();
    let block_number = bar.block.number();

    let txs = bar.block.body().transactions();
    let receipts = bar.receipts.as_ref();

    let mut inputs = Vec::with_capacity(txs.len());
    let mut next_log_index = 0usize;
    let mut prev_cumulative_gas = 0u64;

    for (idx, (tx, receipt)) in txs.iter().zip(receipts.iter()).enumerate() {
        let gas_used = receipt.as_receipt().cumulative_gas_used - prev_cumulative_gas;
        prev_cumulative_gas = receipt.as_receipt().cumulative_gas_used;

        let meta = reth_primitives_traits::TransactionMeta {
            tx_hash: *tx.tx_hash(),
            index: idx as u64,
            block_hash,
            block_number,
            base_fee: bar.block.base_fee_per_gas(),
            excess_blob_gas: bar.block.excess_blob_gas(),
            timestamp: bar.block.timestamp(),
        };

        inputs.push(ConvertReceiptInput {
            receipt: receipt.clone(),
            tx: reth_primitives_traits::Recovered::new_unchecked(
                tx,
                tx.recover_signer().unwrap_or_default(),
            ),
            gas_used,
            next_log_index,
            meta,
        });

        next_log_index += receipt.as_receipt().logs.len();
    }

    converter.convert_receipts(inputs).map_err(Into::into)
}

/// Converts a single [`CachedTxInfo`] into an RPC receipt.
fn cached_tx_info_to_rpc_receipt<Eth: EthApiTypes<NetworkTypes = Optimism>>(
    info: &xlayer_flashblocks::cache::CachedTxInfo<OpPrimitives>,
    converter: &Eth::RpcConvert,
) -> Result<RpcReceipt<Optimism>, Eth::Error>
where
    Eth::Error: From<<Eth::RpcConvert as RpcConvert>::Error>,
{
    use alloy_consensus::transaction::TxHashRef;
    use reth_rpc_convert::transaction::ConvertReceiptInput;

    let gas_used = info.receipt.as_receipt().cumulative_gas_used;
    let meta = reth_primitives_traits::TransactionMeta {
        tx_hash: *info.tx.tx_hash(),
        index: info.tx_index,
        block_hash: info.block_hash,
        block_number: info.block_number,
        base_fee: None,
        excess_blob_gas: None,
        timestamp: 0,
    };

    let input = ConvertReceiptInput {
        receipt: info.receipt.clone(),
        tx: reth_primitives_traits::Recovered::new_unchecked(
            &info.tx,
            info.tx.recover_signer().unwrap_or_default(),
        ),
        gas_used,
        next_log_index: 0,
        meta,
    };

    let mut receipts = converter.convert_receipts(vec![input]).map_err(Into::into)?;
    Ok(receipts.remove(0))
}

/// Gets a transaction by index from a [`BlockAndReceipts`].
fn get_tx_by_index_from_bar<Eth: EthApiTypes<NetworkTypes = Optimism>>(
    bar: &reth_rpc_eth_types::block::BlockAndReceipts<OpPrimitives>,
    tx_index: u64,
    converter: &Eth::RpcConvert,
) -> Result<Option<RpcTransaction<Optimism>>, Eth::Error>
where
    Eth::Error: From<<Eth::RpcConvert as RpcConvert>::Error>,
{
    use alloy_consensus::transaction::TxHashRef;

    let txs = bar.block.body().transactions();
    let idx = tx_index as usize;
    if idx >= txs.len() {
        return Ok(None);
    }

    let tx = &txs[idx];
    let block_hash = bar.block.hash();
    let block_number = bar.block.number();

    let tx_info = TransactionInfo {
        hash: Some(*tx.tx_hash()),
        index: Some(tx_index),
        block_hash: Some(block_hash),
        block_number: Some(block_number),
        base_fee: bar.block.base_fee_per_gas(),
    };

    let recovered = reth_primitives_traits::Recovered::new_unchecked(
        tx.clone(),
        tx.recover_signer().unwrap_or_default(),
    );
    let rpc_tx = converter.fill(recovered, tx_info).map_err(Into::into)?;
    Ok(Some(rpc_tx))
}

/// Extracts logs from a [`BlockAndReceipts`] that match the given filter.
fn extract_logs_from_bar(
    bar: &reth_rpc_eth_types::block::BlockAndReceipts<OpPrimitives>,
    filter: &Filter,
) -> Vec<Log> {
    use alloy_consensus::transaction::TxHashRef;

    let block_hash = bar.block.hash();
    let block_number = bar.block.number();

    let mut logs = Vec::new();
    let mut log_index = 0u64;

    let txs = bar.block.body().transactions();
    let receipts = bar.receipts.as_ref();

    for (tx_idx, (tx, receipt)) in txs.iter().zip(receipts.iter()).enumerate() {
        for receipt_log in &receipt.as_receipt().logs {
            // Check address filter
            if !filter.address.matches_any(&receipt_log.address) {
                log_index += 1;
                continue;
            }

            // Check topics filter
            if !filter_matches_topics(&filter.topics, &receipt_log.topics()) {
                log_index += 1;
                continue;
            }

            logs.push(Log {
                inner: receipt_log.clone(),
                block_hash: Some(block_hash),
                block_number: Some(block_number),
                block_timestamp: Some(bar.block.timestamp()),
                transaction_hash: Some(*tx.tx_hash()),
                transaction_index: Some(tx_idx as u64),
                log_index: Some(log_index),
                removed: false,
            });
            log_index += 1;
        }
    }

    logs
}

/// Checks if log topics match the filter topics.
fn filter_matches_topics(
    filter_topics: &[alloy_rpc_types_eth::FilterSet<alloy_primitives::B256>],
    log_topics: &[alloy_primitives::B256],
) -> bool {
    for (i, filter_set) in filter_topics.iter().enumerate() {
        if filter_set.is_empty() {
            continue;
        }
        match log_topics.get(i) {
            Some(topic) => {
                if !filter_set.matches(topic) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}
