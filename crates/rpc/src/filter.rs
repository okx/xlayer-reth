use jsonrpsee::core::{async_trait, RpcResult};
use jsonrpsee::proc_macros::rpc;
use std::time::Duration;
use tracing::trace;

use alloy_consensus::BlockHeader;
use alloy_eips::{BlockNumHash, BlockNumberOrTag};
use alloy_rpc_types_eth::{Filter, FilterBlockOption, Log};

use reth_optimism_primitives::{OpBlock, OpPrimitives, OpReceipt};
use reth_rpc::{eth::filter::EthFilterError, EthFilter};
use reth_rpc_eth_api::{
    helpers::{EthBlocks, LoadReceipt},
    EthApiTypes, EthFilterApiServer, FullEthApiTypes, QueryLimits, RpcNodeCore, RpcNodeCoreExt,
};
use reth_rpc_eth_types::{
    block::BlockAndReceipts,
    logs_utils::{
        append_matching_block_logs, matching_block_logs_with_tx_hashes, FilterBlockRangeError,
        ProviderOrBlock,
    },
    EthFilterConfig,
};
use reth_rpc_server_types::result::ToRpcResult;
use reth_storage_api::{BlockIdReader, BlockReader};

use xlayer_flashblocks::FlashblockStateCache;

/// Eth Filter API override for flashblocks RPC integration.
#[cfg_attr(not(test), rpc(server, namespace = "eth"))]
#[cfg_attr(test, rpc(server, client, namespace = "eth"))]
pub trait FlashblocksFilterOverride {
    /// Returns logs matching given filter object, with flashblock state cache
    /// overlay support for confirmed and pending blocks.
    #[method(name = "getLogs")]
    async fn logs(&self, filter: Filter) -> RpcResult<Vec<Log>>;
}

/// Extended filter API with flashblocks cache overlay.
#[derive(Debug)]
pub struct FlashblocksEthFilterExt<Eth: EthApiTypes> {
    eth_filter: EthFilter<Eth>,
    flashblocks_state: FlashblockStateCache<OpPrimitives>,
    query_limits: QueryLimits,
    _stale_filter_ttl: Duration,
}

impl<Eth: EthApiTypes + RpcNodeCore> FlashblocksEthFilterExt<Eth> {
    /// Creates a new [`FlashblocksEthFilterExt`].
    pub fn new(
        eth_filter: EthFilter<Eth>,
        flashblocks_state: FlashblockStateCache<OpPrimitives>,
        config: EthFilterConfig,
    ) -> Self {
        let EthFilterConfig { max_blocks_per_filter, max_logs_per_response, stale_filter_ttl } =
            config;
        Self {
            eth_filter,
            flashblocks_state,
            _stale_filter_ttl: stale_filter_ttl,
            query_limits: QueryLimits { max_blocks_per_filter, max_logs_per_response },
        }
    }
}

#[async_trait]
impl<Eth> FlashblocksFilterOverrideServer for FlashblocksEthFilterExt<Eth>
where
    Eth: FullEthApiTypes
        + RpcNodeCoreExt<Provider: BlockIdReader + BlockReader<Block = OpBlock, Receipt = OpReceipt>>
        + RpcNodeCore
        + LoadReceipt
        + EthBlocks
        + 'static,
{
    /// Returns logs matching given filter object.
    ///
    /// Handler for `eth_getLogs`
    async fn logs(&self, filter: Filter) -> RpcResult<Vec<Log>> {
        trace!(target: "rpc::eth", "Serving eth_getLogs");
        match filter.block_option {
            FilterBlockOption::AtBlockHash(hash) => {
                if let Some(bar) = self.flashblocks_state.get_block_by_hash(&hash) {
                    return Ok(get_matching_logs_from_bar(&bar, &filter));
                }
                self.eth_filter.logs(filter).await
            }
            FilterBlockOption::Range { from_block, to_block } => {
                // Handle special case where from block is pending
                if from_block.is_some_and(|b| b.is_pending()) {
                    let to_block = to_block.unwrap_or(BlockNumberOrTag::Pending);
                    if !(to_block.is_pending() || to_block.is_number()) {
                        // always empty range
                        return Ok(Vec::new());
                    }
                    // Try flashblock pending sequence first
                    if let Some(pending_bar) =
                        self.flashblocks_state.get_rpc_block(BlockNumberOrTag::Pending)
                    {
                        if let BlockNumberOrTag::Number(to_num) = to_block
                            && to_num < pending_bar.block.number()
                        {
                            // this block range is empty based on the user input
                            return Ok(Vec::new());
                        }
                        return Ok(get_matching_logs_from_bar(&pending_bar, &filter));
                    }
                    // No flashblock pending, delegate to canonical filter
                    return self.eth_filter.logs(filter).await;
                }

                let pending_height = self.flashblocks_state.get_pending_height();
                let confirm_height = self.flashblocks_state.get_confirm_height();
                if confirm_height == 0 || pending_height == 0 {
                    // Cache not initialized, delegate to canonical filter
                    return self.eth_filter.logs(filter).await;
                }

                let from = match from_block {
                    Some(tag) => match resolve_fb_bound(tag, confirm_height, pending_height) {
                        Some(n) => Some(n),
                        None => return self.eth_filter.logs(filter).await,
                    },
                    None => None,
                };
                let to = match to_block {
                    Some(tag) => match resolve_fb_bound(tag, confirm_height, pending_height) {
                        Some(n) => Some(n),
                        None => return self.eth_filter.logs(filter).await,
                    },
                    None => None,
                };

                let default_start_block = confirm_height;
                let default_end_block = pending_height;

                // Return error if range exceeds current tip (pending)
                if let Some(t) = to
                    && t > pending_height
                {
                    return Err(EthFilterError::BlockRangeExceedsHead.into());
                }
                if let Some(f) = from
                    && f > pending_height
                {
                    return Err(EthFilterError::BlockRangeExceedsHead.into());
                }

                let (from_block_number, to_block_number) =
                    get_filter_block_range(from, to, default_start_block, default_end_block)
                        .map_err(EthFilterError::from)?;

                // Query filter limit check
                if let Some(max_blocks_per_filter) = self
                    .query_limits
                    .max_blocks_per_filter
                    .filter(|limit| to_block_number - from_block_number > *limit)
                {
                    return Err(EthFilterError::QueryExceedsMaxBlocks(max_blocks_per_filter).into());
                }

                // Get block logs from flashblocks state. Note that we cannot assume that flashblocks
                // state has not advanced since the last retrieval of pending and confirmed heights.
                let mut fb_logs: Vec<Vec<Log>> = Vec::new();
                let mut canonical_end = None;
                for num in (from_block_number..=to_block_number).rev() {
                    if let Some(bar) = self.flashblocks_state.get_block_by_number(num) {
                        let num_hash = BlockNumHash::new(bar.block.number(), bar.block.hash());
                        let receipts = bar.receipts.as_ref();
                        let timestamp = bar.block.timestamp();
                        let mut block_logs = Vec::new();
                        append_matching_block_logs(
                            &mut block_logs,
                            ProviderOrBlock::<Eth::Provider>::Block(bar.block),
                            &filter,
                            num_hash,
                            receipts,
                            false,
                            timestamp,
                        )
                        .to_rpc_result()?;
                        fb_logs.push(block_logs);
                    } else {
                        canonical_end = Some(num);
                        break;
                    }
                }

                // Get remaining block logs, if any, from canonical
                let mut all_logs = if let Some(ce) = canonical_end {
                    self.get_canonical_logs(&filter, from_block_number, ce).await?
                } else {
                    Vec::new()
                };

                // Combine flashblock and canonical logs. Flashblock logs were collected
                // in reverse block order. Extend so the final result is ascending.
                for block_logs in fb_logs.into_iter().rev() {
                    all_logs.extend(block_logs);
                }
                Ok(all_logs)
            }
        }
    }
}

