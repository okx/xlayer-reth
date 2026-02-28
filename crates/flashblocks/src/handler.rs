use std::{net::SocketAddr, sync::Arc};
use tracing::{debug, info, trace, warn};

use reth_node_api::FullNodeComponents;
use reth_node_core::dirs::{ChainPath, DataDirPath};
use reth_optimism_flashblocks::{FlashBlock, FlashBlockRx};

use xlayer_builder::{
    args::OpRbuilderArgs,
    metrics::{tokio::FlashblocksTaskMetrics, BuilderMetrics},
    payload::{FlashblockPayloadsCache, WebSocketPublisher},
};

pub struct FlashblocksService<Node>
where
    Node: FullNodeComponents,
{
    node: Node,
    flashblock_rx: FlashBlockRx,
    ws_pub: Arc<WebSocketPublisher>,
    op_args: OpRbuilderArgs,
    datadir: ChainPath<DataDirPath>,
}

impl<Node> FlashblocksService<Node>
where
    Node: FullNodeComponents,
{
    pub fn new(
        node: Node,
        flashblock_rx: FlashBlockRx,
        op_args: OpRbuilderArgs,
        datadir: ChainPath<DataDirPath>,
    ) -> Result<Self, eyre::Report> {
        let ws_addr = SocketAddr::new(
            op_args.flashblocks.flashblocks_addr.parse()?,
            op_args.flashblocks.flashblocks_port,
        );

        let metrics = Arc::new(BuilderMetrics::default());
        let task_metrics = Arc::new(FlashblocksTaskMetrics::new());
        let ws_pub = Arc::new(
            WebSocketPublisher::new(
                ws_addr,
                metrics,
                &task_metrics.websocket_publisher,
                op_args.flashblocks.ws_subscriber_limit,
            )
            .map_err(|e| eyre::eyre!("Failed to create WebSocket publisher: {e}"))?,
        );

        info!(target: "flashblocks", "WebSocket publisher initialized at {}", ws_addr);

        Ok(Self { node, flashblock_rx, ws_pub, op_args, datadir })
    }

    pub fn spawn(mut self) {
        debug!(target: "flashblocks", "Initializing flashblocks service");

        let task_executor = self.node.task_executor().clone();
        if self.op_args.rollup_args.flashblocks_url.is_some() {
            let datadir = self.datadir.clone();
            let flashblock_rx = self.flashblock_rx.resubscribe();
            task_executor.spawn_critical(
                "xlayer-flashblocks-persistence",
                Box::pin(async move {
                    handle_persistence(flashblock_rx, datadir).await;
                }),
            );

            task_executor.spawn_critical(
                "xlayer-flashblocks-publish",
                Box::pin(async move {
                    self.publish().await;
                }),
            );
        }
    }

    async fn publish(&mut self) {
        info!(
            target: "flashblocks",
            "Flashblocks websocket publisher started"
        );

        loop {
            match self.flashblock_rx.recv().await {
                Ok(flashblock) => {
                    trace!(
                        target: "flashblocks",
                        "Received flashblock: index={}, block_hash={}",
                        flashblock.index,
                        flashblock.diff.block_hash
                    );
                    self.publish_flashblock(&flashblock).await;
                }
                Err(e) => {
                    warn!(target: "flashblocks", "Flashblock receiver error: {:?}", e);
                    break;
                }
            }
        }

        info!(target: "flashblocks", "Flashblocks service stopped");
    }

    /// Relays the incoming flashblock to the flashblock websocket subscribers.
    async fn publish_flashblock(&self, flashblock: &Arc<FlashBlock>) {
        match self.ws_pub.publish(flashblock) {
            Ok(_) => {
                trace!(
                    target: "flashblocks",
                    "Published flashblock: index={}, block_hash={}",
                    flashblock.index,
                    flashblock.diff.block_hash
                );
            }
            Err(e) => {
                warn!(
                    target: "flashblocks",
                    "Failed to publish flashblock: {:?}", e
                );
            }
        }
    }
}

/// Handles the persistence of the pending flashblocks sequence to disk.
async fn handle_persistence(mut rx: FlashBlockRx, datadir: ChainPath<DataDirPath>) {
    let cache = FlashblockPayloadsCache::new(Some(&datadir));

    loop {
        match rx.recv().await {
            Ok(flashblock) => {
                if let Err(e) = cache.add_flashblock_payload(FlashBlock::clone(&flashblock)) {
                    warn!(target: "flashblocks", "Failed to cache flashblock payload: {e}");
                    continue;
                }
                if let Err(e) = cache.persist().await {
                    warn!(target: "flashblocks", "Failed to persist pending sequence: {e}");
                }
            }
            Err(e) => {
                warn!(target: "flashblocks", "Persistence handle receiver error: {e:?}");
                break;
            }
        }
    }

    info!(target: "flashblocks", "Flashblocks persistence handle stopped");
}
