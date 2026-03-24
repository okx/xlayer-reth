//! Canonical state stream subscriber utilities.
//!
//! This module provides functions to subscribe to `canonical_state_stream` and
//! process canonical state notifications for inner transaction indexing.

use futures::StreamExt;

use reth_chain_state::{CanonStateNotification, CanonStateSubscriptions};
use reth_evm::ConfigureEvm;
use reth_node_api::{FullNodeComponents, FullNodeTypes};
use reth_primitives_traits::NodePrimitives;
use reth_provider::StateProviderFactory;
use reth_tracing::tracing::{debug, error, info};

use crate::replay_utils::{remove_block, replay_and_index_block};

/// Initializes the inner transaction replay handler that listens to `canonical_state_stream`
/// and indexes internal transactions for each new canonical block.
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
pub fn initialize_innertx_replay<Node>(node: &Node)
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

    task_executor.spawn_critical(
        "xlayer-innertx-replay",
        Box::pin(async move {
            handle_canonical_state_stream(canonical_stream, provider, evm_config).await;
        }),
    );
}

/// Handles the canonical state stream and processes notifications.
async fn handle_canonical_state_stream<P, E, N>(
    mut stream: impl StreamExt<Item = CanonStateNotification<N>> + Unpin,
    provider: P,
    evm_config: E,
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

                for block in new.blocks_iter() {
                    debug!(target: "xlayer::subscriber", "Processing committed block: {:?}", block.hash());

                    let provider_clone = provider.clone();
                    let evm_config_clone = evm_config.clone();

                    if let Err(err) =
                        replay_and_index_block(provider_clone, evm_config_clone, block.clone())
                    {
                        error!(
                            target: "xlayer::subscriber",
                            "Failed to process committed block {:?}: {:?}",
                            block.hash(),
                            err
                        );
                    }
                }
            }
            CanonStateNotification::Reorg { old, new } => {
                debug!(
                    target: "xlayer::subscriber",
                    "Canonical reorg: old range {:?}, new range {:?}",
                    old.range(),
                    new.range()
                );

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
