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

use core::{mem::size_of_val, ops::Deref};

use alloy_consensus::{
    crypto::RecoveryError,
    transaction::{SignerRecoverable, Transaction, TxHashRef},
    Extended, Sealable, Sealed,
};
use alloy_eips::eip2718::{Decodable2718, Eip2718Result, Encodable2718, IsTyped2718, Typed2718};
use alloy_evm::{FromRecoveredTx, FromTxWithEncoded};
use alloy_primitives::{Address, Bytes, ChainId, TxHash, TxKind, B256, U256};
use alloy_rlp::{BufMut, Decodable, Encodable};
use op_alloy_consensus::{OpTransaction, OpTxEnvelope, TxDeposit};
use op_revm::OpSpecId;
use reth_primitives_traits::InMemorySize;
#[cfg(feature = "serde")]
use reth_primitives_traits::SignedTransaction;
use revm::context::TxEnv as RevmTxEnv;
use xlayer_revm::tx_env::XLayerAATransaction;

use crate::aa::{
    build_aa_parts, k1_recover, parse_sender_auth, sender_signature_hash, ParsedSenderAuth,
    TxEip8130,
};

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
/// Wraps [`Extended<OpTxEnvelope, XLayerAAEnvelope>`] in a local
/// newtype so crate-external trait impls (like
/// [`FromRecoveredTx<XLayerTxEnvelope> for XLayerAATransaction`]
/// below) satisfy the orphan rule — `Extended` itself is foreign and
/// not `#[fundamental]`, so an `impl<…> ForeignTrait<Extended<…>>
/// for ForeignType` is rejected by the coherence checker, even when
/// `Extended`'s type arguments are local.
///
/// Decode from raw EIP-2718 bytes dispatches on the type byte
/// automatically — see the module-level doc for the mechanism.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct XLayerTxEnvelope(pub Extended<OpTxEnvelope, XLayerAAEnvelope>);

impl XLayerTxEnvelope {
    /// Wrap a standard OP envelope.
    pub const fn builtin(tx: OpTxEnvelope) -> Self {
        Self(Extended::BuiltIn(tx))
    }

    /// Wrap an XLayerAA (0x7B) transaction, sealing it along the way.
    pub fn aa(tx: TxEip8130) -> Self {
        Self(Extended::Other(XLayerAAEnvelope::new(tx)))
    }

    /// Borrow the inner [`Extended`]. Useful for match-dispatch in
    /// downstream callers that want to handle the two arms directly.
    pub const fn as_extended(&self) -> &Extended<OpTxEnvelope, XLayerAAEnvelope> {
        &self.0
    }

    /// Consume the wrapper and return the inner [`Extended`].
    pub fn into_extended(self) -> Extended<OpTxEnvelope, XLayerAAEnvelope> {
        self.0
    }

    /// If this envelope carries an XLayerAA (0x7B) transaction,
    /// return a borrow on the inner wire body.
    pub fn as_aa(&self) -> Option<&TxEip8130> {
        match &self.0 {
            Extended::Other(aa) => Some(aa.tx()),
            Extended::BuiltIn(_) => None,
        }
    }

    /// Expose the pre-computed hash of the inner transaction.
    pub fn tx_hash(&self) -> B256 {
        match &self.0 {
            Extended::BuiltIn(env) => env.tx_hash(),
            Extended::Other(aa) => aa.hash(),
        }
    }
}

impl From<OpTxEnvelope> for XLayerTxEnvelope {
    fn from(tx: OpTxEnvelope) -> Self {
        Self::builtin(tx)
    }
}

impl From<Extended<OpTxEnvelope, XLayerAAEnvelope>> for XLayerTxEnvelope {
    fn from(inner: Extended<OpTxEnvelope, XLayerAAEnvelope>) -> Self {
        Self(inner)
    }
}

// --- Trait forwarding --------------------------------------------
//
// Every trait [`Extended<B, T>`] impls via blanket rules in
// alloy-consensus / alloy-eips / alloy-rlp / op-alloy-consensus is
// forwarded through here, delegating to the inner `Extended`. This
// is pure boilerplate: the newtype exists to satisfy the orphan
// rule on [`FromRecoveredTx`], not to add behaviour.

impl Typed2718 for XLayerTxEnvelope {
    fn ty(&self) -> u8 {
        self.0.ty()
    }
}

