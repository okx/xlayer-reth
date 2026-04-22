//! Pooled-transaction envelope: the subset of [`XLayerTxEnvelope`]
//! that can appear in the mempool and on the P2P wire.
//!
//! # Pooled vs consensus
//!
//! The distinction mirrors upstream op-alloy: [`OpTxEnvelope`]
//! (consensus) is a superset of [`op_alloy_consensus::OpPooledTransaction`]
//! (pooled) — consensus carries Deposit (`0x7E`), which is
//! sequencer-injected and never user-submitted, whereas pooled is
//! exactly the user-submittable subset.
//!
//! XLayerAA (`0x7B`) *is* user-submittable, so it lives on both sides.
//! That makes the relationship:
//!
//! - consensus: `Extended<OpTxEnvelope, XLayerAAEnvelope>`
//!   (`XLayerTxEnvelope`)
//! - pooled:    `Extended<OpPooledTransaction, XLayerAAEnvelope>`
//!   (`XLayerPooledTxEnvelope`, this module)
//!
//! The `BuiltIn` arm differs (`OpPooledTransaction` strips deposits);
//! the `Other` arm is unchanged.
//!
//! # Why a local newtype (again)
//!
//! Same orphan-rule reason as `XLayerTxEnvelope`: reth's
//! `PoolTransaction` trait requires `Consensus: From<Pooled>` and
//! `Pooled: TryFrom<Consensus>`. We need both `Extended<OpPooledTx,
//! AA>` ↔ `Extended<OpTxEnvelope, AA>` conversions. Neither
//! `Extended` nor its inner variants is `#[fundamental]`, so
//! conversions between two generic `Extended` layerings can't be
//! impl'd from this crate on bare upstream types. Wrapping each in
//! a local newtype (`XLayerTxEnvelope` for consensus,
//! `XLayerPooledTxEnvelope` for pooled) satisfies coherence.
//!
//! [`OpTxEnvelope`]: op_alloy_consensus::OpTxEnvelope

use alloy_consensus::{
    crypto::RecoveryError,
    transaction::{SignerRecoverable, Transaction, TxHashRef},
    Extended,
};
use alloy_eips::{
    eip2718::{Decodable2718, Eip2718Result, Encodable2718, IsTyped2718, Typed2718},
    eip2930::AccessList,
    eip7702::SignedAuthorization,
};
use alloy_primitives::{Address, Bytes, ChainId, TxHash, TxKind, B256, U256};
use alloy_rlp::{BufMut, Decodable, Encodable};
use op_alloy_consensus::{OpPooledTransaction, OpTxEnvelope};
use reth_primitives_traits::InMemorySize;

use crate::{envelope::XLayerAAEnvelope, TxEip8130, XLayerTxEnvelope};

/// Pooled / P2P-wire subset of [`XLayerTxEnvelope`].
///
/// Accepts every type byte that a user is allowed to submit:
/// `0x00` / `0x01` / `0x02` / `0x04` (standard eth variants via
/// [`OpPooledTransaction`]) and `0x7B` (XLayerAA via
/// [`XLayerAAEnvelope`]). Rejects `0x7E` (Deposit) at the
/// [`TryFrom<XLayerTxEnvelope>`] boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct XLayerPooledTxEnvelope(pub Extended<OpPooledTransaction, XLayerAAEnvelope>);

impl XLayerPooledTxEnvelope {
    /// Wrap a built-in pooled OP transaction (non-deposit).
    pub const fn builtin(tx: OpPooledTransaction) -> Self {
        Self(Extended::BuiltIn(tx))
    }

    /// Wrap an XLayerAA (`0x7B`) transaction, sealing it in the process.
    pub fn aa(tx: TxEip8130) -> Self {
        Self(Extended::Other(XLayerAAEnvelope::new(tx)))
    }

    /// Borrow the inner [`Extended`] for exhaustive match-dispatch.
    pub const fn as_extended(&self) -> &Extended<OpPooledTransaction, XLayerAAEnvelope> {
        &self.0
    }

    /// Consume and return the inner [`Extended`].
    pub fn into_extended(self) -> Extended<OpPooledTransaction, XLayerAAEnvelope> {
        self.0
    }

    /// If this envelope carries an XLayerAA tx, return the inner
    /// wire body.
    pub fn as_aa(&self) -> Option<&TxEip8130> {
        match &self.0 {
            Extended::Other(aa) => Some(aa.tx()),
            Extended::BuiltIn(_) => None,
        }
    }
}

// --- Conversions between pooled and consensus ---------------------
//
// These two impls are the whole reason `XLayerPooledTxEnvelope`
// exists as a distinct type: reth's `PoolTransaction` trait requires
// them to reason about pool membership vs on-chain membership.

