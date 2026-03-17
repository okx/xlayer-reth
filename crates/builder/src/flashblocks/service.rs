use crate::{
    flashblocks::{
        builder::FlashblocksBuilder,
        builder_tx::FlashblocksBuilderTx,
        generator::BlockPayloadJobGenerator,
        handler::FlashblocksPayloadHandler,
        handler_ctx::FlashblockHandlerContext,
        utils::{
            cache::FlashblockPayloadsCache,
            p2p::{Message, AGENT_VERSION, FLASHBLOCKS_STREAM_PROTOCOL},
            wspub::WebSocketPublisher,
        },
        BuilderConfig,
    },
    metrics::{tokio::FlashblocksTaskMetrics, BuilderMetrics},
    traits::{NodeBounds, PoolBounds},
};
use eyre::WrapErr as _;
use std::{sync::Arc, time::Duration};

use reth_basic_payload_builder::BasicPayloadJobGeneratorConfig;
use reth_node_api::NodeTypes;
use reth_node_builder::{components::PayloadServiceBuilder, BuilderContext};
use reth_optimism_evm::OpEvmConfig;
use reth_payload_builder::{PayloadBuilderHandle, PayloadBuilderService};
use reth_provider::CanonStateSubscriptions;

pub struct FlashblocksServiceBuilder {
    pub config: BuilderConfig,
    pub bridge_intercept: xlayer_bridge_intercept::BridgeInterceptConfig,
}

impl FlashblocksServiceBuilder {
    /// Set the bridge intercept config to apply to the payload builder.
    pub fn with_bridge_intercept(
        &mut self,
        config: xlayer_bridge_intercept::BridgeInterceptConfig,
    ) -> &mut Self {
        self.bridge_intercept = config;
        self
    }

