use crate::monitor::XLayerMonitor;

use futures::StreamExt;
use std::sync::Arc;
use tracing::{info, warn};

use alloy_consensus::BlockHeader as _;
use reth_payload_builder_primitives::Events;
use reth_payload_primitives::{BuiltPayload, PayloadBuilderAttributes, PayloadTypes};
use reth_storage_api::BlockNumReader;

/// Payload events listener that monitors block building.
///
/// This listener subscribes to payload builder events and tracks when blocks are built.
/// It monitors BuiltPayload events which are triggered when CL calls getPayload,
/// providing accurate block numbers directly from the built block.
pub struct PayloadListener<Provider> {
    monitor: Arc<XLayerMonitor>,
    provider: Provider,
}

impl<Provider> PayloadListener<Provider> {
    /// Create a new payload listener.
    pub fn new(monitor: Arc<XLayerMonitor>, provider: Provider) -> Self {
        Self { monitor, provider }
    }

    /// Start listening to payload builder events.
    ///
    /// This spawns background tasks that subscribe to payload builder events
    /// and call the monitor's handlers:
    /// - Attributes events trigger on_block_build_start (when CL sends payload attributes)
    /// - BuiltPayload events trigger on_block_send_start (when block is built and ready)
    pub fn listen<T>(
        self,
        payload_builder: &reth_payload_builder::PayloadBuilderHandle<T>,
        task_executor: &dyn reth_tasks::TaskSpawner,
    ) where
        T: PayloadTypes + 'static,
        T::BuiltPayload: BuiltPayload,
        T::PayloadBuilderAttributes: PayloadBuilderAttributes,
        Provider: BlockNumReader + Clone + Send + Sync + 'static,
    {
        info!(target: "xlayer::monitor", "monitor: Starting payload events listener");

        let monitor = self.monitor;
        let payload_builder = payload_builder.clone();
        let provider = self.provider;

        // Listen to both Attributes and BuiltPayload events in a single task
        task_executor.spawn_critical(
            "xlayer-monitor-payload-events",
            Box::pin(async move {
                if let Ok(payload_events) = payload_builder.subscribe().await {
                    let mut event_stream = payload_events.into_stream();
                    info!(target: "xlayer::monitor", "monitor: Payload events listener started");

                    while let Some(event_result) = event_stream.next().await {
                        match event_result {
                            Ok(Events::Attributes(attributes)) => {
                                let payload_id = attributes.payload_id();
                                let parent_hash = attributes.parent();
                                
                                // Get the parent block number from the provider
                                // The new block number will be parent_number + 1
                                match provider.block_number(parent_hash) {
                                    Ok(Some(parent_number)) => {
                                        let block_number = parent_number + 1;
                                        
                                        // Call on_block_build_start with the block number
                                        monitor.on_block_build_start(block_number);

                                        info!(
                                            target: "xlayer::monitor",
                                            payload_id = %payload_id,
                                            block_number,
                                            parent_hash = %parent_hash,
                                            "monitor: Payload attributes received, block build started"
                                        );
                                    }
                                    Ok(None) => {
                                        warn!(
                                            target: "xlayer::monitor",
                                            payload_id = %payload_id,
                                            parent_hash = %parent_hash,
                                            "monitor: Parent block not found for payload attributes"
                                        );
                                    }
                                    Err(e) => {
                                        warn!(
                                            target: "xlayer::monitor",
                                            payload_id = %payload_id,
                                            parent_hash = %parent_hash,
                                            error = %e,
                                            "monitor: Failed to get parent block number"
                                        );
                                    }
                                }
                            }
                            Ok(Events::BuiltPayload(payload)) => {
                                // Get accurate block number directly from the built payload
                                let block = payload.block();
                                let block_number = block.number();
                                let block_hash = block.hash();

                                // Call on_block_send_start with the accurate block number
                                monitor.on_block_send_start(block_number);

                                info!(
                                    target: "xlayer::monitor",
                                    block_number,
                                    block_hash = %block_hash,
                                    "monitor: Payload built successfully"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    target: "xlayer::monitor",
                                    error = %e,
                                    "monitor: Payload event stream error"
                                );
                            }
                        }
                    }

                    info!(target: "xlayer::monitor", "monitor: Payload events listener stopped");
                }
            }),
        );
    }
}