impl IsTyped2718 for XLayerTxEnvelope {
    fn is_type(type_id: u8) -> bool {
        <Extended<OpTxEnvelope, XLayerAAEnvelope> as IsTyped2718>::is_type(type_id)
    }
}

impl Encodable2718 for XLayerTxEnvelope {
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

impl Decodable2718 for XLayerTxEnvelope {
    fn typed_decode(ty: u8, buf: &mut &[u8]) -> Eip2718Result<Self> {
        <Extended<OpTxEnvelope, XLayerAAEnvelope> as Decodable2718>::typed_decode(ty, buf).map(Self)
    }

    fn fallback_decode(buf: &mut &[u8]) -> Eip2718Result<Self> {
        <Extended<OpTxEnvelope, XLayerAAEnvelope> as Decodable2718>::fallback_decode(buf).map(Self)
    }
}

impl Encodable for XLayerTxEnvelope {
    fn encode(&self, out: &mut dyn BufMut) {
        self.0.encode(out)
    }

    fn length(&self) -> usize {
        self.0.length()
    }
}

impl Decodable for XLayerTxEnvelope {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        <Extended<OpTxEnvelope, XLayerAAEnvelope> as Decodable>::decode(buf).map(Self)
    }
}

impl Transaction for XLayerTxEnvelope {
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

impl OpTransaction for XLayerTxEnvelope {
    fn is_deposit(&self) -> bool {
        OpTransaction::is_deposit(&self.0)
    }

    fn as_deposit(&self) -> Option<&Sealed<TxDeposit>> {
        OpTransaction::as_deposit(&self.0)
    }
}

// --- InMemorySize ------------------------------------------------
//
// reth uses this to estimate pool / payload buffer footprint. For
// the sealed AA body that's the fixed sealed-struct overhead plus
// the embedded tx's variable-sized fields (calls / account_changes
// / sender_auth / payer_auth). Approximated via
// `core::mem::size_of_val` — exact per-byte accuracy isn't required
// because reth's pool / payload eviction heuristics treat these as
// ballpark figures.

impl InMemorySize for XLayerAAEnvelope {
    fn size(&self) -> usize {
        let tx = self.tx();
        size_of_val(tx)
            + tx.sender_auth.len()
            + tx.payer_auth.len()
            + tx.calls
                .iter()
                .map(|phase| phase.iter().map(|c| c.data.len()).sum::<usize>())
                .sum::<usize>()
    }
}

impl InMemorySize for XLayerTxEnvelope {
    fn size(&self) -> usize {
        match &self.0 {
            Extended::BuiltIn(env) => env.size(),
            Extended::Other(aa) => aa.size(),
        }
    }
}

// --- TxHashRef ---------------------------------------------------
//
// The sealed AA body carries its hash pre-computed; expose it as a
// `&TxHash` (= `&B256`) so the pool / RPC don't recompute on every
// lookup. Standard op envelopes already have `TxHashRef` via a
// blanket impl on `EthereumTxEnvelope`-derived types.

impl TxHashRef for XLayerAAEnvelope {
    fn tx_hash(&self) -> &TxHash {
        self.0.hash_ref()
    }
}

impl TxHashRef for XLayerTxEnvelope {
    fn tx_hash(&self) -> &TxHash {
        match &self.0 {
            // `OpTxEnvelope::tx_hash` resolves to the inherent method
            // (returning `B256` by value) by default — force the
            // `TxHashRef` trait lookup so we get `&B256` back.
            Extended::BuiltIn(env) => <OpTxEnvelope as TxHashRef>::tx_hash(env),
            Extended::Other(aa) => aa.tx_hash(),
        }
    }
}

// --- SignerRecoverable -------------------------------------------
//
// For **configured-account** XLayerAA transactions (`tx.from.is_some()`)
// the sender is the declared `from` field — no ecrecover, no k1
// derivation. The `sender_auth` blob still matters for authorization
// at execution time (the handler validates it against the configured
// owner set), but the envelope-level "who is the sender" answer is
// just `from`.
//
// For **EOA-mode** transactions (`tx.from.is_none()`) we run the
// native k1 recovery on `sender_auth` (a 65-byte ECDSA signature over
// `sender_signature_hash(tx)`). Matches the same code path used by
// `xlayer-revm`'s handler — see
// [`crate::aa::native::k1_recover`].
//
// `recover_signer_unchecked` skips the EIP-2 low-s check. `k1_recover`
// routes through alloy-primitives' ecrecover which enforces low-s,
// so "unchecked" here degrades to the checked path — acceptable since
// EIP-8130 is a post-EIP-2 tx type and no pre-EIP-2 replay concern
// exists.
impl SignerRecoverable for XLayerAAEnvelope {
    fn recover_signer(&self) -> Result<Address, RecoveryError> {
        let tx = self.tx();
        if let Some(addr) = tx.from {
            return Ok(addr);
        }
        let ParsedSenderAuth::Eoa { signature } =
            parse_sender_auth(tx).map_err(|_| RecoveryError::new())?
        else {
            // Should be unreachable — configured-auth requires `from`.
            return Err(RecoveryError::new());
        };
        let hash = sender_signature_hash(tx);
        k1_recover(&hash, &signature).map(|r| r.address).map_err(|_| RecoveryError::new())
    }

