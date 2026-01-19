use op_rbuilder::args::OpRbuilderArgs;
use op_rbuilder::builders::{BuilderConfig, FlashblocksServiceBuilder};
use op_rbuilder::traits::{NodeBounds, PoolBounds};
use reth::builder::components::PayloadServiceBuilder;
use reth_node_api::NodeTypes;
use reth_node_builder::{components::BasicPayloadServiceBuilder, BuilderContext};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::{args::RollupArgs, node::OpPayloadBuilder};
use reth_optimism_payload_builder::config::{OpDAConfig, OpGasLimitConfig};

/// Payload builder strategy for XLayer
enum Builder {
    /// Use FlashblocksServiceBuilder for flashblocks mode
    Flashblocks(Box<FlashblocksServiceBuilder>),
    /// Use BasicPayloadServiceBuilder with OpPayloadBuilder (which implements PayloadBuilderBuilder)
    Default(BasicPayloadServiceBuilder<OpPayloadBuilder>),
}

/// XLayer payload service builder that wraps either FlashblocksServiceBuilder or the default OpPayloadBuilder
pub struct XLayerPayloadServiceBuilder {
    builder: Builder,
}

impl XLayerPayloadServiceBuilder {
    pub fn new(
        op_rbuilder_args: OpRbuilderArgs,
        rollup_args: RollupArgs,
        da_config: OpDAConfig,
        gas_limit_config: OpGasLimitConfig,
    ) -> eyre::Result<Self> {
        let builder = if op_rbuilder_args.flashblocks.enabled {
            let builder_config = BuilderConfig::try_from(op_rbuilder_args)?;
            Builder::Flashblocks(Box::new(FlashblocksServiceBuilder(builder_config)))
        } else {
            let payload_builder = OpPayloadBuilder::new(rollup_args.compute_pending_block)
                .with_da_config(da_config)
                .with_gas_limit_config(gas_limit_config);
            Builder::Default(BasicPayloadServiceBuilder::new(payload_builder))
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
            Builder::Flashblocks(flashblocks_builder) => {
                // Use FlashblocksServiceBuilder
                flashblocks_builder.spawn_payload_builder_service(ctx, pool, evm_config).await
            }
            Builder::Default(basic_builder) => {
                // Use BasicPayloadServiceBuilder - it handles all the boilerplate!
                basic_builder.spawn_payload_builder_service(ctx, pool, evm_config).await
            }
        }
    }
}
