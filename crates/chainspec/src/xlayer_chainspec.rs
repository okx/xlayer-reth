//! [`XLayerChainSpec`] — thin newtype over
//! [`Arc<OpChainSpec>`][OpChainSpec] that exposes a distinct type
//! identity for XLayer chains while reusing every OP-side trait impl
//! via `Deref`.
//!
//! **What this is for.** Most execution-layer code keeps taking
//! `Arc<OpChainSpec>` (pool builder, payload builder, executor, RPC),
//! because those are generic over the `ChainSpec` associated type and
//! `OpChainSpec` is what reth's `OpNode` produces. The wrapper comes
//! in as an escape hatch for XLayer-specific plumbing that wants a
//! stronger-typed argument — e.g. a genesis-state installer that
//! should only accept an XLayer chainspec, or future metadata that
//! doesn't fit into `ChainHardforks`.
//!
//! **What this is *not*.** It is **not** a wholesale replacement for
//! `OpChainSpec` in the builder chain (`NodeBounds`, `XLayerEvmConfig`,
//! etc. stay pinned to `ChainSpec = OpChainSpec` — changing that would
//! cascade through reth's trait hierarchy and isn't needed just for
//! XLayerAA activation). The AA-active query already works on plain
//! `OpChainSpec` through the blanket [`crate::XLayerHardforks`] impl.
//!
//! Concrete usage today is limited — this type exists so future
//! XLayer-exclusive forks, metadata caches, or typed newtype
//! boundaries have a home without requiring another refactor.

use std::{ops::Deref, sync::Arc};

use reth_optimism_chainspec::OpChainSpec;

/// XLayer chainspec wrapper.
///
/// Holds an `Arc<OpChainSpec>` and implements `Deref<Target = OpChainSpec>`,
/// so any read-only API that takes `&OpChainSpec` (or the trait impls
/// on it — `OpHardforks`, `Hardforks`, `EthChainSpec`, …) works through
/// the wrapper without a ceremony. Write methods on the inner `OpChainSpec`
/// are deliberately not re-exposed: XLayer chainspec state is
/// immutable after construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XLayerChainSpec {
    inner: Arc<OpChainSpec>,
}

impl XLayerChainSpec {
    /// Wraps an existing [`Arc<OpChainSpec>`].
    pub fn new(inner: Arc<OpChainSpec>) -> Self {
        Self { inner }
    }

    /// Returns the inner `Arc<OpChainSpec>` — convenient for callers
    /// that need to pass the underlying chainspec into reth APIs still
    /// typed on `OpChainSpec`.
    pub fn inner(&self) -> &Arc<OpChainSpec> {
        &self.inner
    }
}

impl Deref for XLayerChainSpec {
    type Target = OpChainSpec;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl From<Arc<OpChainSpec>> for XLayerChainSpec {
    fn from(inner: Arc<OpChainSpec>) -> Self {
        Self::new(inner)
    }
}

impl From<XLayerChainSpec> for Arc<OpChainSpec> {
    fn from(cs: XLayerChainSpec) -> Self {
        cs.inner
    }
}
