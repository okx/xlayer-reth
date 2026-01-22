use std::sync::Arc;

use reth::builder::components::ComponentsBuilder;
use reth_db::DatabaseEnv;
use reth_node_builder::{
    FullNodeTypesAdapter, Node, NodeBuilder, NodeBuilderWithComponents, NodeTypesWithDBAdapter,
    WithLaunchContext,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{
    OpConsensusBuilder, OpExecutorBuilder, OpNetworkBuilder, OpNode, OpPoolBuilder,
};
use reth_provider::providers::BlockchainProvider;

use crate::payload::XLayerPayloadServiceBuilder;

/// Internal alias for the OP node type adapter.
pub(crate) type OpNodeTypes = FullNodeTypesAdapter<OpNode, Arc<DatabaseEnv>, OpProvider>;
/// Internal alias for the OP node add-ons.
pub(crate) type OpAddOns = <OpNode as Node<OpNodeTypes>>::AddOns;

/// A [`BlockchainProvider`] instance.
pub type OpProvider = BlockchainProvider<NodeTypesWithDBAdapter<OpNode, Arc<DatabaseEnv>>>;

/// Convenience alias for the Base node builder type.
pub type OpNodeBuilder = WithLaunchContext<NodeBuilder<Arc<DatabaseEnv>, OpChainSpec>>;

/// A ComponentsBuilder with XLayer's custom payload builder
pub type XLayerComponentsBuilder<Node> = ComponentsBuilder<
    Node,
    OpPoolBuilder,
    XLayerPayloadServiceBuilder,
    OpNetworkBuilder,
    OpExecutorBuilder,
    OpConsensusBuilder,
>;

/// OP Builder is a [`WithLaunchContext`] reth node builder.
pub type OpBuilder = WithLaunchContext<
    NodeBuilderWithComponents<OpNodeTypes, XLayerComponentsBuilder<OpNodeTypes>, OpAddOns>,
>;
