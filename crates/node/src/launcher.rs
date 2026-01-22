use std::fmt::Debug;
use std::sync::Arc;

use reth_db::DatabaseEnv;
use reth_node_builder::NodeTypesWithDBAdapter;
use reth_node_builder::NodeHandleFor;
use reth_optimism_node::{OpNode, args::RollupArgs};
use reth::builder::Node;
use reth::providers::providers::BlockchainProvider;

use crate::args::NodeArgs;
use crate::types::OpNodeBuilder;
use crate::payload::XLayerPayloadServiceBuilder;

// TODO: where you add add_ons, modules etc.
struct BaseBuilder;

/// Customizes the node builder before launch.
///
/// Register plugins via [`NodePlugin::add_plugin`].
trait NodePlugin: Send + Sync + Debug {
    /// Applies the extension to the supplied builder.
    fn apply(self: Box<Self>, builder: BaseBuilder) -> BaseBuilder;
}

pub struct NodeLauncher {
    args: NodeArgs,
    plugins: Vec<Box<dyn NodePlugin>>,
}

impl NodeLauncher {
    
    pub async fn launch(&self, builder: OpNodeBuilder, args: NodeArgs) -> eyre::Result<NodeHandleFor<OpNode>> {
        let op_node = OpNode::new(args.node_args.rollup_args.clone());

        // Create the XLayer payload service builder
        // It handles both flashblocks and default modes internally
        let payload_builder = XLayerPayloadServiceBuilder::new(args.node_args.clone())?;

        let builder = builder
            .with_types_and_provider::<OpNode, BlockchainProvider<NodeTypesWithDBAdapter<OpNode, Arc<DatabaseEnv>>>>()
            .with_components(op_node.components().payload(payload_builder))
            .with_add_ons(op_node.add_ons())
            .on_component_initialized(move |_ctx| Ok(()));

        builder.launch_with_fn(|builder| {
            builder.launch()
        }).await
    }
}
