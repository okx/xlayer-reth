use std::sync::Arc;

use reth_db::DatabaseEnv;
use reth_node_builder::{
    FullNodeTypesAdapter, Node, NodeBuilder, NodeBuilderWithComponents, NodeTypesWithDBAdapter,
    WithLaunchContext,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_provider::providers::BlockchainProvider;

/// Convenience alias for the Base node builder type.
pub type OpNodeBuilder = WithLaunchContext<NodeBuilder<Arc<DatabaseEnv>, OpChainSpec>>;