/// Pool → consensus: always succeeds. An OpPooledTransaction maps
/// to the corresponding non-Deposit variant of OpTxEnvelope; AA
/// stays AA.
impl From<XLayerPooledTxEnvelope> for XLayerTxEnvelope {
    fn from(tx: XLayerPooledTxEnvelope) -> Self {
        match tx.0 {
            Extended::BuiltIn(p) => Self::builtin(OpTxEnvelope::from(p)),
            Extended::Other(aa) => Self(Extended::Other(aa)),
        }
    }
}

/// Consensus → pool: fails for Deposit (`0x7E`) because deposits
/// are sequencer-injected, not user-submittable. All other
/// variants (0x00 / 0x01 / 0x02 / 0x04 / 0x7B) pass through.
///
/// The `Error` type is the rejected consensus envelope itself,
/// following the same shape as op-alloy's `TryFrom<OpTxEnvelope>
/// for OpPooledTransaction`. Returning the owned value (not just a
/// "reason" string) lets the caller re-use it for a diagnostic
/// path without reconstructing.
impl TryFrom<XLayerTxEnvelope> for XLayerPooledTxEnvelope {
    type Error = XLayerTxEnvelope;

    fn try_from(tx: XLayerTxEnvelope) -> Result<Self, Self::Error> {
        match tx.0 {
            Extended::BuiltIn(op_env) => match OpPooledTransaction::try_from(op_env) {
                Ok(pooled) => Ok(Self::builtin(pooled)),
                // OpPooledTransaction's TryFrom returns the
                // rejected OpTxEnvelope; re-wrap it as XLayerTxEnvelope
                // so the caller's error surface is uniform.
                Err(rejected) => Err(XLayerTxEnvelope::builtin(rejected.into_value())),
            },
            Extended::Other(aa) => Ok(Self(Extended::Other(aa))),
        }
    }
}

// --- Trait forwarding --------------------------------------------
//
// Same boilerplate as XLayerTxEnvelope. The inner `Extended<B, T>`
// already has blanket impls for everything we need via alloy /
// alloy-eips / alloy-rlp / op-alloy; we just forward through the
// newtype.

impl Typed2718 for XLayerPooledTxEnvelope {
    fn ty(&self) -> u8 {
        self.0.ty()
    }
}

impl IsTyped2718 for XLayerPooledTxEnvelope {
    fn is_type(type_id: u8) -> bool {
        <Extended<OpPooledTransaction, XLayerAAEnvelope> as IsTyped2718>::is_type(type_id)
    }
}

impl Encodable2718 for XLayerPooledTxEnvelope {
    fn type_flag(&self) -> Option<u8> {
        self.0.type_flag()
    }
    fn encode_2718_len(&self) -> usize {
        self.0.encode_2718_len()
    }
    fn encode_2718(&self, out: &mut dyn BufMut) {
        self.0.encode_2718(out)
    }
}

impl Decodable2718 for XLayerPooledTxEnvelope {
    fn typed_decode(ty: u8, buf: &mut &[u8]) -> Eip2718Result<Self> {
        <Extended<OpPooledTransaction, XLayerAAEnvelope> as Decodable2718>::typed_decode(ty, buf)
            .map(Self)
    }
    fn fallback_decode(buf: &mut &[u8]) -> Eip2718Result<Self> {
        <Extended<OpPooledTransaction, XLayerAAEnvelope> as Decodable2718>::fallback_decode(buf)
            .map(Self)
    }
}

impl Encodable for XLayerPooledTxEnvelope {
    fn encode(&self, out: &mut dyn BufMut) {
        self.0.encode(out)
    }
    fn length(&self) -> usize {
        self.0.length()
    }
}

impl Decodable for XLayerPooledTxEnvelope {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        <Extended<OpPooledTransaction, XLayerAAEnvelope> as Decodable>::decode(buf).map(Self)
    }
}

