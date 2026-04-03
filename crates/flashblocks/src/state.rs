use crate::{
    cache::{ExecutionTaskQueue, RawFlashblocksCache},
    debug::debug_compare_flashblocks_bundle_states,
    execution::{FlashblockReceipt, OverlayProviderFactory},
    FlashblockStateCache, XLayerEngineValidator,
};
use futures_util::{FutureExt, Stream, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{sync::broadcast::Sender, time::sleep};

use tracing::*;

use alloy_consensus::BlockHeader;
use alloy_eips::eip2718::Encodable2718;
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;

use reth_chain_state::CanonStateNotificationStream;
use reth_evm::ConfigureEvm;
use reth_optimism_forks::OpHardforks;
use reth_primitives_traits::NodePrimitives;
use reth_provider::{
    BlockReader, HashedPostStateProvider, HeaderProvider, StateProviderFactory, StateReader,
};

use xlayer_builder::broadcast::XLayerFlashblockMessage;

const CONNECTION_BACKOUT_PERIOD: Duration = Duration::from_secs(5);

pub async fn handle_incoming_flashblocks<S, N>(
    mut incoming_rx: S,
    received_tx: Sender<Arc<XLayerFlashblockMessage>>,
    raw_cache: Arc<RawFlashblocksCache<N::SignedTx>>,
    task_queue: ExecutionTaskQueue,
) where
    S: Stream<Item = eyre::Result<XLayerFlashblockMessage>> + Unpin + Send + 'static,
    N: NodePrimitives,
{
    info!(target: "flashblocks", "Flashblocks raw handle started");
    loop {
        match incoming_rx.next().await {
            Some(Ok(message)) => {
                if let Err(err) =
                    process_flashblock_payload::<N>(message, &received_tx, &raw_cache, &task_queue)
                {
                    debug!(
                        target: "flashblocks",
                        %err,
                        "Error receiving flashblock payload"
                    );
                    continue;
                };

                // Batch process all other immediately available flashblocks
                while let Some(result) = incoming_rx.next().now_or_never().flatten() {
                    match result {
                        Ok(message) => {
                            if let Err(err) = process_flashblock_payload::<N>(
                                message,
                                &received_tx,
                                &raw_cache,
                                &task_queue,
                            ) {
                                debug!(
                                    target: "flashblocks",
                                    %err,
                                    "Error receiving flashblock payload"
                                );
                                continue;
                            };
                        }
                        Err(err) => {
                            warn!(target: "flashblocks", %err, "Error receiving flashblock");
                            continue;
                        }
                    }
                }
            }
            Some(Err(err)) => {
                warn!(
                    target: "flashblocks:handle",
                    %err,
                    retry_period = CONNECTION_BACKOUT_PERIOD.as_secs(),
                    "Error receiving flashblock"
                );
                sleep(CONNECTION_BACKOUT_PERIOD).await;
            }
            None => {
                break;
            }
        }
    }
    warn!(target: "flashblocks:handle", "Flashblock payload handle ended");
}

fn process_flashblock_payload<N: NodePrimitives>(
    message: XLayerFlashblockMessage,
    received_tx: &tokio::sync::broadcast::Sender<Arc<XLayerFlashblockMessage>>,
    raw_cache: &RawFlashblocksCache<N::SignedTx>,
    task_queue: &ExecutionTaskQueue,
) -> eyre::Result<()> {
    if received_tx.receiver_count() > 0 {
        let _ = received_tx.send(Arc::new(message.clone()));
    }
    // Insert into raw cache and enqueue to execution tasks
    task_queue.insert(raw_cache.handle_message(message)?);
    Ok(())
}

pub async fn handle_execution_tasks<N, EvmConfig, Provider, ChainSpec, V>(
    validator: XLayerEngineValidator<Provider, EvmConfig, V, N, ChainSpec>,
    raw_cache: Arc<RawFlashblocksCache<N::SignedTx>>,
    flashblocks_state: FlashblockStateCache<N>,
) where
    N: NodePrimitives,
    N::Receipt: FlashblockReceipt,
    N::SignedTx: Encodable2718,
    N::Block: From<alloy_consensus::Block<N::SignedTx>>,
    EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin + Send>
        + Send
        + 'static,
    Provider: StateProviderFactory
        + HeaderProvider<Header = <N as NodePrimitives>::BlockHeader>
        + OverlayProviderFactory
        + BlockReader
        + StateReader
        + HashedPostStateProvider
        + Unpin
        + Clone
        + Send
        + 'static,
    ChainSpec: OpHardforks + Send + Sync + 'static,
    V: Send + 'static,
{
    info!(target: "flashblocks", "Flashblocks execution handle started");
    loop {
        let execute_height = flashblocks_state.task_queue.next().await;
        if execute_height <= flashblocks_state.get_confirm_height() {
            // End signal already processed for this height, skip
            continue;
        }

        // Extract buildable sequence for this height from raw cache
        let Some(args) = raw_cache.try_get_buildable_args(execute_height) else {
            trace!(
                target: "flashblocks",
                execute_height,
                "No buildable args for execution task height, skipping"
            );
            continue;
        };
        debug!(
            target: "flashblocks",
            execute_height,
            last_index = args.last_flashblock_index,
            target_index = args.target_index,
            "Executing flashblocks sequence"
        );

        if let Err(err) = validator.execute_sequence(args).await {
            warn!(
                target: "flashblocks",
                %err,
                execute_height,
                "Validator failed to execute flashblocks sequence"
            );
        }
    }
}

pub async fn handle_canonical_stream<N: NodePrimitives>(
    mut canon_rx: CanonStateNotificationStream<N>,
    flashblocks_state: FlashblockStateCache<N>,
    raw_cache: Arc<RawFlashblocksCache<N::SignedTx>>,
    debug_state_comparison: bool,
) {
    info!(target: "flashblocks", "Canonical state handler started");
    while let Some(notification) = canon_rx.next().await {
        let tip = notification.tip();
        let block_hash = tip.hash();
        let block_number = tip.number();
        let is_reorg = notification.reverted().is_some();

        // Debug mode - state comparison between flashblocks RPC generated execution state vs
        // engine payload validator's execution state (`BundleState` + reverts).
        //
        // Note that engine pre-warming must also be disabled so engine payload validator will
        // compute the new payload independently.
        if debug_state_comparison {
            debug_compare_flashblocks_bundle_states(&flashblocks_state, block_number, block_hash);
        }

        raw_cache.handle_canonical_height(block_number);
        flashblocks_state.handle_canonical_block((block_number, block_hash), is_reorg);
        debug!(
            target: "flashblocks",
            block_number,
            ?block_hash,
            is_reorg,
            "Canonical block processed"
        );
    }
    warn!(target: "flashblocks", "Canonical state stream ended");
}
