//! Node-level primitives for XLayer.
//!
//! Mirrors [`reth_optimism_primitives::OpPrimitives`] with one
//! substitution: the signed-transaction slot carries
//! [`XLayerTxEnvelope`] instead of [`OpTxEnvelope`], letting the
//! XLayerAA (0x7B) variant flow through every generic that's
//! parameterised on `NodePrimitives::SignedTx` — pool, payload
//! builder, executor, RPC — without per-crate overrides.
//!
//! This is a pure type alias / struct; all heavy lifting (wire
//! encoding, signer recovery, in-memory size, Compact/MDBX storage)
//! lives on [`XLayerTxEnvelope`] itself in the `xlayer-consensus` crate.

use reth_primitives_traits::NodePrimitives;
use xlayer_consensus::XLayerTxEnvelope;

/// Block type parameterised on [`XLayerTxEnvelope`].
///
/// Structurally identical to [`reth_optimism_primitives::OpBlock`]
/// except for the transaction variant; the header and body shapes
/// are unchanged so existing consensus / engine plumbing keeps
/// working byte-for-byte.
pub type XLayerBlock = alloy_consensus::Block<XLayerTxEnvelope>;

/// Block body for [`XLayerBlock`].
pub type XLayerBlockBody = <XLayerBlock as reth_primitives_traits::Block>::Body;

/// Primitive-type container for the XLayer node.
// `NodePrimitives` itself requires only `Default + Debug + Clone +
// PartialEq + Eq`; no serde bound. xlayer-builder doesn't expose a
// `serde` feature, so keep the derives unconditional and minimal.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct XLayerPrimitives;

impl NodePrimitives for XLayerPrimitives {
    type Block = XLayerBlock;
    type BlockHeader = alloy_consensus::Header;
    type BlockBody = XLayerBlockBody;
    type SignedTx = XLayerTxEnvelope;
    // Receipts stay on the op-alloy type — XLayerAA's phase-status
    // logs are emitted via events at `TX_CONTEXT_ADDRESS`, not via
    // a new receipt variant, so `OpReceipt` needs no change at this
    // layer. Revisit if/when post-M6 RPC work wants a typed
    // receipt extension.
    type Receipt = op_alloy_consensus::OpReceipt;
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_primitives_traits::FullSignedTx;

    /// Compile-gate: the signed-tx slot actually satisfies
    /// `FullSignedTx`. If a required super-trait
    /// (`SignedTransaction`, `MaybeCompact`, `MaybeSerdeBincodeCompat`)
    /// regresses on [`XLayerTxEnvelope`], this test fails at
    /// compile time with a focused diagnostic rather than through a
    /// `NodePrimitives` impl failure three crates down.
    #[test]
    fn signed_tx_satisfies_full_signed_tx() {
        fn assert_full<T: FullSignedTx>() {}
        assert_full::<XLayerTxEnvelope>();
    }

    /// Compile-gate: `XLayerPrimitives` itself satisfies
    /// [`NodePrimitives`], the bound reth's `NodeTypes` requires.
    #[test]
    fn xlayer_primitives_satisfies_node_primitives() {
        fn assert_np<T: NodePrimitives>() {}
        assert_np::<XLayerPrimitives>();
    }
}