    fn recover_signer_unchecked(&self) -> Result<Address, RecoveryError> {
        // EIP-8130 is post-EIP-2; no separate unchecked path.
        self.recover_signer()
    }
}

impl SignerRecoverable for XLayerTxEnvelope {
    fn recover_signer(&self) -> Result<Address, RecoveryError> {
        match &self.0 {
            Extended::BuiltIn(env) => env.recover_signer(),
            Extended::Other(aa) => aa.recover_signer(),
        }
    }

    fn recover_signer_unchecked(&self) -> Result<Address, RecoveryError> {
        match &self.0 {
            Extended::BuiltIn(env) => env.recover_signer_unchecked(),
            Extended::Other(aa) => aa.recover_signer_unchecked(),
        }
    }
}

// --- SignedTransaction -------------------------------------------
//
// The crown jewel of the P2b trait stack — this is the trait bar
// `NodePrimitives::SignedTx` ultimately checks via `FullSignedTx`.
// All super-trait requirements were populated in earlier steps:
//
// - `Clone + Debug + PartialEq + Eq + Hash` — derived on both types
// - `Send + Sync + Unpin` — auto-implemented for all of our fields
// - `Encodable + Decodable` — hand-written above
// - `Encodable2718 + Decodable2718` — hand-written above
// - `Transaction` — hand-written above
// - `MaybeSerde` — blanket on any `T` (non-serde feature) / driven by
//   our `serde` derive cfg-gate on the structs themselves
// - `InMemorySize` — P2b.2a
// - `SignerRecoverable` — P2b.2a
// - `TxHashRef` — P2b.2a
// - `IsTyped2718` — hand-written above
//
// With `reth-primitives-traits` built `default-features = false`
// (no `reth-codec`, no `serde-bincode-compat`) the `FullSignedTx`
// blanket (`SignedTransaction + MaybeCompact + MaybeSerdeBincodeCompat`)
// also covers us for free — `MaybeCompact` and
// `MaybeSerdeBincodeCompat` degrade to blanket impls on every `T`
// when their respective features are off. Database-bound consumers
// (reth node, provider) enable those features, in which case the
// `FullSignedTx` bar tightens and we'll have to add a real `Compact`
// impl for the envelope in P2b.2c or P2b.2d. See module docs.
//
// Both overridable methods (`is_system_tx`, `is_broadcastable_in_full`)
// stay at their trait defaults:
//
// - `is_system_tx` — XLayerAA is user-initiated, not protocol-injected
//   (contrast with OP's deposit type 0x7E which routes this to `true`
//   via the existing `OpTxEnvelope` impl)
// - `is_broadcastable_in_full` — AA txs have no blob side-car, so the
//   default `!self.is_eip4844()` correctly returns `true`

// --- Bincode compatibility (via RLP) -----------------------------
//
// Marker impl that wires [`SerdeBincodeCompat`] through the RLP
// encode/decode path. Required once
// `reth-primitives-traits/serde-bincode-compat` is active — node
// builds activate it transitively via the reth-chain-state stack
// because in-memory block caches are bincode-serialised.
//
// The blanket `impl SerdeBincodeCompat for T: RlpBincode` in
// primitives-traits makes this a one-line marker; we don't need to
// hand-roll a bincode representation, we just say "use the RLP
// wire form" which we've already tested round-trip.

#[cfg(feature = "serde-bincode-compat")]
impl reth_primitives_traits::serde_bincode_compat::RlpBincode for XLayerTxEnvelope {}

#[cfg(feature = "serde-bincode-compat")]
impl reth_primitives_traits::serde_bincode_compat::RlpBincode for XLayerAAEnvelope {}

// Gate the impl on the crate's `serde` feature because
// [`SignedTransaction`]'s super-trait [`MaybeSerde`] resolves to
// `Serialize + Deserialize` once `reth-primitives-traits/serde` is
// active — which every workspace build activates transitively through
// the reth-node-api / reth-optimism-evm stack. Without gating, a
// `cargo check -p xlayer-consensus --no-default-features` (used by
// `just check-no-std` and fault-proof / prover consumers) would fail
// even though the no-std target never actually calls into
// [`SignedTransaction`]. The gate is not a feature flag "in the real
// product" — workspace consumers always turn `serde` on.
#[cfg(feature = "serde")]
impl SignedTransaction for XLayerAAEnvelope {}

#[cfg(feature = "serde")]
impl SignedTransaction for XLayerTxEnvelope {
    fn is_system_tx(&self) -> bool {
        match &self.0 {
            Extended::BuiltIn(env) => env.is_system_tx(),
            Extended::Other(_) => false,
        }
    }

