use crate::{
    cache::RawFlashblocksCache,
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
use op_alloy_rpc_types_engine::{OpFlashblockPayload, OpFlashblockPayloadBase};

use reth_chain_state::CanonStateNotificationStream;
use reth_evm::ConfigureEvm;
use reth_optimism_forks::OpHardforks;
use reth_primitives_traits::NodePrimitives;
use reth_provider::{
    BlockReader, HashedPostStateProvider, HeaderProvider, StateProviderFactory, StateReader,
};

const CONNECTION_BACKOUT_PERIOD: Duration = Duration::from_secs(5);

pub async fn handle_incoming_flashblocks<S, N>(
    mut incoming_rx: S,
    received_tx: Sender<Arc<OpFlashblockPayload>>,
    raw_cache: Arc<RawFlashblocksCache<N::SignedTx>>,
    task_queue: ExecutionTaskQueue,
) where
    S: Stream<Item = eyre::Result<OpFlashblockPayload>> + Unpin + Send + 'static,
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
    flashblock: OpFlashblockPayload,
    received_tx: &tokio::sync::broadcast::Sender<Arc<OpFlashblockPayload>>,
    raw_cache: &RawFlashblocksCache<N::SignedTx>,
    task_queue: &ExecutionTaskQueue,
) -> eyre::Result<()> {
    if received_tx.receiver_count() > 0 {
        let _ = received_tx.send(Arc::new(flashblock.clone()));
    }
    // Insert into raw cache
    let height = flashblock.block_number();
    raw_cache.handle_flashblock(flashblock)?;

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

pub fn handle_execution_tasks<S, N, EvmConfig, Provider, ChainSpec>(
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
                execute_height = execute_height,
                "No buildable args for excution task height, skipping"
            );
            continue;
        };
        debug!(
            target: "flashblocks",
            execute_height = execute_height,
            last_index = args.last_flashblock_index,
            "Executing flashblocks sequence"
        );

        if let Err(err) = validator.execute_sequence(args) {
            warn!(
                target: "flashblocks",
                %err,
                execute_height = execute_height,
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
) {
    info!(target: "flashblocks", "Canonical state handler started");
    while let Some(notification) = canon_rx.next().await {
        let tip = notification.tip();
        let block_hash = tip.hash();
        let block_number = tip.number();
        let is_reorg = notification.reverted().is_some();

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
