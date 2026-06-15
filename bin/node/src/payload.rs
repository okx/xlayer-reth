use reth::builder::components::PayloadServiceBuilder;
use reth_node_api::NodeTypes;
use reth_node_builder::{components::BasicPayloadServiceBuilder, BuilderContext};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::node::OpPayloadBuilder;
use reth_optimism_payload_builder::config::{OpDAConfig, OpGasLimitConfig};
use xlayer_builder::{
    args::BuilderArgs,
    flashblocks::{BuilderConfig, FlashblocksServiceBuilder},
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
        // BLOCKED(XLOP-1118, upstream): Fallback path gasless budget injection requires
        // `OpGasLimitConfig::set_gasless_block_gas_limit(u64)` which does not exist in the pinned
        // okx/reth revision (github.com/okx/reth @ d23dd0b973ad0e4bf383f98510aa9ff63a082834,
        // crate reth-optimism-payload-builder v1.10.2, file crates/optimism/payload/src/config.rs).
        // The struct only exposes `set_gas_limit(u64)` / `gas_limit() -> Option<u64>`.
        // Once the upstream API lands, inject here:
        //   gas_limit_config.set_gasless_block_gas_limit(
        //       xlayer_builder_args.gasless_block_gas_limit().unwrap_or(0)
        //   );
        // The flashblocks path (primary sequencer path) enforces the budget via
        // `FlashblocksBuilderCtx.gasless_block_gas_limit` and is fully functional.

        let builder = if xlayer_builder_args.flashblocks.enabled {
            let builder_config = BuilderConfig::try_from(xlayer_builder_args)?;
            XLayerPayloadServiceBuilderInner::Flashblocks(Box::new(FlashblocksServiceBuilder(
                builder_config,
            )))
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