    fn is_broadcastable_in_full(&self) -> bool {
        match &self.0 {
            Extended::BuiltIn(env) => env.is_broadcastable_in_full(),
            Extended::Other(_) => true,
        }
    }
}

// --- Compact (MDBX storage) ---------------------------------------
//
// `FullSignedTx` requires `MaybeCompact`, which — when
// `reth-primitives-traits/reth-codec` is active — reduces to
// `reth_codecs::Compact`. Workspace builds unify that feature on
// through the reth-node-api tree, so a node-side
// `NodePrimitives::SignedTx = XLayerTxEnvelope` needs a real impl.
//
// The naive implementation below stores the tx as its
// length-prefixed EIP-2718 encoded byte string. It's not the most
// space-efficient option — OpTxEnvelope's upstream Compact impl
// uses bit-packed variant tags + field-level Compact and supports
// zstd compression on big inputs — but it:
//
// 1. **Round-trips** cleanly via the existing 2718 encode/decode pair
//    we've already proved correct via dedicated round-trip tests.
// 2. **Stays stable** across variant additions — future op-stack
//    hardforks can extend `OpTxEnvelope` without forcing an MDBX
//    schema migration on XLayer because we're encoding the 2718
//    wire form, which is fork-stable.
// 3. **Is DB-safe** — MDBX stores arbitrary byte blobs; decode
//    performance is not bottlenecked by 2718 vs Compact (both
//    parse field-by-field).
//
// A future perf-tuning pass can replace this with a proper
// `CompactEnvelope`-style impl if the DB footprint or decode cost
// ever shows up in profiles. The choice is reversible because
// `Compact` writes a length prefix we can branch on to detect the
// old format during a forward migration.
#[cfg(feature = "reth-codec")]
mod compact_impl {
    use super::{Decodable2718, Encodable2718, XLayerTxEnvelope};
    use bytes::BufMut as BytesBufMut;
    use reth_codecs::Compact;
    // `std::vec::Vec` resolves to `alloc::vec::Vec` in no_std mode
    // (via the `extern crate alloc as std` alias in `lib.rs`) and to
    // the real `std::vec::Vec` otherwise. Same trick used elsewhere
    // in this crate to keep one path across both feature-set shapes.
    use std::vec::Vec;

    impl Compact for XLayerTxEnvelope {
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
            let decoded = XLayerTxEnvelope::decode_2718(&mut cursor)
                .expect("Compact stream was written by `to_compact` above");
            (decoded, tail)
        }
    }
}

// --- Wire → exec conversions --------------------------------------
//
// These are the impls the node's pool / payload-builder plumbing
// invokes (via `FromRecoveredTx` / `FromTxWithEncoded`) when it
// hands a recovered transaction to the EVM factory. The older
// `FromRecoveredTx<OpTxEnvelope>` impl in `xlayer-revm/src/tx_env.rs`
// stays in place for nodes that haven't threaded `XLayerTxEnvelope`
// through yet; the pair here lets a XLayer-aware node accept the
// combined envelope and pipe each arm to the right target.
//
// Live here (not in xlayer-revm) because `XLayerTxEnvelope` is local
// to this crate, and the orphan rule requires at least one of
// {trait, generic-arg, implementing-type} to be local. The trait
// parameter `XLayerTxEnvelope` satisfies that; putting the impl in
// xlayer-revm would require xlayer-revm → xlayer-consensus, which
// would break the existing xlayer-consensus → xlayer-revm dep on
// `XLayerAAParts`.

