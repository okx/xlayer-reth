use op_rbuilder::args::OpRbuilderArgs;
use op_rbuilder::builders::WebSocketPublisher;
use op_rbuilder::metrics::OpRBuilderMetrics;
use reth_node_api::FullNodeComponents;
use reth_optimism_flashblocks::FlashBlockRx;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{debug, info, trace, warn};

pub struct FlashblocksService<Node>
where
    Node: FullNodeComponents,
{
    node: Node,
    flashblock_rx: FlashBlockRx,
    ws_pub: Arc<WebSocketPublisher>,
    op_args: OpRbuilderArgs,
}

impl<Node> FlashblocksService<Node>
where
    Node: FullNodeComponents,
{
    pub fn new(
        node: Node,
        flashblock_rx: FlashBlockRx,
        op_args: OpRbuilderArgs,
    ) -> Result<Self, eyre::Report> {
        let ws_addr = SocketAddr::new(
            op_args.flashblocks.flashblocks_addr.parse()?,
            op_args.flashblocks.flashblocks_port,
        );

        let metrics = Arc::new(OpRBuilderMetrics::default());
        let ws_pub = Arc::new(
            WebSocketPublisher::new(ws_addr, metrics)
                .map_err(|e| eyre::eyre!("Failed to create WebSocket publisher: {e}"))?,
        );

        info!(target: "flashblocks", "WebSocket publisher initialized at {}", ws_addr);

        Ok(Self { node, flashblock_rx, ws_pub, op_args })
    }

    pub fn spawn(mut self) {
        debug!(target: "flashblocks", "Initializing flashblocks service");

        let task_executor = self.node.task_executor().clone();
        if self.op_args.rollup_args.flashblocks_url.is_some() {
            task_executor.spawn_critical(
                "xlayer-flashblocks-service",
                Box::pin(async move {
                    self.run().await;
                }),
            );
        }
    }

    async fn run(&mut self) {
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

    async fn publish_flashblock(&self, flashblock: &Arc<reth_optimism_flashblocks::FlashBlock>) {
        match self.ws_pub.publish_op_payload(flashblock) {
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
