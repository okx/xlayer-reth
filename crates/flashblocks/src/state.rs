use crate::{
    cache::RawFlashblocksCache,
    debug::debug_compare_flashblocks_bundle_states,
    execution::validator::FlashblockSequenceValidator,
    execution::{FlashblockReceipt, OverlayProviderFactory},
    service::{ExecutionTaskQueue, ExecutionTaskQueueFlush, EXECUTION_TASK_QUEUE_CAPACITY},
    FlashblockStateCache,
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

use xlayer_builder::broadcast::XLayerFlashblockPayload;

const CONNECTION_BACKOUT_PERIOD: Duration = Duration::from_secs(5);

pub async fn handle_incoming_flashblocks<S, N>(
    mut incoming_rx: S,
    received_tx: Sender<Arc<XLayerFlashblockPayload>>,
    raw_cache: Arc<RawFlashblocksCache<N::SignedTx>>,
    task_queue: ExecutionTaskQueue,
) where
    S: Stream<Item = eyre::Result<XLayerFlashblockPayload>> + Unpin + Send + 'static,
    N: NodePrimitives,
{
    info!(target: "flashblocks", "Flashblocks raw handle started");
    loop {
        match incoming_rx.next().await {
            Some(Ok(payload)) => {
                if let Err(err) =
                    process_flashblock_payload::<N>(payload, &received_tx, &raw_cache, &task_queue)
                {
                    warn!(
                        target: "flashblocks",
                        %err,
                        "Error receiving flashblock payload"
                    );
                    continue;
                };

                // Batch process all other immediately available flashblocks
                while let Some(result) = incoming_rx.next().now_or_never().flatten() {
                    match result {
                        Ok(payload) => {
                            if let Err(err) = process_flashblock_payload::<N>(
                                payload,
                                &received_tx,
                                &raw_cache,
                                &task_queue,
                            ) {
                                warn!(
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
                // Schedule executor
                task_queue.1.notify_one();
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
    payload: XLayerFlashblockPayload,
    received_tx: &tokio::sync::broadcast::Sender<Arc<XLayerFlashblockPayload>>,
    raw_cache: &RawFlashblocksCache<N::SignedTx>,
    task_queue: &ExecutionTaskQueue,
) -> eyre::Result<()> {
    if received_tx.receiver_count() > 0 {
        let _ = received_tx.send(Arc::new(payload.clone()));
    }
    // Insert into raw cache
    let height = payload.inner.block_number();
    raw_cache.handle_flashblock(payload)?;

    // Enqueue to execution tasks
    let mut queue =
        task_queue.0.lock().map_err(|e| eyre::eyre!("Task queue lock poisoned: {e}"))?;
    if !queue.contains(&height) && queue.len() >= EXECUTION_TASK_QUEUE_CAPACITY {
        // Queue is full — evict the lowest block height before inserting.
        let evicted = queue.pop_first();
        warn!(
            target: "flashblocks",
            ?evicted,
            new_height = height,
            "Execution task queue full, evicting lowest height"
        );
    }
    queue.insert(height);
    Ok(())
}

pub fn handle_execution_tasks<N, EvmConfig, Provider, ChainSpec>(
    mut validator: FlashblockSequenceValidator<N, EvmConfig, Provider, ChainSpec>,
    raw_cache: Arc<RawFlashblocksCache<N::SignedTx>>,
    task_queue: ExecutionTaskQueue,
) where
    N: NodePrimitives,
    N::Receipt: FlashblockReceipt,
    N::SignedTx: Encodable2718,
    N::Block: From<alloy_consensus::Block<N::SignedTx>>,
    EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin>
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
{
    info!(target: "flashblocks", "Flashblocks execution handle started");
    let (queue_mutex, condvar) = &*task_queue;
    loop {
        let execute_height = {
            let guard = match queue_mutex.lock() {
                Ok(g) => g,
                Err(err) => {
                    warn!(target: "flashblocks", %err, "Task queue mutex poisoned, retrying");
                    continue;
                }
            };
            let mut queue = match condvar.wait_while(guard, |q| q.is_empty()) {
                Ok(g) => g,
                Err(err) => {
                    warn!(target: "flashblocks", %err, "Task queue condvar wait poisoned, retrying");
                    continue;
                }
            };
            queue.pop_first().unwrap()
        };

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

        if let Err(err) = validator.execute_sequence(args) {
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
    task_queue: ExecutionTaskQueue,
    debug_state_comparison: bool,
) {
    let mut trie_updates = None;
    let mut pending_rx = if debug_state_comparison {
        Some(flashblocks_state.subscribe_pending_sequence())
    } else {
        None
    };

    info!(target: "flashblocks", "Canonical state handler started");
    loop {
        // Use select! to race canonical notifications with pending sequence updates.
        // Pending sequence updates are only processed in debug mode to capture
        // accumulated trie_updates before the block is promoted to confirm.
        let notification = if let Some(ref mut rx) = pending_rx {
            tokio::select! {
                result = canon_rx.next() => {
                    match result {
                        Some(notification) => notification,
                        None => break,
                    }
                },
                Ok(()) = rx.changed() => {
                    if let Some(seq) = rx.borrow_and_update().as_ref()
                        .filter(|s| s.is_target_flashblock())
                    {
                        trie_updates = Some(seq.prefix_execution_meta.accumulated_trie_updates.clone().into_sorted());
                    }
                    continue;
                }
            }
        } else {
            match canon_rx.next().await {
                Some(n) => n,
                None => break,
            }
        };

        let tip = notification.tip();
        let block_hash = tip.hash();
        let block_number = tip.number();
        let is_reorg = notification.reverted().is_some();

        if debug_state_comparison {
            debug_compare_flashblocks_bundle_states(
                &flashblocks_state,
                block_number,
                block_hash,
                trie_updates.take(),
            );
        }

        raw_cache.handle_canonical_height(block_number);
        if flashblocks_state.handle_canonical_block((block_number, block_hash), is_reorg) {
            task_queue.flush();
        }

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
