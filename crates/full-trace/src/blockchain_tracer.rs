//! Blockchain tracer for monitoring canonical state changes

use crate::tracer::{BlockInfo, Tracer};
use alloy_consensus::{transaction::TxHashRef, BlockHeader as _};
use futures::StreamExt;
use reth_primitives_traits::BlockBody as _;
use reth_provider::CanonStateNotification;
use std::sync::Arc;
use tracing::{debug, info};

/// Handle canonical state stream notifications.
///
/// This function monitors the blockchain for canonical state changes (commits and reorgs)
/// and calls the appropriate tracer event handlers for each block and transaction.
///
/// # Parameters
/// - `stream`: The canonical state notification stream from the blockchain provider
/// - `tracer`: The tracer that handles events
///
/// # Note
/// This function is called internally by `Tracer::initialize_blockchain_tracer()`.
/// You typically don't need to call this directly.
pub async fn handle_canonical_state_stream<Args, N>(
    mut stream: impl StreamExt<Item = CanonStateNotification<N>> + Unpin,
    tracer: Arc<Tracer<Args>>,
) where
    Args: Clone + Send + Sync + 'static,
    N: reth_primitives_traits::NodePrimitives + 'static,
    N::SignedTx: alloy_consensus::transaction::TxHashRef,
{
    info!(target: "xlayer::full_trace::blockchain", "Blockchain tracer started, waiting for canonical state notifications");

    while let Some(notification) = stream.next().await {
        match notification {
            CanonStateNotification::Commit { new } => {
                debug!(target: "xlayer::full_trace::blockchain", "Canonical commit: range {:?}", new.range());

                for block in new.blocks_iter() {
                    let sealed_block = block.sealed_block();
                    let block_hash = sealed_block.hash();
                    let block_number = sealed_block.header().number();

                    debug!(target: "xlayer::full_trace::blockchain", "Processing committed block: number={}, hash={:?}", block_number, block_hash);

                    // Create block info
                    let block_info = BlockInfo { block_number, block_hash };

                    // Notify block commit
                    tracer.on_block_commit(&block_info);

                    // Notify each transaction commit
                    for tx in sealed_block.body().transactions() {
                        let tx_hash = *tx.tx_hash();
                        tracer.on_tx_commit(&block_info, tx_hash);
                    }
                }
            }
            CanonStateNotification::Reorg { old, new } => {
                debug!(
                    target: "xlayer::full_trace::blockchain",
                    "Canonical reorg: old range {:?}, new range {:?}",
                    old.range(),
                    new.range()
                );

                // Handle old blocks being removed (if needed in the future)
                for block in old.blocks_iter() {
                    debug!(target: "xlayer::full_trace::blockchain", "Removing reorged block: {:?}", block.hash());
                    // TODO: Add reorg handling logic if needed
                }

                // Handle new blocks being added
                for block in new.blocks_iter() {
                    let sealed_block = block.sealed_block();
                    let block_hash = sealed_block.hash();
                    let block_number = sealed_block.header().number();

                    debug!(target: "xlayer::full_trace::blockchain", "Processing new reorg block: number={}, hash={:?}", block_number, block_hash);

                    // Create block info
                    let block_info = BlockInfo { block_number, block_hash };

                    // Notify block commit
                    tracer.on_block_commit(&block_info);

                    // Notify each transaction commit
                    for tx in sealed_block.body().transactions() {
                        let tx_hash = *tx.tx_hash();
                        tracer.on_tx_commit(&block_info, tx_hash);
                    }
                }
            }
        }
    }

    info!(target: "xlayer::full_trace::blockchain", "Blockchain tracer stopped - canonical state stream closed");
}
