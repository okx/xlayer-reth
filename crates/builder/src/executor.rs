//! XLayerAA-aware block executor builder.
//!
//! Mirrors [`reth_optimism_node::node::OpExecutorBuilder`] one-for-one
//! except the returned [`ConfigureEvm`] plugs in
//! [`XLayerAAEvmFactory`] instead of the upstream `OpEvmFactory`. The
//! upstream builder calls `OpEvmConfig::new(...)`, which hard-codes
//! `OpEvmFactory<OpTx>` and therefore misses the XLayerAA handler for
//! type-`0x7B` transactions.
//!
//! **Why this exists.** The executor builder runs whenever the node
//! **replays** a block rather than producing one — peer block import,
//! engine API `newPayload`, state sync. E4a flipped the payload path
//! to `XLayerEvmConfig`; without this executor twin, a peer block
//! carrying a `0x7B` tx would dispatch to `OpEvmFactory` and fail at
//! the tx-type branch. Payload and executor must consume the same
//! `ConfigureEvm` so block validation round-trips with block
//! production.
//!
//! **Why the `EVM` type is Node-parameterised.** Matches
//! `OpExecutorBuilder::EVM`'s shape — `<Node::Types as
//! NodeTypes>::ChainSpec / Primitives` — so reth's type inference can
//! propagate the Node's associated types through cleanly. An earlier
//! attempt that pinned `type EVM = XLayerEvmConfig` (concrete
//! `OpChainSpec`/`OpPrimitives`) made
//! `ComponentsBuilder<_, ..., ..., ..., ..., ...>:
//! NodeComponentsBuilder<...>` fail to resolve the
//! `BlockchainProvider<_>` type param.
//!
//! Flashblocks' `FlashblockSequenceValidator`
//! (`bin/node/src/main.rs:189`) and flashblocks sequencer's internal
//! EVM plumbing (`crates/builder/src/flashblocks/service.rs:132,162`)
//! are intentionally **not** touched by this builder — they remain on
//! upstream `OpEvmConfig` until E4b.
//!
//! [`ConfigureEvm`]: reth_evm::ConfigureEvm
//! [`XLayerAAEvmFactory`]: xlayer_revm::XLayerAAEvmFactory

use core::marker::PhantomData;

use alloy_op_evm::OpBlockExecutorFactory;
use reth_node_api::{FullNodeTypes, NodeTypes};
use reth_node_builder::{components::ExecutorBuilder, BuilderContext};
use reth_optimism_evm::{OpBlockAssembler, OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_forks::OpHardforks;
use reth_optimism_primitives::OpPrimitives;
use revm::context::TxEnv;
use xlayer_revm::{XLayerAAEvmFactory, XLayerAATransaction};

/// Executor builder that produces an `OpEvmConfig` parametrised with
/// [`XLayerAAEvmFactory`] instead of the upstream `OpEvmFactory`.
#[derive(Debug, Copy, Clone, Default)]
#[non_exhaustive]
pub struct XLayerExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for XLayerExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: OpHardforks, Primitives = OpPrimitives>>,
{
    type EVM = OpEvmConfig<
        <Node::Types as NodeTypes>::ChainSpec,
        <Node::Types as NodeTypes>::Primitives,
        OpRethReceiptBuilder,
        XLayerAAEvmFactory<XLayerAATransaction<TxEnv>>,
    >;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let chain_spec = ctx.chain_spec();
        Ok(OpEvmConfig {
            block_assembler: OpBlockAssembler::new(chain_spec.clone()),
            executor_factory: OpBlockExecutorFactory::new(
                OpRethReceiptBuilder::default(),
                chain_spec,
                XLayerAAEvmFactory::<XLayerAATransaction<TxEnv>>::default(),
            ),
            _pd: PhantomData,
        })
    }
}