/// Spec used when building `XLayerAAParts` at the recovered-tx
/// boundary. The authoritative spec lives on the EVM context (read
/// by the handler from `ctx.cfg().spec()`), but `FromRecoveredTx`'s
/// signature doesn't expose it, so we use the latest fork we ship —
/// AA is only active at or past XLayerAA activation, which in turn
/// requires Isthmus+, so the default maps to the correct schedule
/// for every currently-reachable spec. A future AA-specific fee
/// revision should either (a) add a new `FromRecoveredTxWith<Spec>`
/// boundary upstream or (b) widen `XLayerAATransaction` to carry the
/// raw `TxEip8130` and let the handler call `build_aa_parts` with
/// the real spec.
const BOUNDARY_SPEC: OpSpecId = OpSpecId::ISTHMUS;

/// Convert an already-recovered [`XLayerTxEnvelope`] into the
/// [`XLayerAATransaction`] the EVM factory expects.
///
/// - [`Extended::BuiltIn`] — delegate to the existing
///   `FromRecoveredTx<OpTxEnvelope>` impl; AA parts stay default.
/// - [`Extended::Other`] — call [`build_aa_parts`] against the
///   inner [`TxEip8130`]. On success, emit a fully-populated
///   [`XLayerAAParts`]; on failure (bad sender_auth, unsupported
///   verifier, feature-not-ready) fall back to default parts so
///   the handler can reject at a later gate rather than panic here
///   — `FromRecoveredTx::from_recovered_tx` is infallible by
///   signature and the pool has already pre-validated structure.
impl FromRecoveredTx<XLayerTxEnvelope> for XLayerAATransaction<RevmTxEnv> {
    fn from_recovered_tx(tx: &XLayerTxEnvelope, sender: Address) -> Self {
        match &tx.0 {
            Extended::BuiltIn(op_env) => {
                <Self as FromRecoveredTx<OpTxEnvelope>>::from_recovered_tx(op_env, sender)
            }
            Extended::Other(aa_env) => build_aa_tx(aa_env.tx(), sender),
        }
    }
}

impl FromTxWithEncoded<XLayerTxEnvelope> for XLayerAATransaction<RevmTxEnv> {
    fn from_encoded_tx(tx: &XLayerTxEnvelope, caller: Address, encoded: Bytes) -> Self {
        match &tx.0 {
            Extended::BuiltIn(op_env) => {
                <Self as FromTxWithEncoded<OpTxEnvelope>>::from_encoded_tx(op_env, caller, encoded)
            }
            Extended::Other(aa_env) => build_aa_tx(aa_env.tx(), caller),
        }
    }
}

/// Shared AA-side boundary for both [`FromRecoveredTx`] and
/// [`FromTxWithEncoded`]. `encoded` is not needed for the AA path —
/// the execution pipeline reads everything from [`XLayerAAParts`] —
/// so the two callers collapse to one helper.
fn build_aa_tx(tx: &TxEip8130, sender: Address) -> XLayerAATransaction<RevmTxEnv> {
    // Start from the upstream `OpTransaction<TxEnv>` projection of a
    // plain tx so base fields (chain_id, gas_limit, priority_fee,
    // kind, value, access list, etc.) are populated. The tx_type
    // gets stamped as 0x7B via the handler's validate_env path; here
    // we just care that the AA parts are present.
    //
    // `to` / `value` / `data` stay at their defaults — AA calls flow
    // through `XLayerAAParts::call_phases`, not the single-call
    // TxEnv fields.
    let revm_tx = RevmTxEnv {
        caller: sender,
        chain_id: Some(tx.chain_id),
        nonce: tx.nonce_sequence,
        gas_limit: tx.gas_limit,
        // `gas_price` isn't meaningful for 1559-style pricing; the
        // priority fee lives in `gas_priority_fee`.
        gas_price: 0,
        gas_priority_fee: Some(tx.max_priority_fee_per_gas),
        tx_type: crate::aa::AA_TX_TYPE_ID,
        ..RevmTxEnv::default()
    };

    let op_tx = op_revm::OpTransaction::new(revm_tx);

    // Try to materialise the AA parts. On any build error, fall
    // back to default parts — the handler's validate_env gates on
    // `XLAYERAA_TX_TYPE + non-empty call_phases`, so an AA tx with
    // empty parts surfaces a clean reject, not a silent success.
    match build_aa_parts(tx, BOUNDARY_SPEC) {
        Ok(built) => XLayerAATransaction::with_parts(op_tx, built.parts),
        Err(_) => XLayerAATransaction::new(op_tx),
    }
}

