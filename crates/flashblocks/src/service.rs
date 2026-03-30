use crate::{
    cache::{raw::RawFlashblocksCache, FlashblockStateCache},
    execution::{FlashblockReceipt, FlashblockSequenceValidator, OverlayProviderFactory},
    persist::{handle_persistence, handle_relay_flashblocks},
    state::{handle_canonical_stream, handle_execution_tasks, handle_incoming_flashblocks},
    ReceivedFlashblocksRx,
};
use futures_util::Stream;
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    sync::{Arc, Condvar, Mutex, OnceLock},
};
use tokio::sync::broadcast::Sender;
use tracing::*;

use alloy_eips::eip2718::Encodable2718;
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;

use reth_chain_state::CanonStateNotificationStream;
use reth_engine_primitives::TreeConfig;
use reth_evm::ConfigureEvm;
use reth_node_core::dirs::{ChainPath, DataDirPath};
use reth_optimism_forks::OpHardforks;
use reth_primitives_traits::NodePrimitives;
use reth_provider::{
    BlockReader, HashedPostStateProvider, HeaderProvider, StateProviderFactory, StateReader,
};
use reth_tasks::TaskExecutor;

use xlayer_builder::{
    args::FlashblocksArgs,
    broadcast::{WebSocketPublisher, XLayerFlashblockPayload},
    flashblocks::PayloadEventsSender,
    metrics::{tokio::FlashblocksTaskMetrics, BuilderMetrics},
};

pub const EXECUTION_TASK_QUEUE_CAPACITY: usize = 5;

pub type ExecutionTaskQueue = Arc<(Mutex<BTreeSet<u64>>, Condvar)>;

/// Extension trait for [`ExecutionTaskQueue`] providing a flush operation.
pub trait ExecutionTaskQueueFlush {
    /// Clears all pending execution tasks from the queue.
    ///
    /// Called when a flush is detected on the flashblocks state layer (reorg or stale
    /// pending) to drain any queued block heights that were built against now-invalidated
    /// state. The execution worker will re-enter its wait loop and pick up fresh tasks
    /// from incoming flashblocks after this call.
    fn flush(&self);
}

impl ExecutionTaskQueueFlush for ExecutionTaskQueue {
    fn flush(&self) {
        match self.0.lock() {
            Ok(mut queue) => {
                let flushed = queue.len();
                queue.clear();
                if flushed > 0 {
                    warn!(
                        target: "flashblocks",
                        flushed,
                        "Execution task queue flushed on state reset"
                    );
                }
            }
            Err(err) => {
                warn!(
                    target: "flashblocks",
                    %err,
                    "Failed to flush execution task queue: mutex poisoned"
                );
            }
        }
    }
}

/// Context for flashblocks RPC state handles.
pub struct FlashblocksRpcCtx<N: NodePrimitives, EvmConfig, Provider, ChainSpec> {
    /// Canonical chainstate provider.
    pub provider: Provider,
    /// Canonical state notification stream.
    pub canon_state_rx: CanonStateNotificationStream<N>,
    /// Evm config for the sequence validator.
    pub evm_config: EvmConfig,
    /// Chain specs for the sequence validator.
    pub chain_spec: Arc<ChainSpec>,
    /// Node engine tree configuration for the sequence validator.
    pub tree_config: TreeConfig,
    /// Flashblocks RPC debug mode to enable state comparison.
    pub debug_state_comparison: bool,
}

/// Context for handling flashblocks persistence and relaying.
pub struct FlashblocksPersistCtx {
    /// Data directory for flashblocks persistence.
    pub datadir: ChainPath<DataDirPath>,
    /// Whether to relay flashblocks to the subscribers.
    pub relay_flashblocks: bool,
}

pub struct FlashblocksRpcService<N, EvmConfig, Provider, ChainSpec>
where
    N: NodePrimitives,
    EvmConfig: ConfigureEvm,
    ChainSpec: OpHardforks,
{
    /// Flashblock configurations.
    args: FlashblocksArgs,
    /// Flashblocks state cache (shared with RPC handlers).
    flashblocks_state: FlashblockStateCache<N>,
    /// Flashblocks RPC context.
    rpc_ctx: FlashblocksRpcCtx<N, EvmConfig, Provider, ChainSpec>,
    /// Flashblocks persist context.
    persist_ctx: FlashblocksPersistCtx,
    /// Task executor.
    task_executor: TaskExecutor,
    /// Broadcast channel to forward received flashblocks from the subscription.
    received_flashblocks_tx: Sender<Arc<XLayerFlashblockPayload>>,
}

