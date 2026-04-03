use crate::{
    cache::{raw::RawFlashblocksCache, FlashblockStateCache},
    execution::{FlashblockReceipt, OverlayProviderFactory},
    persist::{handle_persistence, handle_relay_flashblocks},
    state::{handle_canonical_stream, handle_execution_tasks, handle_incoming_flashblocks},
    ReceivedFlashblocksRx, XLayerEngineValidator,
};
use futures_util::Stream;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::broadcast::Sender;
use tracing::*;

use alloy_eips::eip2718::Encodable2718;
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;

use reth_chain_state::CanonStateNotificationStream;
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
    broadcast::{WebSocketPublisher, XLayerFlashblockMessage},
    metrics::{tokio::FlashblocksTaskMetrics, BuilderMetrics},
};

/// Context for flashblocks RPC state handles.
pub struct FlashblocksRpcCtx<N: NodePrimitives> {
    /// Canonical state notification stream.
    pub canon_state_rx: CanonStateNotificationStream<N>,
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

pub struct FlashblocksRpcService<N>
where
    N: NodePrimitives,
{
    /// Flashblock configurations.
    args: FlashblocksArgs,
    /// Flashblocks state cache (shared with RPC handlers).
    flashblocks_state: FlashblockStateCache<N>,
    /// Flashblocks RPC context.
    rpc_ctx: FlashblocksRpcCtx<N>,
    /// Flashblocks persist context.
    persist_ctx: FlashblocksPersistCtx,
    /// Task executor.
    task_executor: TaskExecutor,
    /// Broadcast channel to forward received flashblocks from the subscription.
    received_flashblocks_tx: Sender<Arc<XLayerFlashblockMessage>>,
}

impl<N> FlashblocksRpcService<N>
where
    N: NodePrimitives,
    N::Receipt: FlashblockReceipt,
    N::SignedTx: Encodable2718,
    N::Block: From<alloy_consensus::Block<N::SignedTx>>,
{
    pub fn new(
        args: FlashblocksArgs,
        flashblocks_state: FlashblockStateCache<N>,
        task_executor: TaskExecutor,
        rpc_ctx: FlashblocksRpcCtx<N>,
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

    pub fn spawn_rpc<S, EvmConfig, Provider, ChainSpec, V>(
        self,
        engine_validator: XLayerEngineValidator<Provider, EvmConfig, V, N, ChainSpec>,
        incoming_rx: S,
    ) where
        S: Stream<Item = eyre::Result<XLayerFlashblockMessage>> + Unpin + Send + 'static,
        EvmConfig: ConfigureEvm<
                Primitives = N,
                NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin + Send,
            > + Send
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
        V: Send + Sync + 'static,
    {
        debug!(target: "flashblocks", "Initializing flashblocks rpc");
        let raw_cache = Arc::new(RawFlashblocksCache::new());

        // Spawn incoming raw flashblocks handle.
        let received_tx = self.received_flashblocks_tx.clone();
        self.task_executor.spawn_critical_task(
            "xlayer-flashblocks-payload",
            Box::pin(handle_incoming_flashblocks::<S, N>(
                incoming_rx,
                received_tx,
                raw_cache.clone(),
                self.flashblocks_state.task_queue.clone(),
            )),
        );

        let cache = raw_cache.clone();
        self.task_executor.spawn_critical_blocking_task(
            "xlayer-flashblocks-execution",
            Box::pin(handle_execution_tasks(
                engine_validator,
                cache,
                self.flashblocks_state.clone(),
            )),
        );

        // Spawn the canonical stream handle.
        self.task_executor.spawn_critical_task(
            "xlayer-flashblocks-canonical",
            Box::pin(handle_canonical_stream::<N>(
                self.rpc_ctx.canon_state_rx,
                self.flashblocks_state,
                raw_cache,
                self.rpc_ctx.debug_state_comparison,
            )),
        );
    }
}
