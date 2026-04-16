use std::sync::{Arc, OnceLock};

use reth::builder::components::PayloadServiceBuilder;
use reth_node_api::NodeTypes;
use reth_node_builder::{components::BasicPayloadServiceBuilder, BuilderContext};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::node::OpPayloadBuilder;
use reth_optimism_payload_builder::config::{OpDAConfig, OpGasLimitConfig};
use xlayer_bridge_intercept::BridgeInterceptConfig;
use xlayer_builder::{
    args::BuilderArgs,
    broadcast::PeerStatusTracker,
    default::DefaultBuilderServiceBuilder,
    flashblocks::{BuilderConfig, FlashblocksServiceBuilder},
    traits::{NodeBounds, PoolBounds},
};
use xlayer_eip8130_pool::AaPoolHandle;

/// Payload builder strategy for X Layer.
enum XLayerPayloadServiceBuilderInner {
    /// Uses [`FlashblocksServiceBuilder`] for sequencer nodes producing flashblocks.
    Flashblocks(Box<FlashblocksServiceBuilder>),
    /// Uses [`DefaultBuilderServiceBuilder`] that wraps the default OP builder with
    /// builder p2p and flashblocks reorg protection.
    DefaultWithP2P(Box<DefaultBuilderServiceBuilder>),
    /// Uses [`BasicPayloadServiceBuilder`] with [`OpPayloadBuilder`] for RPC nodes.
    Default(BasicPayloadServiceBuilder<OpPayloadBuilder>),
}

/// The X Layer payload service builder that builds [`FlashblocksServiceBuilder`] if
/// flashblocks are enabled, otherwise builds [`DefaultBuilderServiceBuilder`].
pub struct XLayerPayloadServiceBuilder {
    builder: XLayerPayloadServiceBuilderInner,
}

impl XLayerPayloadServiceBuilder {
    pub fn new(
        xlayer_builder_args: BuilderArgs,
        compute_pending_block: bool,
        sequencer_mode: bool,
        peer_status: Arc<OnceLock<PeerStatusTracker>>,
    ) -> eyre::Result<Self> {
        Self::with_config(
            xlayer_builder_args,
            compute_pending_block,
            sequencer_mode,
            peer_status,
            OpDAConfig::default(),
            OpGasLimitConfig::default(),
        )
    }

    pub fn with_config(
        xlayer_builder_args: BuilderArgs,
        compute_pending_block: bool,
        sequencer_mode: bool,
        peer_status: Arc<OnceLock<PeerStatusTracker>>,
        da_config: OpDAConfig,
        gas_limit_config: OpGasLimitConfig,
    ) -> eyre::Result<Self> {
        let flashblocks_enabled = xlayer_builder_args.flashblocks.enabled;
        let builder = if sequencer_mode {
            let config = BuilderConfig::try_from(xlayer_builder_args)?;
            if flashblocks_enabled {
                XLayerPayloadServiceBuilderInner::Flashblocks(Box::new(FlashblocksServiceBuilder {
                    config,
                    bridge_intercept: Default::default(),
                    peer_status_sink: peer_status.clone(),
                    aa_pool: None,
                }))
            } else {
                XLayerPayloadServiceBuilderInner::DefaultWithP2P(Box::new(
                    DefaultBuilderServiceBuilder {
                        compute_pending_block,
                        config,
                        da_config,
                        gas_limit_config,
                        peer_status_sink: peer_status.clone(),
                    },
                ))
            }
        } else {
            let payload_builder = OpPayloadBuilder::new(compute_pending_block)
                .with_da_config(da_config)
                .with_gas_limit_config(gas_limit_config);
            XLayerPayloadServiceBuilderInner::Default(BasicPayloadServiceBuilder::new(
                payload_builder,
            ))
        };
        Ok(Self { builder })
    }

    /// Sets the AA side pool handle for EIP-8130 merged transaction sourcing.
    /// Only the flashblocks builder supports AA pool integration.
    pub fn with_aa_pool(mut self, handle: AaPoolHandle) -> Self {
        if let XLayerPayloadServiceBuilderInner::Flashblocks(fb) = &mut self.builder {
            fb.aa_pool = Some(handle);
        }
        self
    }

    /// Apply bridge intercept config. Only the flashblocks builder supports bridge
    /// intercept — the default builder runs unmodified upstream `OpPayloadBuilder` logic
    /// as a failsafe, so bridge filtering is intentionally not applied.
    pub fn with_bridge_config(mut self, config: BridgeInterceptConfig) -> Self {
        match &mut self.builder {
            XLayerPayloadServiceBuilderInner::Flashblocks(fb) => {
                fb.with_bridge_intercept(config);
            }
            // DefaultWithP2P runs the upstream OpPayloadBuilder as a failsafe during
            // conductor failover — bridge intercept is not supported on this path.
            XLayerPayloadServiceBuilderInner::DefaultWithP2P(_) => {}
            XLayerPayloadServiceBuilderInner::Default(_) => {}
        }
        self
    }
}

impl<Node, Pool> PayloadServiceBuilder<Node, Pool, OpEvmConfig> for XLayerPayloadServiceBuilder
where
    Node: NodeBounds,
    Pool: PoolBounds,
{
    async fn spawn_payload_builder_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: OpEvmConfig,
    ) -> eyre::Result<reth_payload_builder::PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>>
    {
        match self.builder {
            XLayerPayloadServiceBuilderInner::Flashblocks(flashblocks_builder) => {
                flashblocks_builder.spawn_payload_builder_service(ctx, pool, evm_config).await
            }
            XLayerPayloadServiceBuilderInner::DefaultWithP2P(default_builder) => {
                default_builder.spawn_payload_builder_service(ctx, pool, evm_config).await
            }
            XLayerPayloadServiceBuilderInner::Default(basic_builder) => {
                basic_builder.spawn_payload_builder_service(ctx, pool, evm_config).await
            }
        }
    }
}
