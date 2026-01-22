use crate::monitor::XLayerMonitor;

use futures::StreamExt;
use std::sync::Arc;
use tracing::info;

use alloy_consensus::BlockHeader as _;
use reth_payload_primitives::{BuiltPayload, PayloadTypes};

/// Payload events listener that monitors block building.
///
/// This listener subscribes to payload builder events and tracks when blocks are built.
/// It monitors BuiltPayload events which are triggered when CL calls getPayload,
/// providing accurate block numbers directly from the built block.
pub struct PayloadListener {
    monitor: Arc<XLayerMonitor>,
}

impl PayloadListener {
    /// Create a new payload listener.
    pub fn new(monitor: Arc<XLayerMonitor>) -> Self {
        Self { monitor }
    }

    /// Start listening to payload builder events.
    ///
    /// This spawns a background task that subscribes to payload builder events
    /// and calls the monitor's block build start handler.
    pub fn listen<T>(
        self,
        payload_builder: &reth_payload_builder::PayloadBuilderHandle<T>,
        task_executor: &dyn reth_tasks::TaskSpawner,
    ) where
        T: PayloadTypes + 'static,
        T::BuiltPayload: BuiltPayload,
    {
        info!(target: "xlayer::monitor", "monitor: Starting payload events listener");

        let monitor = self.monitor;
        let payload_builder = payload_builder.clone();

        task_executor.spawn_critical(
            "xlayer-monitor-payload-events",
            Box::pin(async move {
                if let Ok(payload_events) = payload_builder.subscribe().await {
                    // Use built_payload_stream to get accurate block numbers
                    let mut built_payload_stream = payload_events.into_built_payload_stream();
                    info!(target: "xlayer::monitor", "monitor: Payload events listener started");

                    while let Some(payload) = built_payload_stream.next().await {
                        // Get accurate block number directly from the built payload
                        let block = payload.block();
                        let block_number = block.number();
                        let block_hash = block.hash();

                        // Call on_block_build_start with the accurate block number
                        monitor.on_block_build_start(block_number);

                        info!(
                            target: "xlayer::monitor",
                            block_number,
                            block_hash = %block_hash,
                            "monitor: Payload built successfully"
                        );
                    }

                    info!(target: "xlayer::monitor", "monitor: Payload events listener stopped");
                }
            }),
        );
    }
}
