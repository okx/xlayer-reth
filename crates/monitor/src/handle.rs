use crate::monitor::XLayerMonitor;

use futures::StreamExt;
use std::sync::Arc;
use tracing::info;

use alloy_consensus::{transaction::TxHashRef, BlockHeader as _};
use alloy_eips::BlockNumHash;
use reth_engine_primitives::ConsensusEngineEvent;
use reth_payload_builder::PayloadBuilderHandle;
use reth_payload_builder_primitives::Events;
use reth_payload_primitives::{BuiltPayload, PayloadTypes};
use reth_primitives_traits::BlockBody as _;
use reth_provider::BlockNumReader;
use reth_tasks::TaskExecutor;

/// Monitor handle logic for handling consensus engine events and payload building events.
///
/// 1. Consensus engine events:
/// - Block received events (engine_newPayload)
/// - Canonical block added events on chain-state updates
///
/// 2. Payload building events:
/// - Attributes event from engine_forkchoiceUpdated
/// - BuiltPayload events from engine_getPayload
pub fn start_monitor_handle<N, T, Provider>(
    task_executor: &TaskExecutor,
    monitor: Arc<XLayerMonitor>,
    provider: Provider,
    payload_builder: PayloadBuilderHandle<T>,
    mut engine_event_stream: reth_tokio_util::EventStream<ConsensusEngineEvent<N>>,
) where
    N: reth_primitives_traits::NodePrimitives + 'static,
    N::SignedTx: TxHashRef,
    T: PayloadTypes + 'static,
    T::BuiltPayload: BuiltPayload,
    Provider: BlockNumReader + Clone + Send + Sync + 'static,
{
    // Check if monitor is enabled, if not, return early
    if !monitor.args.enable {
        return;
    }

    info!(target: "xlayer::monitor", "starting monitor handle");

    let monitor_handle = async move {
        // Initialize built payload stream
        let mut built_payload_stream = if let Ok(payload_events) = payload_builder.subscribe().await
        {
            payload_events.into_stream()
        } else {
            info!(target: "xlayer::monitor", "monitor handle failed to subscribe to payload builder events");
            return;
        };

        loop {
            tokio::select! {
                // Handle consensus engine events
                engine_event = engine_event_stream.next() => {
                    let Some(engine_event) = engine_event else { break };
                    match engine_event {
                        ConsensusEngineEvent::BlockReceived(block_num_hash) => {
                            monitor.on_block_received(block_num_hash);
                        }
                        ConsensusEngineEvent::CanonicalBlockAdded(executed_block, _duration) => {
                            let sealed_block = &executed_block.recovered_block;
                            let num_hash = BlockNumHash::new(sealed_block.header().number(), sealed_block.hash());

                            for tx in sealed_block.body().transactions() {
                                monitor.on_tx_commit(num_hash, *tx.tx_hash());
                            }
                            monitor.on_block_commit(num_hash);
                        }
                        _ => {}
                    }
                }
                // Handle payload building events
                payload_event = built_payload_stream.next() => {
                    let Some(payload_event) = payload_event else { break };
                    match payload_event {
                        Ok(Events::Attributes(_)) => {
                            if let Ok(latest) = provider.last_block_number() {
                                monitor.on_block_build_start(latest + 1);
                            }
                        }
                        Ok(Events::BuiltPayload(payload)) => {
                            let num_hash = BlockNumHash::new(payload.block().number(), payload.block().hash());
                            monitor.on_block_send_start(num_hash);
                        }
                        _ => {}
                    }
                }
            }
        }

        info!(target: "xlayer::monitor", "monitor handle stopped");
    };

    task_executor.spawn_critical_task("xlayer monitor handle", Box::pin(monitor_handle));
}
