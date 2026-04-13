//! Canonical state stream subscriber utilities.
//!
//! This module provides functions to subscribe to `canonical_state_stream` and
//! process canonical state notifications for inner transaction indexing.

use alloy_consensus::BlockHeader;
use futures::StreamExt;

use reth_chain_state::{CanonStateNotification, CanonStateSubscriptions};
use reth_evm::ConfigureEvm;
use reth_node_api::{FullNodeComponents, FullNodeTypes};
use reth_primitives_traits::NodePrimitives;
use reth_provider::StateProviderFactory;
use reth_tracing::tracing::{debug, error, info};

use crate::{
    cache::FlashblocksInnerTxCache,
    replay_utils::{index_block_innertx, remove_block, replay_and_index_block},
};

/// Initializes the inner transaction replay handler that listens to `canonical_state_stream`
/// and indexes internal transactions for each new canonical block.
///
/// When `innertx_cache` is provided, blocks whose innertx traces were already
/// computed by the flashblocks validator are skipped (no redundant replay).
///
/// # Note
///
/// This function should be called from within `extend_rpc_modules` or similar hook.
/// Unlike ExEx, this approach:
/// - Does NOT receive notifications during Pipeline sync
/// - Only processes real-time blocks from Engine API
/// - May miss notifications if the handler is slow (broadcast channel)
///
/// For reliable processing of all blocks including synced ones, use ExEx instead.
pub fn initialize_innertx_replay<Node>(node: &Node, innertx_cache: Option<FlashblocksInnerTxCache>)
where
    Node: FullNodeComponents + Clone + 'static,
    <Node as FullNodeTypes>::Provider: CanonStateSubscriptions,
{
    let provider = node.provider().clone();
    let evm_config = node.evm_config().clone();
    let task_executor = node.task_executor().clone();

    // Subscribe to canonical state updates
    let canonical_stream = provider.canonical_state_stream();

    info!(target: "xlayer::subscriber", "Initializing inner tx replay handler for canonical state stream");

    task_executor.spawn_critical_task(
        "xlayer-innertx-replay",
        Box::pin(async move {
            handle_canonical_state_stream(canonical_stream, provider, evm_config, innertx_cache)
                .await;
        }),
    );
}

/// Handles the canonical state stream and processes notifications.
async fn handle_canonical_state_stream<P, E, N>(
    mut stream: impl StreamExt<Item = CanonStateNotification<N>> + Unpin,
    provider: P,
    evm_config: E,
    innertx_cache: Option<FlashblocksInnerTxCache>,
) where
    P: StateProviderFactory + Clone + Send + Sync + 'static,
    E: ConfigureEvm<Primitives = N> + Clone + Send + Sync + 'static,
    N: NodePrimitives + 'static,
{
    info!(target: "xlayer::subscriber", "Inner tx replay handler started, waiting for canonical state notifications");

    while let Some(notification) = stream.next().await {
        match notification {
            CanonStateNotification::Commit { new } => {
                debug!(target: "xlayer::subscriber", "Canonical commit: range {:?}", new.range());

                for (block, receipts) in new.blocks_and_receipts() {
                    let block_hash = block.hash();
                    let block_number = block.header().number();

                    // If innertx was already computed by the flashblocks
                    // validator, index from cache (skip EVM replay).
                    if let Some(traces) = innertx_cache
                        .as_ref()
                        .and_then(|c| c.get_raw_traces_by_number(block_number))
                    {
                        debug!(
                            target: "xlayer::subscriber",
                            block_number,
                            ?block_hash,
                            "Indexing innertx from flashblocks cache (skipping replay)",
                        );
                        if let Err(err) = index_block_innertx::<N>(block, receipts, traces) {
                            error!(
                                target: "xlayer::subscriber",
                                "Failed to index cached innertx for block {block_hash:?}: {err:?}",
                            );
                        }
                        continue;
                    }

                    debug!(target: "xlayer::subscriber", "Flashblocks cache miss, processing committed block: {block_hash:?}");
                    let provider_clone = provider.clone();
                    let evm_config_clone = evm_config.clone();

                    if let Err(err) =
                        replay_and_index_block(provider_clone, evm_config_clone, block.clone())
                    {
                        error!(
                            target: "xlayer::subscriber",
                            "Failed to process committed block {block_hash:?}: {err:?}",
                        );
                    }
                }

                // Evict all cached innertx at or below the canonical tip.
                if let Some(cache) = &innertx_cache {
                    cache.evict_up_to(new.tip().number());
                }
            }
            CanonStateNotification::Reorg { old, new } => {
                debug!(
                    target: "xlayer::subscriber",
                    "Canonical reorg: old range {:?}, new range {:?}",
                    old.range(),
                    new.range()
                );

                // Flush innertx cache — reorged data is invalid.
                if let Some(cache) = &innertx_cache {
                    cache.clear();
                }

                // Remove old blocks
                for block in old.blocks_iter() {
                    debug!(target: "xlayer::subscriber", "Removing reorged block: {:?}", block.hash());

                    let evm_config_clone = evm_config.clone();

                    if let Err(err) = remove_block(evm_config_clone, block.clone()) {
                        error!(
                            target: "xlayer::subscriber",
                            "Failed to remove reorged block {:?}: {:?}",
                            block.hash(),
                            err
                        );
                    }
                }

                // Add new blocks
                for block in new.blocks_iter() {
                    debug!(target: "xlayer::subscriber", "Processing new reorg block: {:?}", block.hash());

                    let provider_clone = provider.clone();
                    let evm_config_clone = evm_config.clone();

                    if let Err(err) =
                        replay_and_index_block(provider_clone, evm_config_clone, block.clone())
                    {
                        error!(
                            target: "xlayer::subscriber",
                            "Failed to process new reorg block {:?}: {:?}",
                            block.hash(),
                            err
                        );
                    }
                }
            }
        }
    }

    info!(target: "xlayer::subscriber", "Inner tx replay handler stopped - canonical state stream closed");
}
