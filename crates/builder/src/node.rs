//! `XLayerNode` — the top-level node type definition.
//!
//! Mirrors [`reth_optimism_node::OpNode`] but pins the node types to
//! XLayer primitives ([`XLayerPrimitives`]) and engine types
//! ([`XLayerEngineTypes`]) so `NodeTypes::SignedTx =
//! XLayerTxEnvelope` and `NodeTypes::Block = XLayerBlock`. Those two
//! substitutions are what let `0x7B` transactions flow through the
//! pool → block → state-sync chain end-to-end.
//!
//! # Composition over forking
//!
//! The struct carries an [`OpNode`] instance. Configuration methods
//! (`with_da_config`, args, etc.) delegate straight through. Only
//! `NodeTypes` and `Node` trait impls — where the primitives /
//! engine-types projection matters — are overridden.
//!
//! [`XLayerPrimitives`]: crate::primitives::XLayerPrimitives
//! [`XLayerEngineTypes`]: crate::engine_types::XLayerEngineTypes
//! [`XLayerBlock`]: crate::primitives::XLayerBlock
//! [`OpNode`]: reth_optimism_node::OpNode

use alloy_consensus::Header;
use reth_node_api::NodeTypes;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{args::RollupArgs, OpNode};
use reth_optimism_payload_builder::config::{OpDAConfig, OpGasLimitConfig};
use reth_storage_api::EmptyBodyStorage;
use xlayer_consensus::XLayerTxEnvelope;

use crate::{engine_types::XLayerEngineTypes, primitives::XLayerPrimitives};

/// Top-level node configuration for the XLayer node.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct XLayerNode {
    /// Inner OP node carrying the default rollup args / DA config /
    /// gas limit. Ownership stays here so every config method
    /// upstream exposes keeps working through the newtype.
    pub inner: OpNode,
}

impl XLayerNode {
    /// Creates a new instance from the given rollup args.
    pub fn new(args: RollupArgs) -> Self {
        Self { inner: OpNode::new(args) }
    }

    /// Configure the data availability configuration.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.inner = self.inner.with_da_config(da_config);
        self
    }

    /// Configure the gas limit configuration.
    pub fn with_gas_limit_config(mut self, gas_limit_config: OpGasLimitConfig) -> Self {
        self.inner = self.inner.with_gas_limit_config(gas_limit_config);
        self
    }
}

impl NodeTypes for XLayerNode {
    type Primitives = XLayerPrimitives;
    type ChainSpec = OpChainSpec;
    // `OpStorage = EmptyBodyStorage<T, H>` is already generic. Plug
    // our signed-tx envelope in; body storage semantics are
    // identical to the OP body (just with our tx variant).
    type Storage = EmptyBodyStorage<XLayerTxEnvelope, Header>;
    type Payload = XLayerEngineTypes;
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_provider::providers::NodeTypesForProvider;

    /// Compile-gate: `XLayerNode` satisfies the `NodeTypesForProvider`
    /// bound required by `NodeBuilder::with_types_and_provider` /
    /// `BlockchainProvider`. If any super-bound regresses (ChainSpec,
    /// Storage, Primitives' trait bar), the failure surfaces here
    /// rather than in main.rs.
    #[test]
    fn xlayer_node_satisfies_node_types_for_provider() {
        fn assert_ntfp<T: NodeTypesForProvider>() {}
        assert_ntfp::<XLayerNode>();
    }
}
