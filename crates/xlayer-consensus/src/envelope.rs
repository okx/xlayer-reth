//! Wire-level envelope that carries either a standard OP transaction
//! ([`OpTxEnvelope`]) or an XLayerAA ([`TxEip8130`]) transaction.
//!
//! # Why [`Extended<OpTxEnvelope, XLayerAAEnvelope>`] and not a
//! patched `OpTxEnvelope::Eip8130` variant
//!
//! The original plan called for patching the [`OpTxEnvelope`] enum
//! directly in the `op-alloy` submodule — adding an `Eip8130` variant
//! and cascading through [`OpTxType`], [`OpTypedTransaction`],
//! [`OpPooledTransaction`], every match arm, etc. That turns out to
//! be ~10 files' worth of submodule patches with cascading codec
//! work in `op-reth` primitives and non-trivial rustc-ICE risk
//! (shared with the earlier `OpHardforks` default-method misadventure
//! — see `docs/xlayer-aa.md`).
//!
//! alloy provides a first-class mechanism for this exact scenario:
//! [`alloy_consensus::Extended<BuiltIn, Other>`]. Its own crate doc
//! calls it out explicitly:
//!
//! > This is intended to be used to extend existing presets, for
//! > example the ethereum or opstack transaction types and receipts.
//!
//! So we use it. `BuiltIn = OpTxEnvelope`, `Other = XLayerAAEnvelope`.
//! Dispatch on the EIP-2718 type byte is automatic: if the byte
//! matches an `OpTxType` variant (0x00 / 0x01 / 0x02 / 0x04 / 0x7e),
//! decode routes to `BuiltIn`; otherwise to `Other` where
//! [`TxEip8130::typed_decode`] accepts 0x7B. Encode just forwards
//! whichever variant is held.
//!
//! ## Why the newtype wrapper [`XLayerAAEnvelope`]
//!
//! [`Extended`]'s blanket `OpTransaction` impl requires `T: OpTransaction`.
//! The orphan rule forbids us from impl'ing a foreign trait
//! ([`OpTransaction`]) on a foreign type ([`Sealed<T>`]) even with a
//! local generic, so [`Sealed<TxEip8130>`] can't satisfy [`OpTransaction`]
//! directly. We wrap it in a local newtype — [`XLayerAAEnvelope`] —
//! and forward all of `Sealed<TxEip8130>`'s trait surface through the
//! newtype while providing a deposit-free [`OpTransaction`] impl.
//!
//! [`OpTxType`]: op_alloy_consensus::OpTxType
//! [`OpTypedTransaction`]: op_alloy_consensus::OpTypedTransaction
//! [`OpPooledTransaction`]: op_alloy_consensus::OpPooledTransaction
//! [`OpTransaction`]: op_alloy_consensus::OpTransaction

use core::ops::Deref;

use alloy_consensus::{transaction::Transaction, Extended, Sealable, Sealed};
use alloy_eips::eip2718::{Decodable2718, Eip2718Result, Encodable2718, IsTyped2718, Typed2718};
use alloy_primitives::{Address, Bytes, ChainId, TxKind, B256, U256};
use alloy_rlp::{BufMut, Decodable, Encodable};
use op_alloy_consensus::{OpTransaction, OpTxEnvelope, TxDeposit};

use crate::aa::TxEip8130;

/// Newtype wrapper over [`Sealed<TxEip8130>`] that lets us provide an
/// [`OpTransaction`] impl (orphan rule blocks impl on bare
/// `Sealed<TxEip8130>`).
///
/// All other traits on [`Sealed<TxEip8130>`] — [`Transaction`],
/// [`Typed2718`], [`Encodable2718`], etc. — are forwarded to the
/// inner through a hand-written delegation below (derive-forwarding
/// crates like `derive_more`'s `Deref` alone aren't enough because
/// most trait methods take `&self` and return concrete types the
/// compiler can't thread through a generic blanket).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct XLayerAAEnvelope(pub Sealed<TxEip8130>);

impl XLayerAAEnvelope {
    /// Seal an XLayerAA tx (computing its hash) and wrap it.
    pub fn new(tx: TxEip8130) -> Self {
        Self(tx.seal_slow())
    }

