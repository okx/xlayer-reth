//! Default builder service with P2P reorg protection.
//!
//! [`DefaultBuilderServiceBuilder`] implements `PayloadServiceBuilder` and wires together:
//! - The P2P node (reuses [`broadcast::NodeBuilder`])
//! - The [`FlashblockPayloadsCache`] (shared between handler and builder)
//! - The [`DefaultPayloadBuilder`] (wraps `OpPayloadBuilder` + cache)
//! - The [`DefaultPayloadHandler`] (routes P2P messages to cache)

use crate::{
    broadcast::{
        types::{AGENT_VERSION, FLASHBLOCKS_STREAM_PROTOCOL},
        wspub::WebSocketPublisher,
    },
    default::{handler::DefaultPayloadHandler, DefaultPayloadBuilder},
    flashblocks::{utils::cache::FlashblockPayloadsCache, BuilderConfig},
    metrics::{tokio::FlashblocksTaskMetrics, BuilderMetrics},
    traits::{NodeBounds, PoolBounds},
};
use eyre::WrapErr as _;
use std::sync::Arc;

use reth_basic_payload_builder::{BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig};
use reth_node_api::NodeTypes;
use reth_node_builder::{components::PayloadServiceBuilder, BuilderContext};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig, OpGasLimitConfig};
use reth_payload_builder::{PayloadBuilderHandle, PayloadBuilderService};
use reth_provider::CanonStateSubscriptions;

pub struct DefaultBuilderServiceBuilder {
    pub compute_pending_block: bool,
    pub config: BuilderConfig,
    pub da_config: OpDAConfig,
    pub gas_limit_config: OpGasLimitConfig,
}

impl<Node, Pool> PayloadServiceBuilder<Node, Pool, OpEvmConfig> for DefaultBuilderServiceBuilder
where
    Node: NodeBounds,
    Pool: PoolBounds,
{
    async fn spawn_payload_builder_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        _: OpEvmConfig,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>> {
        let cancel = tokio_util::sync::CancellationToken::new();

        let metrics = Arc::new(BuilderMetrics::default());
        let task_metrics = Arc::new(FlashblocksTaskMetrics::new());
        let ws_pub: Arc<WebSocketPublisher> = WebSocketPublisher::new(
            self.config.flashblocks.ws_addr,
            metrics.clone(),
            &task_metrics.websocket_publisher,
            self.config.flashblocks.ws_subscriber_limit,
        )
        .wrap_err("failed to create ws publisher for default builder")?
        .into();

        let mut broadcast_builder =
            crate::broadcast::NodeBuilder::new(ws_pub.clone(), metrics.clone());

        if let Some(ref private_key_file) = self.config.flashblocks.p2p_private_key_file
            && !private_key_file.is_empty()
        {
            let private_key_hex = std::fs::read_to_string(private_key_file)
                .wrap_err_with(|| {
                    format!("failed to read p2p private key file: {private_key_file}")
                })?
                .trim()
                .to_string();
            broadcast_builder = broadcast_builder.with_keypair_hex_string(private_key_hex);
        }

        let known_peers: Vec<crate::broadcast::Multiaddr> =
            if let Some(ref p2p_known_peers) = self.config.flashblocks.p2p_known_peers {
                p2p_known_peers
                    .split(',')
                    .map(|s| s.to_string())
                    .filter_map(|s| s.parse().ok())
                    .collect()
            } else {
                vec![]
            };

        let crate::broadcast::NodeBuildResult {
            node,
            outgoing_message_tx: _,
            mut incoming_message_rxs,
        } = broadcast_builder
            .with_agent_version(AGENT_VERSION.to_string())
            .with_protocol(FLASHBLOCKS_STREAM_PROTOCOL)
            .with_known_peers(known_peers)
            .with_port(self.config.flashblocks.p2p_port)
            .with_cancellation_token(cancel.clone())
            .with_max_peer_count(self.config.flashblocks.p2p_max_peer_count)
            .try_build()
            .wrap_err("failed to build flashblocks p2p node")?;
        let multiaddrs = node.multiaddrs();
        ctx.task_executor().spawn_task(async move {
            if let Err(e) = node.run().await {
                tracing::error!(error = %e, "p2p node exited");
            }
        });
        tracing::info!(target: "payload_builder", multiaddrs = ?multiaddrs, "default p2p node started");

        let incoming_message_rx = incoming_message_rxs
            .remove(&FLASHBLOCKS_STREAM_PROTOCOL)
            .expect("flashblocks p2p protocol must be found in receiver map");

        let p2p_cache = if self.config.flashblocks.replay_from_persistence_file {
            FlashblockPayloadsCache::new(Some(ctx.config().datadir()))
        } else {
            FlashblockPayloadsCache::new(None)
        };

        let conf = ctx.config().builder.clone();
        let default_builder = reth_optimism_payload_builder::OpPayloadBuilder::with_builder_config(
            pool,
            ctx.provider().clone(),
            OpEvmConfig::optimism(ctx.chain_spec()),
            OpBuilderConfig { da_config: self.da_config, gas_limit_config: self.gas_limit_config },
        )
        .set_compute_pending_block(self.compute_pending_block);
        let payload_builder = DefaultPayloadBuilder::new(default_builder, p2p_cache.clone());
        let payload_job_config = BasicPayloadJobGeneratorConfig::default()
            .interval(conf.interval)
            .deadline(conf.deadline)
            .max_payload_tasks(conf.max_payload_tasks);

        let payload_generator = BasicPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            payload_job_config,
            payload_builder,
        );
        let (payload_service, payload_builder_handle) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        let payload_handler =
            DefaultPayloadHandler::new(incoming_message_rx, p2p_cache, ws_pub, cancel);

        ctx.task_executor()
            .spawn_critical_task("default payload builder service", Box::pin(payload_service));
        ctx.task_executor().spawn_critical_task(
            "default builder payload handler",
            Box::pin(payload_handler.run()),
        );

        tracing::info!(target: "payload_builder", "Default payload builder service started");
        Ok(payload_builder_handle)
    }
}
