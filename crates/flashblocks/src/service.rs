use crate::{
    handle::{handle_persistence, handle_relay_flashblocks},
    ReceivedFlashblocksRx,
};
use futures_util::{FutureExt, Stream, StreamExt};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::time::sleep;
use tracing::*;

use op_alloy_rpc_types_engine::OpFlashblockPayload;
use reth_node_core::dirs::{ChainPath, DataDirPath};
use reth_tasks::TaskExecutor;

use xlayer_builder::{
    args::FlashblocksArgs,
    flashblocks::WebSocketPublisher,
    metrics::{tokio::FlashblocksTaskMetrics, BuilderMetrics},
};

const CONNECTION_BACKOUT_PERIOD: Duration = Duration::from_secs(5);

pub struct FlashblocksRpcService<S> {
    /// Incoming flashblock stream.
    incoming_flashblock_rx: S,
    /// Broadcast channel to forward received flashblocks from the subscription.
    received_flashblocks_tx: tokio::sync::broadcast::Sender<Arc<OpFlashblockPayload>>,
    /// Task executor.
    task_executor: TaskExecutor,
    /// Flashblocks websocket publisher for relaying flashblocks to subscribers.
    ws_pub: Arc<WebSocketPublisher>,
    /// Whether to relay flashblocks to the subscribers.
    relay_flashblocks: bool,
    /// Data directory for flashblocks persistence.
    datadir: ChainPath<DataDirPath>,
}

impl<S> FlashblocksRpcService<S>
where
    S: Stream<Item = eyre::Result<OpFlashblockPayload>> + Unpin + 'static,
{
    pub fn new(
        task_executor: TaskExecutor,
        incoming_flashblock_rx: S,
        args: FlashblocksArgs,
        relay_flashblocks: bool,
        datadir: ChainPath<DataDirPath>,
    ) -> Result<Self, eyre::Report> {
        let (received_flashblocks_tx, _) = tokio::sync::broadcast::channel(128);

        // Initialize ws publisher for relaying flashblocks
        let ws_addr = SocketAddr::new(args.flashblocks_addr.parse()?, args.flashblocks_port);
        let metrics = Arc::new(BuilderMetrics::default());
        let task_metrics = Arc::new(FlashblocksTaskMetrics::new());
        let ws_pub = Arc::new(
            WebSocketPublisher::new(
                ws_addr,
                metrics,
                &task_metrics.websocket_publisher,
                args.ws_subscriber_limit,
            )
            .map_err(|e| eyre::eyre!("Failed to create WebSocket publisher: {e}"))?,
        );
        info!(target: "flashblocks", "WebSocket publisher initialized at {}", ws_addr);

        Ok(Self {
            incoming_flashblock_rx,
            received_flashblocks_tx,
            task_executor,
            ws_pub,
            relay_flashblocks,
            datadir,
        })
    }

    /// Returns a new subscription to received flashblocks.
    pub fn subscribe_received_flashblocks(&self) -> ReceivedFlashblocksRx {
        self.received_flashblocks_tx.subscribe()
    }

    pub fn spawn(&self) {
        debug!(target: "flashblocks", "Initializing flashblocks service");
        // Spawn persistence handle
        let datadir = self.datadir.clone();
        let rx = self.subscribe_received_flashblocks();
        self.task_executor.spawn_critical(
            "xlayer-flashblocks-persistence",
            Box::pin(async move {
                handle_persistence(rx, datadir).await;
            }),
        );

        // Spawn relayer handle
        if self.relay_flashblocks {
            let rx = self.subscribe_received_flashblocks();
            let ws_pub = self.ws_pub.clone();
            self.task_executor.spawn_critical(
                "xlayer-flashblocks-publish",
                Box::pin(async move {
                    handle_relay_flashblocks(rx, ws_pub).await;
                }),
            );
        }
    }

    /// Contains the main logic for processing raw incoming flashblocks, and updating the
    /// flashblocks state cache layer. The logic pipeline is as follows:
    /// 1. Notifies subscribers
    /// 2. Inserts into the raw flashblocks cache
    pub async fn handle_flashblocks(&mut self) {
        loop {
            tokio::select! {
                // Event 1: New flashblock arrives (batch process all ready flashblocks)
                result = self.incoming_flashblock_rx.next() => {
                    match result {
                        Some(Ok(flashblock)) => {
                            // Process first flashblock
                            self.process_flashblock(flashblock);

                            // Batch process all other immediately available flashblocks
                            while let Some(result) = self.incoming_flashblock_rx.next().now_or_never().flatten() {
                                match result {
                                    Ok(fb) => self.process_flashblock(fb),
                                    Err(err) => warn!(target: "flashblocks", %err, "Error receiving flashblock"),
                                }
                            }
                        }
                        Some(Err(err)) => {
                            warn!(
                                target: "flashblocks",
                                %err,
                                retry_period = CONNECTION_BACKOUT_PERIOD.as_secs(),
                                "Error receiving flashblock"
                            );
                            sleep(CONNECTION_BACKOUT_PERIOD).await;
                        }
                        None => {
                            warn!(target: "flashblocks", "Flashblock stream ended");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Processes a single flashblock: notifies subscribers, and inserts into
    /// the raw flashblocks cache.
    fn process_flashblock(&mut self, flashblock: OpFlashblockPayload) {
        self.notify_received_flashblock(&flashblock);
        // TODO: Insert into the raw flashblocks cache
    }

    /// Notifies all subscribers about the received flashblock.
    fn notify_received_flashblock(&self, flashblock: &OpFlashblockPayload) {
        if self.received_flashblocks_tx.receiver_count() > 0 {
            let _ = self.received_flashblocks_tx.send(Arc::new(flashblock.clone()));
        }
    }
}