    fn spawn_payload_builder_service<Node, Pool>(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        builder_tx: FlashblocksBuilderTx,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>>
    where
        Node: NodeBounds,
        Pool: PoolBounds,
    {
        // TODO: is there a different global token?
        // this is effectively unused right now due to the usage of reth's `task_executor`.
        let cancel = tokio_util::sync::CancellationToken::new();

        let (incoming_message_rx, outgoing_message_tx) = if self.config.flashblocks.p2p_enabled {
            let mut builder = crate::p2p::NodeBuilder::new();

            if let Some(ref private_key_file) = self.config.flashblocks.p2p_private_key_file
                && !private_key_file.is_empty()
            {
                let private_key_hex = std::fs::read_to_string(private_key_file)
                    .wrap_err_with(|| {
                        format!("failed to read p2p private key file: {private_key_file}")
                    })?
                    .trim()
                    .to_string();
                builder = builder.with_keypair_hex_string(private_key_hex);
            }

            let known_peers: Vec<crate::p2p::Multiaddr> =
                if let Some(ref p2p_known_peers) = self.config.flashblocks.p2p_known_peers {
                    p2p_known_peers
                        .split(',')
                        .map(|s| s.to_string())
                        .filter_map(|s| s.parse().ok())
                        .collect()
                } else {
                    vec![]
                };

            let crate::p2p::NodeBuildResult { node, outgoing_message_tx, mut incoming_message_rxs } =
                builder
                    .with_agent_version(AGENT_VERSION.to_string())
                    .with_protocol(FLASHBLOCKS_STREAM_PROTOCOL)
                    .with_known_peers(known_peers)
                    .with_port(self.config.flashblocks.p2p_port)
                    .with_cancellation_token(cancel.clone())
                    .with_max_peer_count(self.config.flashblocks.p2p_max_peer_count)
                    .try_build::<Message>()
                    .wrap_err("failed to build flashblocks p2p node")?;
            let multiaddrs = node.multiaddrs();
            ctx.task_executor().spawn_task(async move {
                if let Err(e) = node.run().await {
                    tracing::error!(error = %e, "p2p node exited");
                }
            });
            tracing::info!(target: "payload_builder", multiaddrs = ?multiaddrs, "flashblocks p2p node started");

            let incoming_message_rx = incoming_message_rxs
                .remove(&FLASHBLOCKS_STREAM_PROTOCOL)
                .expect("flashblocks p2p protocol must be found in receiver map");
            (incoming_message_rx, outgoing_message_tx)
        } else {
            let (_incoming_message_tx, incoming_message_rx) = tokio::sync::mpsc::channel(16);
            let (outgoing_message_tx, _outgoing_message_rx) = tokio::sync::mpsc::channel(16);
            (incoming_message_rx, outgoing_message_tx)
        };

        let metrics = Arc::new(BuilderMetrics::default());
        let task_metrics = Arc::new(FlashblocksTaskMetrics::new());

        // Channels for built flashblock payloads
        let (built_fb_payload_tx, built_fb_payload_rx) = tokio::sync::mpsc::channel(16);
        // Channels for built full block payloads
        let (built_payload_tx, built_payload_rx) = tokio::sync::mpsc::channel(16);

        let p2p_cache = if self.config.flashblocks.replay_from_persistence_file {
            FlashblockPayloadsCache::new(Some(ctx.config().datadir()))
        } else {
            FlashblockPayloadsCache::new(None)
        };

        let ws_pub: Arc<WebSocketPublisher> = WebSocketPublisher::new(
            self.config.flashblocks.ws_addr,
            metrics.clone(),
            &task_metrics.websocket_publisher,
            self.config.flashblocks.ws_subscriber_limit,
        )
        .wrap_err("failed to create ws publisher")?
        .into();
        let mut payload_builder = FlashblocksBuilder::new(
            OpEvmConfig::optimism(ctx.chain_spec()),
            pool,
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            self.config.clone(),
            builder_tx,
            built_fb_payload_tx,
            built_payload_tx,
            p2p_cache.clone(),
            ws_pub.clone(),
            metrics.clone(),
            task_metrics.clone(),
        );
        payload_builder.bridge_intercept_config = self.bridge_intercept.clone();
        let payload_job_config = BasicPayloadJobGeneratorConfig::default();

        let payload_generator = BlockPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            payload_job_config,
            payload_builder,
            true,
            self.config.block_time_leeway,
        );

        let (payload_service, payload_builder_handle) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        let handler_ctx = FlashblockHandlerContext::new(
            &ctx.provider().clone(),
            self.config.clone(),
            OpEvmConfig::optimism(ctx.chain_spec()),
            metrics.clone(),
        )
        .wrap_err("failed to create flashblocks payload builder context")?;

        let payload_handler = FlashblocksPayloadHandler::new(
            handler_ctx,
            built_fb_payload_rx,
            built_payload_rx,
            incoming_message_rx,
            outgoing_message_tx,
            payload_service.payload_events_handle(),
            p2p_cache.clone(),
            ws_pub.clone(),
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            cancel,
            self.config.flashblocks.p2p_send_full_payload,
            self.config.flashblocks.p2p_process_full_payload,
        );

        ctx.task_executor().spawn_critical_task(
            "custom payload builder service",
            Box::pin(task_metrics.payload_builder_service.instrument(payload_service)),
        );
        ctx.task_executor().spawn_critical_task(
            "flashblocks payload handler",
            Box::pin(task_metrics.payload_handler.instrument(payload_handler.run())),
        );

        // Spawn the tokio metrics collector (records metrics every second)
        task_metrics.clone().spawn_metrics_collector(Duration::from_secs(1));

        tracing::info!(target: "payload_builder", "Flashblocks payload builder service started");
        Ok(payload_builder_handle)
    }
}

impl<Node, Pool> PayloadServiceBuilder<Node, Pool, OpEvmConfig> for FlashblocksServiceBuilder
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
        let signer = self.config.builder_signer;

        let builder_tx = if let Some(builder_signer) = signer
            && let Some(flashblocks_number_contract_address) =
                self.config.flashblocks.number_contract_address
        {
            FlashblocksBuilderTx::new_number_contract(
                builder_signer,
                flashblocks_number_contract_address,
            )
        } else {
            FlashblocksBuilderTx::new_base(signer)
        };

        self.spawn_payload_builder_service(ctx, pool, builder_tx)
    }
}
