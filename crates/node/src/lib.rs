//! XLayer node with custom EVM configuration.

// Re-export OpNode.
pub use reth_optimism_node::OpNode as XLayerNode;

// Export required types.
pub use xlayer_evm::{XLayerEvmConfig, xlayer_evm_config};
pub use reth_node_builder::components::ExecutorBuilder;
pub use reth_node_builder::{BuilderContext, FullNodeTypes};
pub use reth_node_api::NodeTypes;
pub use reth_optimism_chainspec::OpChainSpec;
pub use reth_optimism_primitives::OpPrimitives;

/// XLayer ExecutorBuilder using XLayerEvmConfig.
#[derive(Debug, Default, Clone, Copy)]
pub struct XLayerExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for XLayerExecutorBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypes<
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
        >
    >,
{
    type EVM = XLayerEvmConfig;

    async fn build_evm(
        self,
        ctx: &BuilderContext<Node>,
    ) -> eyre::Result<Self::EVM> {
        tracing::info!(
            target: "xlayer::executor",
            "🔧 Building XLayer EVM (Poseidon precompile will be loaded at runtime)"
        );
        Ok(xlayer_evm_config(ctx.chain_spec()))
    }
}
