use reth::builder::components::PayloadServiceBuilder;
use reth_node_api::NodeTypes;
use reth_node_builder::{components::BasicPayloadServiceBuilder, BuilderContext};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::node::OpPayloadBuilder;
use reth_optimism_payload_builder::config::{OpDAConfig, OpGasLimitConfig};
use xlayer_builder::{
    args::OpRbuilderArgs,
    payload::{BuilderConfig, FlashblocksServiceBuilder},
    traits::{NodeBounds, PoolBounds},
};

/// Payload builder strategy for X Layer.
enum XLayerPayloadServiceBuilderInner {
    /// Uses [`FlashblocksServiceBuilder`] for sequencer nodes producing flashblocks.
    Flashblocks(Box<FlashblocksServiceBuilder>),
    /// Uses [`BasicPayloadServiceBuilder`] with [`OpPayloadBuilder`] for follower/RPC nodes.
    Default(BasicPayloadServiceBuilder<OpPayloadBuilder>),
}

/// The X Layer payload service builder that delegates to either [`FlashblocksServiceBuilder`]
/// or the default [`BasicPayloadServiceBuilder`].
pub struct XLayerPayloadServiceBuilder {
    builder: XLayerPayloadServiceBuilderInner,
}

impl XLayerPayloadServiceBuilder {
    pub fn new(xlayer_builder_args: OpRbuilderArgs) -> eyre::Result<Self> {
        Self::with_config(xlayer_builder_args, OpDAConfig::default(), OpGasLimitConfig::default())
    }

    pub fn with_config(
        xlayer_builder_args: OpRbuilderArgs,
        da_config: OpDAConfig,
        gas_limit_config: OpGasLimitConfig,
    ) -> eyre::Result<Self> {
        let builder = if xlayer_builder_args.flashblocks.enabled {
            let builder_config = BuilderConfig::try_from(xlayer_builder_args)?;
            XLayerPayloadServiceBuilderInner::Flashblocks(Box::new(FlashblocksServiceBuilder(
                builder_config,
            )))
        } else {
            let payload_builder =
                OpPayloadBuilder::new(xlayer_builder_args.rollup_args.compute_pending_block)
                    .with_da_config(da_config)
                    .with_gas_limit_config(gas_limit_config);
            XLayerPayloadServiceBuilderInner::Default(BasicPayloadServiceBuilder::new(
                payload_builder,
            ))
        };

        Ok(Self { builder })
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
                // Use FlashblocksServiceBuilder
                flashblocks_builder.spawn_payload_builder_service(ctx, pool, evm_config).await
            }
            XLayerPayloadServiceBuilderInner::Default(basic_builder) => {
                // Use BasicPayloadServiceBuilder - it handles all the boilerplate!
                basic_builder.spawn_payload_builder_service(ctx, pool, evm_config).await
            }
        }
    }
}
