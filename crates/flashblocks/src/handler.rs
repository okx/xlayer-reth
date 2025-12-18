use op_rbuilder::args::OpRbuilderArgs;
use op_rbuilder::builders::WebSocketPublisher;
use op_rbuilder::metrics::OpRBuilderMetrics;
use reth_node_api::FullNodeComponents;
use reth_optimism_flashblocks::FlashBlockRx;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{debug, info, warn};

pub struct FlashblocksService<Node>
where
    Node: FullNodeComponents,
{
    node: Node,
    flash_block_rx: FlashBlockRx,
    ws_pub: Arc<WebSocketPublisher>,
    op_args: OpRbuilderArgs,
}

impl<Node> FlashblocksService<Node>
where
    Node: FullNodeComponents,
{
    pub fn new(
        node: Node,
        flash_block_rx: FlashBlockRx,
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

        info!(target: "xlayer::flashblocks", "WebSocket publisher initialized at {}", ws_addr);

        Ok(Self { node, flash_block_rx, ws_pub, op_args })
    }

    pub fn spawn(mut self) {
        info!(target: "xlayer::flashblocks", "Initializing flashblocks service");

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
            target: "xlayer::flashblocks",
            "Flashblocks websocket publisher started"
        );

        loop {
            tokio::select! {
                result = self.flash_block_rx.recv() => {
                    match result {
                        Ok(flash_block) => {
                            info!(
                                target: "xlayer::flashblocks",
                                "Received flashblock: index={}, block_hash={}",
                                flash_block.index,
                                flash_block.diff.block_hash
                            );
                            self.publish_flashblock(&flash_block).await;
                        }
                        Err(e) => {
                            warn!(target: "xlayer::flashblocks", "Flashblock receiver error: {:?}", e);
                            break;
                        }
                    }
                }
            }
        }

        info!(target: "xlayer::flashblocks", "Flashblocks service stopped");
    }

    async fn publish_flashblock(&self, flash_block: &Arc<reth_optimism_flashblocks::FlashBlock>) {
        match serde_json::to_string(&**flash_block) {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(payload) => {
                    if let Err(e) = self.ws_pub.publish(&payload) {
                        warn!(
                            target: "xlayer::flashblocks",
                            "Failed to publish flashblock: {:?}", e
                        );
                    } else {
                        debug!(
                            target: "xlayer::flashblocks",
                            "Published flashblock: index={}, block_hash={}",
                            flash_block.index,
                            flash_block.diff.block_hash
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        target: "xlayer::flashblocks",
                        "Failed to deserialize flashblock: {:?}", e
                    );
                }
            },
            Err(e) => {
                warn!(
                    target: "xlayer::flashblocks",
                    "Failed to serialize flashblock: {:?}", e
                );
            }
        }
    }
}
