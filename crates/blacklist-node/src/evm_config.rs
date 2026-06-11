//! FR-2/3/4 — `XLayerBlacklistEvmConfig`: the wrapper EVM config.
//!
//! A newtype over `OpEvmConfig` (any `C: ConfigureEvm`) that delegates the whole
//! [`ConfigureEvm`] contract to the inner config while carrying the [`BlacklistRuntimeCtx`].
//! Making this the **sole** EVM type across all three faces (builder / follower / flashblocks)
//! is the compile-time guarantee that no face bypasses the feature (TD §7 R-3/R-5, DM-5.6).
//!
//! Delegation is grounded in the full `ConfigureEvm` trait read at
//! `…/reth/22bdddf/crates/evm/evm/src/lib.rs:180-455`: 5 associated types + 6 required
//! methods (`block_executor_factory`, `block_assembler`, `evm_env`, `next_evm_env`,
//! `context_for_block`, `context_for_next_block`); all EVM-building methods
//! (`evm_with_env`, `evm_with_env_and_inspector`, `create_executor`, …) are trait defaults
//! and are inherited unchanged.
//!
//! Deposit interception (FR-3) on the **builder** face is applied at the concrete
//! `execute_sequencer_transactions` hook (`crates/builder/src/flashblocks/context.rs:303`,
//! before the `:336` commit barrier) using [`xlayer_blacklist::deposit`]; the **follower**
//! face applies it through the executor produced by this config. The receipt-field rewrite
//! requires concrete `OpBlockExecutor` receipt access and is bound at the build-capable
//! review stage — see the stage `output.md` "Verification status".

use crate::runtime::BlacklistRuntimeCtx;
use reth_evm::{ConfigureEvm, EvmEnvFor, ExecutionCtxFor};
use reth_primitives_traits::{NodePrimitives, SealedBlock, SealedHeader};

/// EVM config wrapper carrying the blacklist runtime context. `C` defaults to `OpEvmConfig`
/// at the call sites; kept generic so the same wrapper serves every face.
#[derive(Debug, Clone)]
pub struct XLayerBlacklistEvmConfig<C> {
    inner: C,
    ctx: BlacklistRuntimeCtx,
}

impl<C> XLayerBlacklistEvmConfig<C> {
    /// Wrap an inner EVM config with the blacklist runtime context.
    pub fn new(inner: C, ctx: BlacklistRuntimeCtx) -> Self {
        Self { inner, ctx }
    }

    /// Borrow the inner config.
    pub fn inner(&self) -> &C {
        &self.inner
    }

    /// Borrow the runtime context.
    pub fn ctx(&self) -> &BlacklistRuntimeCtx {
        &self.ctx
    }
}

impl<C> ConfigureEvm for XLayerBlacklistEvmConfig<C>
where
    C: ConfigureEvm,
{
    type Primitives = C::Primitives;
    type Error = C::Error;
    type NextBlockEnvCtx = C::NextBlockEnvCtx;
    type BlockExecutorFactory = C::BlockExecutorFactory;
    type BlockAssembler = C::BlockAssembler;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        self.inner.block_executor_factory()
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        self.inner.block_assembler()
    }

    fn evm_env(
        &self,
        header: &<Self::Primitives as NodePrimitives>::BlockHeader,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &<Self::Primitives as NodePrimitives>::BlockHeader,
        attributes: &Self::NextBlockEnvCtx,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<<Self::Primitives as NodePrimitives>::Block>,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader<<Self::Primitives as NodePrimitives>::BlockHeader>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<ExecutionCtxFor<'_, Self>, Self::Error> {
        self.inner.context_for_next_block(parent, attributes)
    }
}