#[cfg(test)]
mod tests {
    use alloy_consensus::{SignableTransaction, Signed, TxEip1559};
    use alloy_primitives::{Address, Bytes, Signature, TxKind, U256};
    use xlayer_revm::transaction::XLayerAAParts;

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
        match decoded.into_extended() {
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
        match decoded.into_extended() {
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
        let env = XLayerTxEnvelope::aa(sample_aa_tx());

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
        let aa = XLayerTxEnvelope::aa(sample_aa_tx());
        assert!(!aa.is_deposit());

        let eip1559 = XLayerTxEnvelope::builtin(OpTxEnvelope::Eip1559(sample_eip1559()));
        assert!(!eip1559.is_deposit());
    }

    /// The [`FromRecoveredTx<XLayerTxEnvelope>`] impl routes an AA
    /// envelope through [`build_aa_parts`] — on success the parts
    /// are populated; on verification failure (which is what happens
    /// with `sample_aa_tx`'s stub signature) the impl falls back to
    /// default parts but still stamps `tx_type = 0x7B` on the exec
    /// env. Both outcomes are acceptable — the test pins just the
    /// stamp, which proves the AA arm of the dispatcher fired.
    ///
    /// Full "valid sig → non-empty parts" coverage lives in
    /// [`build_aa_parts`]'s own tests — the AA unit module already
    /// exercises the successful K1 / P256 / WebAuthn paths against
    /// real test vectors, and duplicating that plumbing here for
    /// one end-to-end roundtrip would re-derive crypto helpers the
    /// aa module already owns.
    #[test]
    fn from_recovered_tx_stamps_0x7b_for_aa_envelope() {
        let env = XLayerTxEnvelope::aa(sample_aa_tx());

        let converted = <XLayerAATransaction<RevmTxEnv> as FromRecoveredTx<XLayerTxEnvelope>>::from_recovered_tx(
            &env,
            Address::repeat_byte(0x01),
        );

        assert_eq!(
            converted.op.base.tx_type, AA_TX_TYPE_ID,
            "AA-envelope conversion must stamp tx_type 0x7B on the exec env",
        );
        // call_phases depends on whether the embedded auth is
        // verifiable; either 0 (verification failed → default
        // fallback) or the wire tx's phase count is correct.
        assert!(converted.xlayeraa.call_phases.len() <= 1);
    }

    /// `SignerRecoverable::recover_signer` must return the declared
    /// `from` field for configured-account AA txs — no ecrecover.
    /// Our `sample_aa_tx` sets `from = Some(Address::repeat_byte(0x01))`,
    /// so we expect exactly that address back.
    #[test]
    fn recover_signer_returns_configured_from_address() {
        let env = XLayerTxEnvelope::aa(sample_aa_tx());
        let recovered = env.recover_signer().expect("configured account recovers trivially");
        assert_eq!(recovered, Address::repeat_byte(0x01));
    }

    /// EOA-mode AA txs require ecrecover over `sender_signature_hash`.
    /// The stub 0xFF signature in `sample_aa_tx` won't verify cleanly,
    /// but the recovery path must not panic — it surfaces a
    /// `RecoveryError` instead. Pin the "no panic" invariant.
    #[test]
    fn eoa_mode_recover_doesnt_panic_on_bad_sig() {
        let mut tx = sample_aa_tx();
        tx.from = None;
        tx.sender_auth = Bytes::from_static(&[0xFF; 65]);
        let env = XLayerTxEnvelope::aa(tx);
        // Either succeeds (if 0xFF magically yields a valid point) or
        // surfaces an error — either way the call returns.
        let _ = env.recover_signer();
    }

    /// `TxHashRef::tx_hash` on an AA envelope yields the pre-computed
    /// seal hash, not a re-derived one. Pin that the ref matches
    /// `XLayerAAEnvelope::hash()` which is the canonical source.
    #[test]
    fn tx_hash_ref_returns_precomputed_seal() {
        let aa_tx = sample_aa_tx();
        let expected = alloy_consensus::Sealable::hash_slow(&aa_tx);
        let env = XLayerTxEnvelope::aa(aa_tx);
        assert_eq!(*TxHashRef::tx_hash(&env), expected);
    }

    /// `InMemorySize::size` must at least account for `sender_auth`'s
    /// length (one of the dominant variable-size fields in an AA tx).
    /// Pins a sanity lower bound so a future "just return
    /// size_of::<Self>()" regression fires.
    #[test]
    fn in_memory_size_grows_with_sender_auth() {
        let small_auth = XLayerTxEnvelope::aa(TxEip8130 {
            sender_auth: Bytes::from_static(&[0; 8]),
            ..sample_aa_tx()
        });
        let big_auth = XLayerTxEnvelope::aa(TxEip8130 {
            sender_auth: Bytes::from_static(&[0; 8192]),
            ..sample_aa_tx()
        });
        assert!(big_auth.size() > small_auth.size());
    }

    /// Static check: `XLayerTxEnvelope` satisfies the full
    /// [`SignedTransaction`] trait bar. This is the whole reason for
    /// the trait stack — if one of the super-traits regresses, this
    /// compile-gate catches it with a clearer error than surfacing
    /// through a downstream `NodePrimitives::SignedTx` bound three
    /// crates away. Gated on the `serde` feature for the same reason
    /// the impl itself is — see the impl doc block.
    #[cfg(feature = "serde")]
    #[test]
    fn xlayer_tx_envelope_is_signed_transaction() {
        fn assert_signed_tx<T: reth_primitives_traits::SignedTransaction>() {}
        assert_signed_tx::<XLayerTxEnvelope>();
        assert_signed_tx::<XLayerAAEnvelope>();
    }

    /// AA txs are not protocol-injected; they're user-submitted and
    /// must fall on the non-system path. Deposit txs (0x7E) are the
    /// only system-tx kind on op-stack and stay routed via the
    /// BuiltIn arm.
    #[cfg(feature = "serde")]
    #[test]
    fn aa_is_not_system_tx() {
        use reth_primitives_traits::SignedTransaction;
        let env = XLayerTxEnvelope::aa(sample_aa_tx());
        assert!(!env.is_system_tx());
    }

    /// AA txs have no blob sidecar, so they're broadcast-safe in full.
    #[cfg(feature = "serde")]
    #[test]
    fn aa_is_broadcastable_in_full() {
        use reth_primitives_traits::SignedTransaction;
        let env = XLayerTxEnvelope::aa(sample_aa_tx());
        assert!(env.is_broadcastable_in_full());
    }

    /// `try_recover` / `try_clone_into_recovered` are default-method
    /// provisions on [`SignedTransaction`]; they must route back
    /// through our [`SignerRecoverable`] impl. The "happy path"
    /// (configured `from`) exercises the recovered wrapper end-to-end.
    #[cfg(feature = "serde")]
    #[test]
    fn try_clone_into_recovered_roundtrips_configured_sender() {
        use reth_primitives_traits::SignedTransaction;
        let env = XLayerTxEnvelope::aa(sample_aa_tx());
        let recovered = env.try_clone_into_recovered().expect("configured recovers trivially");
        assert_eq!(recovered.signer(), Address::repeat_byte(0x01));
    }

    /// The BuiltIn arm must delegate to the existing
    /// [`FromRecoveredTx<OpTxEnvelope>`] impl — AA parts stay
    /// default-empty, exec env reflects the standard op-revm shape.
    #[test]
    fn from_recovered_tx_for_builtin_is_opaque_delegation() {
        let env = XLayerTxEnvelope::builtin(OpTxEnvelope::Eip1559(sample_eip1559()));

        let converted = <XLayerAATransaction<RevmTxEnv> as FromRecoveredTx<XLayerTxEnvelope>>::from_recovered_tx(
            &env,
            Address::repeat_byte(0x02),
        );

        assert_eq!(converted.xlayeraa, XLayerAAParts::default());
        assert_eq!(converted.op.base.nonce, 7);
    }
}
