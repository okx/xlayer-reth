//! Extension trait for querying XLayer-specific fork activations.
//!
//! Blanket-impl'd on any [`Hardforks`] type — so `OpChainSpec`,
//! [`crate::XLayerChainSpec`], or a plain `Arc<OpChainSpec>` all
//! expose the same query API without needing a chainspec wrapper at
//! every call site.

use reth_ethereum_forks::Hardforks;

use crate::hardfork::XLayerHardfork;

/// XLayer-specific fork-activation queries on top of generic
/// [`Hardforks`]. Adds one method per XLayerHardfork variant — grows
/// in lock-step with [`XLayerHardfork`].
pub trait XLayerHardforks: Hardforks {
    /// Returns `true` once
    /// [`XLayerHardfork::XLayerAA`] is active at `timestamp`.
    /// XLayerAA activation enables EIP-8130 account-abstraction
    /// transactions (type `0x7B`) and installs the AA predeploy set.
    fn is_xlayer_aa_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.fork(XLayerHardfork::XLayerAA).active_at_timestamp(timestamp)
    }
}

impl<T: Hardforks> XLayerHardforks for T {}
