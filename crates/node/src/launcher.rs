use std::fmt::Debug;

use reth::builder::Node;
use reth::providers::providers::BlockchainProvider;
use reth_node_builder::NodeHandleFor;
use reth_optimism_node::OpNode;

use crate::args::NodeArgs;
use crate::payload::XLayerPayloadServiceBuilder;
use crate::types::{OpBuilder, OpNodeBuilder};

// TODO: where you add add_ons, modules etc.
pub struct BaseBuilder {
    builder: OpBuilder,
}

impl BaseBuilder {
    pub fn new(builder: OpBuilder) -> Self {
        Self { builder }
    }

    /// Launches the node after applying accumulated hooks, delegating to the provided closure.
    pub fn launch_with_fn<L, R>(self, launcher: L) -> R
    where
        L: FnOnce(OpBuilder) -> R,
    {
        launcher(self.build())
    }

    pub fn build(self) -> OpBuilder {
        let Self { builder } = self;

        builder
    }
}

/// Customizes the node builder before launch.
///
/// Register plugins via [`NodePlugin::add_plugin`].
pub trait NodePlugin: Send + Sync + Debug {
    /// Applies the extension to the supplied builder.
    fn apply(self: Box<Self>, builder: BaseBuilder) -> BaseBuilder;
}

pub struct NodeLauncher {
    args: NodeArgs,
    plugins: Vec<Box<dyn NodePlugin>>,
}

impl NodeLauncher {
    pub async fn launch(
        &self,
        builder: OpNodeBuilder,
        args: NodeArgs,
        plugins: Vec<Box<dyn NodePlugin>>,
    ) -> eyre::Result<NodeHandleFor<OpNode>> {
        let op_node = OpNode::new(args.node_args.rollup_args.clone());

        // Create the XLayer payload service builder
        // It handles both flashblocks and default modes internally
        let payload_builder = XLayerPayloadServiceBuilder::new(args.node_args.clone())?;

        let builder = builder
            .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
            .with_components(op_node.components().payload(payload_builder))
            .with_add_ons(op_node.add_ons())
            .on_component_initialized(move |_ctx| Ok(()));

        let builder = plugins
            .into_iter()
            .fold(BaseBuilder::new(builder), |builder, plugin: Box<dyn NodePlugin>| {
                plugin.apply(builder)
            });

        builder.launch_with_fn(|builder| builder.launch()).await
    }
}
