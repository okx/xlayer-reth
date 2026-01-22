use crate::monitor::{BlockInfo, XLayerMonitor};

use futures::StreamExt;
use std::sync::Arc;
use tracing::info;

use alloy_consensus::{transaction::TxHashRef, BlockHeader as _};
use reth_engine_primitives::ConsensusEngineEvent;
use reth_primitives_traits::BlockBody as _;

/// Consensus engine events listener that monitors block received and canonical chain events.
///
/// This listener subscribes to consensus engine events and tracks:
/// - Block received events (when newPayload is called)
/// - Canonical block added events (when blocks are committed to the canonical chain)
pub struct ConsensusListener {
    monitor: Arc<XLayerMonitor>,
}

impl ConsensusListener {
    /// Create a new consensus listener.
    pub fn new(monitor: Arc<XLayerMonitor>) -> Self {
        Self { monitor }
    }

    /// Start listening to consensus engine events.
    ///
    /// This spawns a background task that subscribes to consensus engine events
    /// and calls the appropriate monitor handlers:
    /// - on_block_received when BlockReceived events occur
    /// - on_block_commit and on_tx_commit when CanonicalBlockAdded events occur
    pub fn listen<N>(
        self,
        mut event_stream: reth_tokio_util::EventStream<ConsensusEngineEvent<N>>,
        task_executor: &dyn reth_tasks::TaskSpawner,
    ) where
        N: reth_primitives_traits::NodePrimitives + 'static,
        N::SignedTx: TxHashRef,
    {
        info!(target: "xlayer::monitor", "monitor: Starting consensus engine events listener");

        let monitor = self.monitor;

        task_executor.spawn_critical(
            "xlayer-monitor-consensus-events",
            Box::pin(async move {
                info!(target: "xlayer::monitor", "monitor: Consensus engine events listener started");

                while let Some(event) = event_stream.next().await {
                    match event {
                        ConsensusEngineEvent::BlockReceived(block_num_hash) => {
                            monitor.on_block_received(block_num_hash);
                        }
                        ConsensusEngineEvent::CanonicalBlockAdded(executed_block, _duration) => {
                            // Handle canonical block commit
                            let sealed_block = &executed_block.recovered_block;
                            let block_hash = sealed_block.hash();
                            let block_number = sealed_block.header().number();
                            let block_info = BlockInfo { block_number, block_hash };

                            // Notify each transaction commit
                            for tx in sealed_block.body().transactions() {
                                monitor.on_tx_commit(&block_info, *tx.tx_hash());
                            }

                            // Notify block commit
                            monitor.on_block_commit(&block_info);
                        }
                        _ => {
                            // Ignore other events
                        }
                    }
                }

                info!(target: "xlayer::monitor", "monitor: Consensus engine events listener stopped");
            }),
        );
    }
}
