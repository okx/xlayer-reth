//! FR-2/3 (follower face) — node-builder `ExecutorBuilder` producing the wrapper EVM config.
//!
//! In this reth version `ExecutorBuilder` has only `type EVM: ConfigureEvm` + `build_evm`
//! (no separate `Executor` type — the executor is produced at runtime by
//! `ConfigureEvm::create_executor`). So this builder delegates to op-reth's
//! `OpExecutorBuilder` to construct the inner `OpEvmConfig`, then wraps it in
//! [`XLayerBlacklistEvmConfig`] carrying the chain-keyed [`BlacklistRuntimeCtx`].
//!
//! Grounded signatures (verbatim):
//! - `ExecutorBuilder { type EVM: ConfigureEvm<Primitives = PrimitivesTy<Node::Types>> + 'static;
//!   fn build_evm(self, &BuilderContext<Node>) -> impl Future<Output = eyre::Result<Self::EVM>> }`
//!   — reth `crates/node/builder/src/components/execute.rs:7-18`.
//! - `OpExecutorBuilder::build_evm` builds `OpEvmConfig::new(ctx.chain_spec(), OpRethReceiptBuilder::default())`
//!   — op-reth `node/src/node.rs` (`impl ExecutorBuilder for OpExecutorBuilder`).

use crate::runtime::BlacklistRuntimeCtx;
use crate::XLayerBlacklistEvmConfig;
use reth_chainspec::EthChainSpec;
use reth_node_builder::{components::ExecutorBuilder, BuilderContext, FullNodeTypes};
use reth_optimism_node::node::OpExecutorBuilder;

/// Node-builder executor component that installs the blacklist wrapper EVM config on the
/// follower / executor face. Wraps the default [`OpExecutorBuilder`].
#[derive(Debug, Default, Clone)]
pub struct XLayerExecutorBuilder {
    inner: OpExecutorBuilder,
}

impl XLayerExecutorBuilder {
    /// New builder wrapping the default OP executor builder.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<Node> ExecutorBuilder<Node> for XLayerExecutorBuilder
where
    Node: FullNodeTypes,
    OpExecutorBuilder: ExecutorBuilder<Node>,
{
    type EVM = XLayerBlacklistEvmConfig<<OpExecutorBuilder as ExecutorBuilder<Node>>::EVM>;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let inner_evm = self.inner.build_evm(ctx).await?;
        let chain_id = ctx.chain_spec().chain().id();
        Ok(XLayerBlacklistEvmConfig::new(inner_evm, BlacklistRuntimeCtx::new(chain_id)))
    }
}
