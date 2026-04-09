use reth::builder::components::PayloadServiceBuilder;
use reth_node_api::NodeTypes;
use reth_node_builder::BuilderContext;
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_payload_builder::config::{OpDAConfig, OpGasLimitConfig};
use xlayer_bridge_intercept::BridgeInterceptConfig;
use xlayer_builder::{
    args::BuilderArgs,
    default::DefaultBuilderServiceBuilder,
    flashblocks::{BuilderConfig, FlashblocksServiceBuilder},
    traits::{NodeBounds, PoolBounds},
};

/// Payload builder strategy for X Layer.
enum XLayerPayloadServiceBuilderInner {
    /// Uses [`FlashblocksServiceBuilder`] for sequencer nodes producing flashblocks.
    Flashblocks(Box<FlashblocksServiceBuilder>),
    /// Uses [`DefaultBuilderServiceBuilder`] that wraps the default OP builder with
    /// builder p2p and flashblocks reorg protection.
    Default(Box<DefaultBuilderServiceBuilder>),
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
    ) -> eyre::Result<Self> {
        Self::with_config(
            xlayer_builder_args,
            compute_pending_block,
            OpDAConfig::default(),
            OpGasLimitConfig::default(),
        )
    }

    pub fn with_config(
        xlayer_builder_args: BuilderArgs,
        compute_pending_block: bool,
        da_config: OpDAConfig,
        gas_limit_config: OpGasLimitConfig,
    ) -> eyre::Result<Self> {
        let flashblocks_enabled = xlayer_builder_args.flashblocks.enabled;
        let config = BuilderConfig::try_from(xlayer_builder_args)?;
        let builder = if flashblocks_enabled {
            XLayerPayloadServiceBuilderInner::Flashblocks(Box::new(FlashblocksServiceBuilder {
                config,
                bridge_intercept: Default::default(),
            }))
        } else {
            XLayerPayloadServiceBuilderInner::Default(Box::new(DefaultBuilderServiceBuilder {
                compute_pending_block,
                config,
                da_config,
                gas_limit_config,
            }))
        };
        Ok(Self { builder })
    }

    /// Apply bridge intercept config to the flashblocks builder.
    pub fn with_bridge_config(mut self, config: BridgeInterceptConfig) -> Self {
        if let XLayerPayloadServiceBuilderInner::Flashblocks(ref mut fb) = self.builder {
            fb.with_bridge_intercept(config);
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
            XLayerPayloadServiceBuilderInner::Default(default_builder) => {
                default_builder.spawn_payload_builder_service(ctx, pool, evm_config).await
            }
        }
    }
}