    /// Borrow the inner [`Sealed`] wrapper.
    pub const fn as_sealed(&self) -> &Sealed<TxEip8130> {
        &self.0
    }

    /// Consume the wrapper and return the inner [`Sealed`].
    pub fn into_sealed(self) -> Sealed<TxEip8130> {
        self.0
    }

    /// Borrow the inner [`TxEip8130`].
    pub fn tx(&self) -> &TxEip8130 {
        self.0.inner()
    }

    /// Pre-computed hash of the inner tx.
    pub fn hash(&self) -> B256 {
        self.0.hash()
    }
}

impl Deref for XLayerAAEnvelope {
    type Target = Sealed<TxEip8130>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<TxEip8130> for XLayerAAEnvelope {
    fn from(tx: TxEip8130) -> Self {
        Self::new(tx)
    }
}

impl From<Sealed<TxEip8130>> for XLayerAAEnvelope {
    fn from(sealed: Sealed<TxEip8130>) -> Self {
        Self(sealed)
    }
}

// --- Trait forwarding --------------------------------------------

impl OpTransaction for XLayerAAEnvelope {
    /// An XLayerAA tx is never a deposit (deposits own tx type `0x7E`
    /// with their own wire format and envelope variant).
    fn is_deposit(&self) -> bool {
        false
    }

    /// Always `None` for the same reason.
    fn as_deposit(&self) -> Option<&Sealed<TxDeposit>> {
        None
    }
}

impl Typed2718 for XLayerAAEnvelope {
    fn ty(&self) -> u8 {
        self.0.ty()
    }
}

impl IsTyped2718 for XLayerAAEnvelope {
    fn is_type(type_id: u8) -> bool {
        <TxEip8130 as IsTyped2718>::is_type(type_id)
    }
}

impl Encodable2718 for XLayerAAEnvelope {
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

impl Decodable2718 for XLayerAAEnvelope {
    fn typed_decode(ty: u8, buf: &mut &[u8]) -> Eip2718Result<Self> {
        Sealed::<TxEip8130>::typed_decode(ty, buf).map(Self)
    }

    fn fallback_decode(buf: &mut &[u8]) -> Eip2718Result<Self> {
        Sealed::<TxEip8130>::fallback_decode(buf).map(Self)
    }
}

// `Sealed<T>` doesn't implement `Encodable` / `Decodable` (only their
// 2718-typed counterparts). Delegate through the inner [`TxEip8130`]
// — its own RLP encoding is the body of the sealed envelope, and
// decoding reconstructs the seal hash via [`Sealable::seal_slow`].
impl Encodable for XLayerAAEnvelope {
    fn encode(&self, out: &mut dyn BufMut) {
        self.0.inner().encode(out)
    }

    fn length(&self) -> usize {
        self.0.inner().length()
    }
}

impl Decodable for XLayerAAEnvelope {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let tx = TxEip8130::decode(buf)?;
        Ok(Self::new(tx))
    }
}

impl Transaction for XLayerAAEnvelope {
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
    fn access_list(&self) -> Option<&alloy_eips::eip2930::AccessList> {
        self.0.access_list()
    }
    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        self.0.blob_versioned_hashes()
    }
    fn authorization_list(&self) -> Option<&[alloy_eips::eip7702::SignedAuthorization]> {
        self.0.authorization_list()
    }
    fn effective_tip_per_gas(&self, base_fee: u64) -> Option<u128> {
        self.0.effective_tip_per_gas(base_fee)
    }
    fn to(&self) -> Option<Address> {
        self.0.to()
    }
}

// --- The envelope type -------------------------------------------

/// Combined XLayer transaction envelope.
///
/// [`Extended::BuiltIn`] carries any standard OP transaction shape
/// (Legacy / 2930 / 1559 / 7702 / Deposit); [`Extended::Other`]
/// carries [`XLayerAAEnvelope`]. Decode from raw EIP-2718 bytes
/// dispatches on the type byte automatically — see the module-level
/// doc for the mechanism.
pub type XLayerTxEnvelope = Extended<OpTxEnvelope, XLayerAAEnvelope>;