impl<Eth> FlashblocksEthFilterExt<Eth>
where
    Eth: FullEthApiTypes
        + RpcNodeCoreExt<Provider: BlockIdReader + BlockReader<Block = OpBlock, Receipt = OpReceipt>>
        + RpcNodeCore
        + LoadReceipt
        + EthBlocks
        + 'static,
{
    /// Delegates a sub-range log query to the canonical [`EthFilter`].
    ///
    /// Constructs a [`Filter`] with explicit `Number` block bounds so that the
    /// underlying filter does not re-resolve `Latest` / `Pending` against the
    /// canonical head.
    async fn get_canonical_logs(&self, filter: &Filter, from: u64, to: u64) -> RpcResult<Vec<Log>> {
        let sub_filter = Filter {
            block_option: FilterBlockOption::Range {
                from_block: Some(BlockNumberOrTag::Number(from)),
                to_block: Some(BlockNumberOrTag::Number(to)),
            },
            ..filter.clone()
        };
        self.eth_filter.logs(sub_filter).await
    }
}

fn get_filter_block_range(
    from_block: Option<u64>,
    to_block: Option<u64>,
    default_start_block: u64,
    default_end_block: u64,
) -> Result<(u64, u64), FilterBlockRangeError> {
    let from_block_number = from_block.unwrap_or(default_start_block);
    let to_block_number = to_block.unwrap_or(default_end_block);

    // from > to is an invalid range
    if from_block_number > to_block_number {
        return Err(FilterBlockRangeError::InvalidBlockRange);
    }
    // we cannot query blocks that don't exist yet
    if to_block_number > default_end_block {
        return Err(FilterBlockRangeError::BlockRangeExceedsHead);
    }
    Ok((from_block_number, to_block_number))
}

/// Extracts logs from a [`BlockAndReceipts`] that match the given filter.
fn get_matching_logs_from_bar(bar: &BlockAndReceipts<OpPrimitives>, filter: &Filter) -> Vec<Log> {
    let block_num_hash = BlockNumHash::new(bar.block.number(), bar.block.hash());
    let tx_hashes_and_receipts =
        bar.block.body().transactions.iter().map(|tx| tx.tx_hash()).zip(bar.receipts.iter());
    matching_block_logs_with_tx_hashes(
        filter,
        block_num_hash,
        bar.block.timestamp(),
        tx_hashes_and_receipts,
        false,
    )
}

/// Resolves a [`BlockNumberOrTag`] to an absolute block number using the flashblocks
/// cache heights.
fn resolve_fb_bound(
    tag: BlockNumberOrTag,
    confirm_height: u64,
    pending_height: u64,
) -> Option<u64> {
    match tag {
        BlockNumberOrTag::Pending => Some(pending_height),
        BlockNumberOrTag::Latest => Some(confirm_height),
        BlockNumberOrTag::Number(n) => Some(n),
        BlockNumberOrTag::Earliest | BlockNumberOrTag::Safe | BlockNumberOrTag::Finalized => None,
    }
}