impl Transaction for XLayerPooledTxEnvelope {
    fn chain_id(&self) -> Option<ChainId> {
        self.0.chain_id()
    }
    fn nonce(&self) -> u64 {
        self.0.nonce()
    }
    fn gas_limit(&self) -> u64 {
        self.0.gas_limit()
    }
    fn gas_price(&self) -> Option<u128> {
        self.0.gas_price()
    }
    fn max_fee_per_gas(&self) -> u128 {
        self.0.max_fee_per_gas()
    }
    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.0.max_priority_fee_per_gas()
    }
    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        self.0.max_fee_per_blob_gas()
    }
    fn priority_fee_or_price(&self) -> u128 {
        self.0.priority_fee_or_price()
    }
    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        self.0.effective_gas_price(base_fee)
    }
    fn is_dynamic_fee(&self) -> bool {
        self.0.is_dynamic_fee()
    }
    fn kind(&self) -> TxKind {
        self.0.kind()
    }
    fn is_create(&self) -> bool {
        self.0.is_create()
    }
    fn value(&self) -> U256 {
        self.0.value()
    }
    fn input(&self) -> &Bytes {
        self.0.input()
    }
    fn access_list(&self) -> Option<&AccessList> {
        self.0.access_list()
    }
    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        self.0.blob_versioned_hashes()
    }
    fn authorization_list(&self) -> Option<&[SignedAuthorization]> {
        self.0.authorization_list()
    }
    fn effective_tip_per_gas(&self, base_fee: u64) -> Option<u128> {
        self.0.effective_tip_per_gas(base_fee)
    }
    fn to(&self) -> Option<Address> {
        self.0.to()
    }
}

impl InMemorySize for XLayerPooledTxEnvelope {
    fn size(&self) -> usize {
        match &self.0 {
            Extended::BuiltIn(p) => p.size(),
            Extended::Other(aa) => aa.size(),
        }
    }
}

impl TxHashRef for XLayerPooledTxEnvelope {
    fn tx_hash(&self) -> &TxHash {
        match &self.0 {
            // OpPooledTransaction's inherent `hash()` returns `&B256`
            // directly, which matches TxHashRef's return type.
            Extended::BuiltIn(p) => p.hash(),
            Extended::Other(aa) => aa.tx_hash(),
        }
    }
}

impl SignerRecoverable for XLayerPooledTxEnvelope {
    fn recover_signer(&self) -> Result<Address, RecoveryError> {
        match &self.0 {
            Extended::BuiltIn(p) => p.recover_signer(),
            Extended::Other(aa) => aa.recover_signer(),
        }
    }
    fn recover_signer_unchecked(&self) -> Result<Address, RecoveryError> {
        match &self.0 {
            Extended::BuiltIn(p) => p.recover_signer_unchecked(),
            Extended::Other(aa) => aa.recover_signer_unchecked(),
        }
    }
}

// --- SignedTransaction (workspace builds only) -------------------
//
// Gated on `serde` for the same reason `XLayerTxEnvelope`'s impl is:
// `MaybeSerde` degrades to `Serialize + Deserialize` whenever
// `reth-primitives-traits/serde` is on (every workspace node build).
#[cfg(feature = "serde")]
impl reth_primitives_traits::SignedTransaction for XLayerPooledTxEnvelope {
    fn is_system_tx(&self) -> bool {
        // Pooled is by definition user-submittable; OpPooledTransaction
        // doesn't carry deposits (the only OP system tx kind), so
        // both arms are always `false`. Kept as a match to make the
        // invariant explicit — if a future variant is a system tx,
        // the compile breaks on the missing arm.
        match &self.0 {
            Extended::BuiltIn(_) => false,
            Extended::Other(_) => false,
        }
    }

    fn is_broadcastable_in_full(&self) -> bool {
        // No blob txs reach the OP pool (OpPooledTransaction has no
        // Eip4844 variant); AA has no sidecar. Broadcastable in full.
        true
    }
}

// --- Bincode + Compact (workspace builds only) -------------------
//
// Same gating as the consensus-side envelope.

#[cfg(feature = "serde-bincode-compat")]
impl reth_primitives_traits::serde_bincode_compat::RlpBincode for XLayerPooledTxEnvelope {}

#[cfg(feature = "reth-codec")]
mod compact_impl {
    use super::{Decodable2718, Encodable2718, XLayerPooledTxEnvelope};
    use bytes::BufMut as BytesBufMut;
    use reth_codecs::Compact;
    use std::vec::Vec;

    impl Compact for XLayerPooledTxEnvelope {
        fn to_compact<B>(&self, buf: &mut B) -> usize
        where
            B: BytesBufMut + AsMut<[u8]>,
        {
            let mut scratch = Vec::with_capacity(self.encode_2718_len());
            self.encode_2718(&mut scratch);
            let written = scratch.len();
            buf.put_slice(&scratch);
            written
        }