#[cfg(test)]
mod tests {
    use alloy_consensus::{SignableTransaction, Signed, TxEip1559};
    use alloy_primitives::{Address, Bytes, Signature, TxKind, U256};

    use super::*;
    use crate::aa::{Call, AA_TX_TYPE_ID};
    use std::vec::Vec;

    fn sample_aa_tx() -> TxEip8130 {
        TxEip8130 {
            chain_id: 196,
            from: Some(Address::repeat_byte(0x01)),
            nonce_key: U256::from(0u64),
            nonce_sequence: 42,
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
            nonce: 7,
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

    /// The 0x7B type byte must route to [`Extended::Other`], not
    /// [`Extended::BuiltIn`]. If [`OpTxType`] ever adds 0x7B as a
    /// variant, this dispatcher silently flips and the test catches
    /// it.
    #[test]
    fn raw_0x7b_decodes_as_other() {
        let aa_tx = sample_aa_tx();
        let mut buf = Vec::new();
        aa_tx.encode_2718(&mut buf);
        assert_eq!(buf[0], AA_TX_TYPE_ID);

        let decoded = XLayerTxEnvelope::decode_2718(&mut buf.as_slice()).unwrap();
        match decoded {
            Extended::Other(env) => {
                assert_eq!(env.tx(), &aa_tx);
                assert_eq!(env.ty(), AA_TX_TYPE_ID);
            }
            Extended::BuiltIn(inner) => {
                panic!("0x7B routed to BuiltIn as {:?}; must route to Other", inner)
            }
        }
    }

    /// Non-AA tx type bytes (0x02 here) must route to
    /// [`Extended::BuiltIn`].
    #[test]
    fn standard_eip1559_decodes_as_builtin() {
        let tx = sample_eip1559();
        let mut buf = Vec::new();
        tx.encode_2718(&mut buf);
        assert_eq!(buf[0], 0x02);

        let decoded = XLayerTxEnvelope::decode_2718(&mut buf.as_slice()).unwrap();
        match decoded {
            Extended::BuiltIn(OpTxEnvelope::Eip1559(inner)) => {
                assert_eq!(inner.tx().nonce, 7);
            }
            other => panic!("0x02 did not route to BuiltIn::Eip1559: {:?}", other),
        }
    }

    /// Round-trip: encode, decode, assert byte-equal re-encode.
    /// Catches any asymmetry between the Extended forward and
    /// reverse paths.
    #[test]
    fn envelope_encode_decode_is_stable() {
        let env: XLayerTxEnvelope = Extended::Other(XLayerAAEnvelope::new(sample_aa_tx()));

        let mut buf = Vec::new();
        env.encode_2718(&mut buf);

        let roundtrip = XLayerTxEnvelope::decode_2718(&mut buf.as_slice()).unwrap();
        let mut buf2 = Vec::new();
        roundtrip.encode_2718(&mut buf2);
        assert_eq!(buf, buf2);
    }

    /// [`OpTransaction::is_deposit`] / [`OpTransaction::as_deposit`]
    /// on [`XLayerAAEnvelope`] always return the deposit-free
    /// defaults. Guards against an accidental "route AA through
    /// deposit fast paths" regression in the impl above.
    #[test]
    fn xlayer_aa_envelope_is_not_deposit() {
        let env = XLayerAAEnvelope::new(sample_aa_tx());
        assert!(!env.is_deposit());
        assert!(env.as_deposit().is_none());
    }

    /// [`Extended`]'s combined [`OpTransaction`] impl relies on
    /// both arms satisfying the trait. Confirm it routes through
    /// to our newtype without shadowing the built-in path.
    #[test]
    fn extended_delegates_is_deposit_both_sides() {
        let aa: XLayerTxEnvelope = Extended::Other(XLayerAAEnvelope::new(sample_aa_tx()));
        assert!(!aa.is_deposit());

        let eip1559: XLayerTxEnvelope = Extended::BuiltIn(OpTxEnvelope::Eip1559(sample_eip1559()));
        assert!(!eip1559.is_deposit());
    }
}
