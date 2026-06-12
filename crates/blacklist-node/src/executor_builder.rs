//! FR-3/FR-5 (follower face) — node-builder `ExecutorBuilder` that installs the blacklist
//! deposit hook WITHOUT changing the EVM-config type.
//!
//! Critically, this builds the **same** `OpEvmConfig<ChainSpec, Primitives>` type that the
//! upstream `OpExecutorBuilder` produces (so `N::Evm` is unchanged and the OpAddOns / engine
//! / RPC stack still resolves — avoiding the type-pinning wall, see
//! [[upstream-component-type-pinning]]). The only difference is that the deposit-intercept
//! hook is attached to the config's (public) `executor_factory` field via the
//! `with_blacklist_hook` setter added on `OpBlockExecutorFactory` (XLOP-1100). With no hook
//! / disabled chain the produced config is byte-identical to the upstream default.
//!
//! Grounded signatures (verbatim):
//! - `impl<Node> ExecutorBuilder<Node> for OpExecutorBuilder { type EVM =
//!   OpEvmConfig<ChainSpec, Primitives>; fn build_evm = OpEvmConfig::new(ctx.chain_spec(),
//!   OpRethReceiptBuilder::default()) }` — op-reth `node/src/node.rs:982-988`.

use crate::follower_hook::XLayerDepositBlacklistHook;
use crate::runtime::BlacklistRuntimeCtx;
use alloy_op_evm::block::DepositBlacklistHook;
use reth_node_api::NodeTypes;
use reth_node_builder::{components::ExecutorBuilder, BuilderContext, FullNodeTypes};
use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_forks::OpHardforks;
use reth_optimism_primitives::OpPrimitives;
use std::sync::Arc;

/// Node-builder executor component that attaches the blacklist deposit hook to the
/// follower / newPayload-validation EVM config, keeping the upstream `OpEvmConfig` type.
#[derive(Debug, Clone)]
pub struct XLayerExecutorBuilder {
    ctx: BlacklistRuntimeCtx,
}

impl XLayerExecutorBuilder {
    /// New builder carrying the shared blacklist runtime context.
    pub fn new(ctx: BlacklistRuntimeCtx) -> Self {
        Self { ctx }
    }
}

impl<Node> ExecutorBuilder<Node> for XLayerExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: OpHardforks, Primitives = OpPrimitives>>,
{
    type EVM =
        OpEvmConfig<<Node::Types as NodeTypes>::ChainSpec, <Node::Types as NodeTypes>::Primitives>;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let mut evm_config = OpEvmConfig::new(ctx.chain_spec(), OpRethReceiptBuilder::default());
        let hook: Arc<dyn DepositBlacklistHook> =
            Arc::new(XLayerDepositBlacklistHook::new(self.ctx));
        evm_config.executor_factory = evm_config.executor_factory.with_blacklist_hook(Some(hook));
        Ok(evm_config)
    }
}