        fn from_compact(buf: &[u8], len: usize) -> (Self, &[u8]) {
            let (head, tail) = buf.split_at(len);
            let mut cursor: &[u8] = head;
            let decoded = XLayerPooledTxEnvelope::decode_2718(&mut cursor)
                .expect("Compact stream was written by `to_compact` above");
            (decoded, tail)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aa::{Call, AA_TX_TYPE_ID};
    use alloy_consensus::{SignableTransaction, Signed, TxEip1559};
    use alloy_primitives::{Address, Bytes, Signature, TxKind, U256};

    fn sample_aa_tx() -> TxEip8130 {
        TxEip8130 {
            chain_id: 196,
            from: Some(Address::repeat_byte(0x01)),
            nonce_key: U256::from(0u64),
            nonce_sequence: 1,
            expiry: 0,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 10_000_000_000,
            gas_limit: 100_000,
            account_changes: Vec::new(),
            calls: std::vec![std::vec![Call {
                to: Address::repeat_byte(0xBB),
                data: Bytes::from_static(&[0xDE, 0xAD]),
            }]],
            payer: None,
            sender_auth: Bytes::from_static(&[0xFF; 65]),
            payer_auth: Bytes::new(),
        }
    }

    fn sample_eip1559() -> Signed<TxEip1559> {
        let tx = TxEip1559 {
            chain_id: 196,
            nonce: 3,
            gas_limit: 21_000,
            max_fee_per_gas: 2_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: TxKind::Call(Address::repeat_byte(0xAA)),
            value: U256::from(1u64),
            access_list: Default::default(),
            input: Bytes::new(),
        };
        let sig = Signature::from_scalars_and_parity(Default::default(), Default::default(), false);
        tx.into_signed(sig)
    }

    /// EIP-2718 round-trip: encode a pooled AA envelope, decode it
    /// back, re-encode. Byte-equal guarantees the type-byte routing
    /// doesn't silently flip.
    #[test]
    fn aa_pooled_round_trip() {
        let env = XLayerPooledTxEnvelope::aa(sample_aa_tx());
        let mut buf = Vec::new();
        env.encode_2718(&mut buf);
        assert_eq!(buf[0], AA_TX_TYPE_ID);
        let decoded = XLayerPooledTxEnvelope::decode_2718(&mut buf.as_slice()).unwrap();
        let mut buf2 = Vec::new();
        decoded.encode_2718(&mut buf2);
        assert_eq!(buf, buf2);
    }

    /// Consensus → pool for a standard OP tx: always succeeds.
    #[test]
    fn consensus_to_pool_builtin_roundtrips() {
        let consensus = XLayerTxEnvelope::builtin(OpTxEnvelope::Eip1559(sample_eip1559()));
        let pooled: XLayerPooledTxEnvelope = consensus.try_into().expect("non-deposit is poolable");
        let back: XLayerTxEnvelope = pooled.into();
        // Round-trip re-emerges as the same consensus variant.
        matches!(back.as_extended(), Extended::BuiltIn(OpTxEnvelope::Eip1559(_)));
    }

    /// Consensus → pool for an AA tx: succeeds unchanged.
    #[test]
    fn consensus_to_pool_aa_succeeds() {
        let consensus = XLayerTxEnvelope::aa(sample_aa_tx());
        let pooled: XLayerPooledTxEnvelope = consensus.try_into().expect("AA must be poolable");
        assert!(matches!(pooled.0, Extended::Other(_)));
    }

    /// Consensus → pool for a Deposit (0x7E): must fail with the
    /// rejected envelope returned for diagnostic use.
    #[test]
    fn consensus_to_pool_deposit_rejects() {
        use alloy_consensus::Sealed;
        use op_alloy_consensus::TxDeposit;
        let deposit = TxDeposit {
            source_hash: Default::default(),
            from: Address::repeat_byte(0xCC),
            to: TxKind::Call(Address::repeat_byte(0xDD)),
            mint: 0u128,
            value: U256::ZERO,
            gas_limit: 100_000,
            is_system_transaction: false,
            input: Bytes::new(),
        };
        let consensus = XLayerTxEnvelope::builtin(OpTxEnvelope::Deposit(Sealed::new(deposit)));
        let err = XLayerPooledTxEnvelope::try_from(consensus).unwrap_err();
        // The rejection must return the original deposit so the
        // caller can surface it in an RPC error message.
        assert!(matches!(err.as_extended(), Extended::BuiltIn(OpTxEnvelope::Deposit(_))));
    }

    /// `is_system_tx` on a pooled envelope is always false — pooled
    /// is user-submittable by definition. Regression guard against a
    /// future `OpPooledTransaction` variant sneaking a system-type in.
    #[cfg(feature = "serde")]
    #[test]
    fn pooled_is_never_system_tx() {
        use reth_primitives_traits::SignedTransaction;
        let aa = XLayerPooledTxEnvelope::aa(sample_aa_tx());
        assert!(!aa.is_system_tx());
        let eip1559 =
            XLayerPooledTxEnvelope::builtin(OpPooledTransaction::Eip1559(sample_eip1559()));
        assert!(!eip1559.is_system_tx());
    }
}
