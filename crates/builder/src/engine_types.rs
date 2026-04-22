//! Engine-API types for the XLayer node.
//!
//! Two ZST markers live here:
//!
//! - [`XLayerPayloadTypes`] — a type alias for
//!   [`OpPayloadTypes<XLayerPrimitives>`], leveraging upstream's
//!   existing generic over `NodePrimitives`. No fork needed;
//!   `OpBuiltPayload<N>: BuiltPayload` is blanket-impl'd for any
//!   `N: NodePrimitives`.
//!
//! - [`XLayerEngineTypes`] — a **fork** of [`OpEngineTypes`]. The
//!   fork exists because [`OpEngineTypes`]'s [`EngineTypes`] impl
//!   pins `BuiltPayload::Primitives::Block = OpBlock`
//!   (concretely), and [`XLayerBlock = Block<XLayerTxEnvelope>`] is
//!   a different concrete type than [`OpBlock = Block<OpTxEnvelope>`].
//!   We mirror upstream's impl with one change: drop the `Block =
//!   OpBlock` constraint and let the payload envelope conversions
//!   carry the type-parameter weight themselves (they're already
//!   generic on `N: NodePrimitives<Block = Block<T>>` for any
//!   `T: SignedTransaction` — upstream just chose to pin the
//!   outer bound for OpNode's needs).
//!
//! Everything else (payload attributes, execution data, validator)
//! is reused as-is from `reth-optimism-payload-builder`.
//!
//! [`OpPayloadTypes`]: reth_optimism_payload_builder::OpPayloadTypes
//! [`OpEngineTypes`]: reth_optimism_node::OpEngineTypes
//! [`EngineTypes`]: reth_node_api::EngineTypes
//! [`XLayerBlock`]: crate::primitives::XLayerBlock
//! [`OpBlock`]: reth_optimism_primitives::OpBlock

use alloy_rpc_types_engine::{ExecutionPayloadEnvelopeV2, ExecutionPayloadV1};
use op_alloy_rpc_types_engine::{
    OpExecutionData, OpExecutionPayloadEnvelopeV3, OpExecutionPayloadEnvelopeV4,
};
use reth_node_api::{
    payload::{BuiltPayload, PayloadTypes},
    EngineTypes, NodePrimitives,
};
use reth_optimism_payload_builder::OpPayloadTypes;
use reth_primitives_traits::SealedBlock;
use std::marker::PhantomData;

use crate::primitives::XLayerPrimitives;

/// Payload-types ZST for the XLayer node.
///
/// Alias over [`OpPayloadTypes<XLayerPrimitives>`]. The upstream
/// type is already generic over `N: NodePrimitives` — the only
/// reason XLayer can't use the default `OpPayloadTypes` is that
/// the default parameterizes on `OpPrimitives`, whose `SignedTx =
/// OpTxEnvelope` doesn't decode 0x7B. Swapping `N` to
/// `XLayerPrimitives` fixes it at the alias level.
pub type XLayerPayloadTypes = OpPayloadTypes<XLayerPrimitives>;

/// Engine-types ZST for the XLayer node.
///
/// Structural mirror of [`reth_optimism_node::OpEngineTypes`] with
/// the `Block = OpBlock` bound dropped from the [`EngineTypes`]
/// impl. Everything else — the PayloadTypes projection, the
/// execution-envelope aliases, `block_to_payload` — is byte-for-byte
/// the same.
#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
#[non_exhaustive]
pub struct XLayerEngineTypes<T: PayloadTypes = XLayerPayloadTypes> {
    _marker: PhantomData<T>,
}

impl<T: PayloadTypes<ExecutionData = OpExecutionData>> PayloadTypes for XLayerEngineTypes<T> {
    type ExecutionData = T::ExecutionData;
    type BuiltPayload = T::BuiltPayload;
    type PayloadAttributes = T::PayloadAttributes;
    type PayloadBuilderAttributes = T::PayloadBuilderAttributes;

    fn block_to_payload(
        block: SealedBlock<
            <<Self::BuiltPayload as BuiltPayload>::Primitives as NodePrimitives>::Block,
        >,
    ) -> <T as PayloadTypes>::ExecutionData {
        // `OpExecutionData::from_block_unchecked` is generic on
        // `T: Encodable2718 + Transaction`, both of which
        // `XLayerTxEnvelope` satisfies. `into_ethereum_block()` on
        // `Block<T>` is a no-op identity conversion (returns Self).
        OpExecutionData::from_block_unchecked(
            block.hash(),
            &reth_primitives_traits::Block::into_ethereum_block(block.into_block()),
        )
    }
}

impl<T: PayloadTypes<ExecutionData = OpExecutionData>> EngineTypes for XLayerEngineTypes<T>
where
    T::BuiltPayload: BuiltPayload
        + TryInto<ExecutionPayloadV1>
        + TryInto<ExecutionPayloadEnvelopeV2>
        + TryInto<OpExecutionPayloadEnvelopeV3>
        + TryInto<OpExecutionPayloadEnvelopeV4>,
{
    // Engine-API payload envelopes are shape-compatible with the OP
    // variants; XLayerAA's AA-specific fields are carried in the
    // receipt (via the `xlayer-rpc-types` extension), not the
    // engine-API envelope.
    type ExecutionPayloadEnvelopeV1 = ExecutionPayloadV1;
    type ExecutionPayloadEnvelopeV2 = ExecutionPayloadEnvelopeV2;
    type ExecutionPayloadEnvelopeV3 = OpExecutionPayloadEnvelopeV3;
    type ExecutionPayloadEnvelopeV4 = OpExecutionPayloadEnvelopeV4;
    type ExecutionPayloadEnvelopeV5 = OpExecutionPayloadEnvelopeV4;
    type ExecutionPayloadEnvelopeV6 = OpExecutionPayloadEnvelopeV4;
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_node_api::{EngineTypes, PayloadTypes};

    /// Compile-gate: `XLayerPayloadTypes` satisfies
    /// [`PayloadTypes`] with our primitives plugged in. If the
    /// blanket on `OpBuiltPayload<N>: BuiltPayload` ever requires
    /// more than `N: NodePrimitives`, this fails at compile time.
    #[test]
    fn xlayer_payload_types_satisfies_payload_types() {
        fn assert_pt<T: PayloadTypes>() {}
        assert_pt::<XLayerPayloadTypes>();
    }

    /// Compile-gate: `XLayerEngineTypes` with its default
    /// `XLayerPayloadTypes` parameter satisfies [`EngineTypes`].
    /// Drives the whole point of the fork — if this fails it means
    /// the TryInto bounds aren't met for `OpBuiltPayload<XLayerPrimitives>`,
    /// which would indicate a silent regression in the upstream
    /// `From<OpBuiltPayload<N>>` impls' generic bounds.
    #[test]
    fn xlayer_engine_types_satisfies_engine_types() {
        fn assert_et<T: EngineTypes>() {}
        assert_et::<XLayerEngineTypes>();
    }
}