impl<N, EvmConfig, Provider, ChainSpec> FlashblocksRpcService<N, EvmConfig, Provider, ChainSpec>
where
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
    pub fn new(
        args: FlashblocksArgs,
        flashblocks_state: FlashblockStateCache<N>,
        task_executor: TaskExecutor,
        rpc_ctx: FlashblocksRpcCtx<N, EvmConfig, Provider, ChainSpec>,
        persist_ctx: FlashblocksPersistCtx,
    ) -> eyre::Result<Self> {
        let (received_flashblocks_tx, _) = tokio::sync::broadcast::channel(128);
        Ok(Self {
            args,
            flashblocks_state,
            rpc_ctx,
            persist_ctx,
            task_executor,
            received_flashblocks_tx,
        })
    }

    /// Returns a new subscription to received flashblocks.
    pub fn subscribe_received_flashblocks(&self) -> ReceivedFlashblocksRx {
        self.received_flashblocks_tx.subscribe()
    }

    pub fn spawn_persistence(&self) -> eyre::Result<()> {
        // Spawn persistence handle
        debug!(target: "flashblocks", "Initializing flashblocks persistence");
        let datadir = self.persist_ctx.datadir.clone();
        let rx = self.subscribe_received_flashblocks();
        self.task_executor.spawn_critical_task(
            "xlayer-flashblocks-persistence",
            Box::pin(async move {
                handle_persistence(rx, datadir).await;
            }),
        );
        // Spawn relayer handle
        if self.persist_ctx.relay_flashblocks {
            let ws_addr =
                SocketAddr::new(self.args.flashblocks_addr.parse()?, self.args.flashblocks_port);
            let metrics = Arc::new(BuilderMetrics::default());
            let task_metrics = Arc::new(FlashblocksTaskMetrics::new());
            let ws_pub = Arc::new(
                WebSocketPublisher::new(
                    ws_addr,
                    metrics,
                    &task_metrics.websocket_publisher,
                    self.args.ws_subscriber_limit,
                )
                .map_err(|e| eyre::eyre!("Failed to create WebSocket publisher: {e}"))?,
            );
            info!(target: "flashblocks", "WebSocket publisher initialized at {ws_addr}");

            let rx = self.subscribe_received_flashblocks();
            self.task_executor.spawn_critical_task(
                "xlayer-flashblocks-publish",
                Box::pin(async move {
                    handle_relay_flashblocks(rx, ws_pub).await;
                }),
            );
        }
        Ok(())
    }

    pub fn spawn_rpc<S>(self, incoming_rx: S)
    where
        S: Stream<Item = eyre::Result<XLayerFlashblockPayload>> + Unpin + Send + 'static,
    {
        debug!(target: "flashblocks", "Initializing flashblocks rpc");
        let raw_cache = Arc::new(RawFlashblocksCache::new());
        let validator = FlashblockSequenceValidator::new(
            self.rpc_ctx.evm_config,
            self.rpc_ctx.provider,
            self.rpc_ctx.chain_spec,
            self.flashblocks_state.clone(),
            self.task_executor.clone(),
            self.rpc_ctx.tree_config,
        );
        let task_queue = Arc::new((Mutex::new(BTreeSet::new()), Condvar::new()));

        // Spawn incoming raw flashblocks handle.
        let received_tx = self.received_flashblocks_tx.clone();
        self.task_executor.spawn_critical_task(
            "xlayer-flashblocks-payload",
            Box::pin(handle_incoming_flashblocks::<S, N>(
                incoming_rx,
                received_tx,
                raw_cache.clone(),
                task_queue.clone(),
            )),
        );

        // Spawn the flashblocks sequence execution task on a dedicated OS thread.
        let cache = raw_cache.clone();
        let queue = task_queue.clone();
        reth_tasks::spawn_os_thread("xlayer-flashblocks-execution", move || {
            handle_execution_tasks::<N, EvmConfig, Provider, ChainSpec>(validator, cache, queue);
        });

        // Spawn the canonical stream handle.
        self.task_executor.spawn_critical_task(
            "xlayer-flashblocks-canonical",
            Box::pin(handle_canonical_stream::<N>(
                self.rpc_ctx.canon_state_rx,
                self.flashblocks_state,
                raw_cache,
                task_queue,
                self.rpc_ctx.debug_state_comparison,
            )),
        );
    }

    pub fn spawn_prewarm(&self, events_sender: Arc<OnceLock<PayloadEventsSender>>)
    where
        N: NodePrimitives<
            Block = <reth_optimism_primitives::OpPrimitives as NodePrimitives>::Block,
            Receipt = <reth_optimism_primitives::OpPrimitives as NodePrimitives>::Receipt,
        >,
    {
        let mut pending_rx = self.flashblocks_state.subscribe_pending_sequence();
        if let Some(payload_events_sender) = events_sender.get().cloned() {
            self.task_executor.spawn_critical_task(
                "xlayer-flashblocks-prewarm",
                Box::pin(async move {
                    use either::Either;
                    use reth_optimism_payload_builder::OpBuiltPayload;
                    use reth_optimism_primitives::OpPrimitives;
                    use reth_payload_builder_primitives::Events;

                    while pending_rx.changed().await.is_ok() {
                        let Some(pending_sequence) = pending_rx.borrow_and_update().clone() else {
                            continue;
                        };
                        let executed = &pending_sequence.executed_block;
                        let block = executed.recovered_block.clone_sealed_block();
                        let trie_data = executed.trie_data();
                        let built =
                            reth_payload_primitives::BuiltPayloadExecutedBlock::<OpPrimitives> {
                                recovered_block: executed.recovered_block.clone(),
                                execution_output: executed.execution_output.clone(),
                                hashed_state: Either::Right(trie_data.hashed_state),
                                trie_updates: Either::Right(trie_data.trie_updates),
                            };
                        // Use default zero id — to avoid accumulating stale entries in the engine state tree.
                        let payload = OpBuiltPayload::<OpPrimitives>::new(
                            reth_payload_builder::PayloadId::default(),
                            Arc::new(block),
                            alloy_primitives::U256::ZERO,
                            Some(built),
                        );
                        let _ = payload_events_sender.send(Events::BuiltPayload(payload));
                    }
                }),
            );
        }
    }
}
