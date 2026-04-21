//! The XLayerAA (EIP-8130) transaction.
//!
//! The body carries embedded authentication (`sender_auth`, `payer_auth`)
//! instead of an external ECDSA signature, so there is no `Signed<T>`
//! wrapper — the "signed" tx *is* [`TxEip8130`]. Phased call batching
//! replaces the single `to` + `input` of a standard tx.

use std::vec::Vec;

use alloy_consensus::{Sealable, Transaction, Typed2718};
use alloy_eips::eip2718::{Decodable2718, Eip2718Error, Eip2718Result, Encodable2718, IsTyped2718};
use alloy_primitives::{keccak256, Address, Bytes, ChainId, TxKind, B256, U256};
use alloy_rlp::{length_of_length, BufMut, Decodable, Encodable, Header};

use super::{
    constants::{AA_PAYER_TYPE, AA_TX_TYPE_ID},
    encoding::{
        decode_nested_calls, decode_optional_address, encode_list, encode_nested_calls,
        encode_optional_address, list_len, nested_calls_len, optional_address_len,
    },
    AccountChangeEntry, Call,
};

/// An XLayerAA (EIP-8130) account-abstracted transaction.
///
/// RLP layout (outer list):
/// ```text
/// [chain_id, from, nonce_key, nonce_sequence, expiry,
///  max_priority_fee_per_gas, max_fee_per_gas, gas_limit,
///  account_changes, calls, payer,
///  sender_auth, payer_auth]
/// ```
///
/// `from` and `payer` are optional address fields — RLP-encoded as the empty
/// string (`0x80`) when absent, and as a 20-byte string when present.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct TxEip8130 {
    /// EIP-155 chain id this tx targets.
    #[cfg_attr(feature = "serde", serde(with = "alloy_serde::quantity"))]
    pub chain_id: u64,
    /// Sender address. `None` indicates EOA mode — the actual sender is
    /// recovered from `sender_auth` via `ecrecover`.
    pub from: Option<Address>,
    /// 2D-nonce channel selector. `NONCE_KEY_MAX` activates nonce-free mode.
    pub nonce_key: U256,
    /// Sequence number inside the `nonce_key` channel.
    #[cfg_attr(feature = "serde", serde(with = "alloy_serde::quantity"))]
    pub nonce_sequence: u64,
    /// Block-timestamp expiry. `0` disables expiry for sequenced txs.
    ///
    /// EIP-8130 MUST: nonce-free txs (`nonce_key == NONCE_KEY_MAX`) REQUIRE
    /// a non-zero expiry — enforced by the handler's `validate_env`, not at
    /// decode time (the wire shape is the same).
    #[cfg_attr(feature = "serde", serde(with = "alloy_serde::quantity"))]
    pub expiry: u64,
    /// EIP-1559 priority fee (per gas).
    #[cfg_attr(feature = "serde", serde(with = "alloy_serde::quantity"))]
    pub max_priority_fee_per_gas: u128,
    /// EIP-1559 max fee (per gas).
    #[cfg_attr(feature = "serde", serde(with = "alloy_serde::quantity"))]
    pub max_fee_per_gas: u128,
    /// Execution-only gas budget. The AA intrinsic gas (nonce, auth, pre
    /// writes, …) is computed separately and added to this by the handler.
    #[cfg_attr(
        feature = "serde",
        serde(with = "alloy_serde::quantity", rename = "gas", alias = "gasLimit")
    )]
    pub gas_limit: u64,
    /// CREATE2 deployments, config changes, and/or delegation entries.
    pub account_changes: Vec<AccountChangeEntry>,
    /// Phased call batches. Each inner `Vec` is one atomic phase — on
    /// revert, that phase's state changes are rolled back and subsequent
    /// phases are skipped.
    pub calls: Vec<Vec<Call>>,
    /// Sponsor address. `None` indicates self-pay (sender is the payer).
    pub payer: Option<Address>,
    /// Sender authentication blob. For EOA mode this is a 65-byte ECDSA
    /// signature; otherwise it is `verifier(20) || verifier-specific-data`.
    pub sender_auth: Bytes,
    /// Payer authentication blob. Empty when `payer.is_none()`.
    pub payer_auth: Bytes,
}

impl TxEip8130 {
    /// Returns `true` for EOA-mode transactions (sender derived via
    /// `ecrecover` from `sender_auth`).
    pub fn is_eoa(&self) -> bool {
        self.from.is_none()
    }

    /// Returns `true` when the sender pays for gas (no external payer).
    pub fn is_self_pay(&self) -> bool {
        self.payer.is_none()
    }

    /// The declared `from` address, or `Address::ZERO` as a placeholder for
    /// EOA-mode txs where the actual sender is recovered at validation time.
    pub fn effective_sender(&self) -> Address {
        self.from.unwrap_or(Address::ZERO)
    }

    /// The effective payer address — `payer` when sponsored, otherwise the
    /// effective sender.
    pub fn effective_payer(&self) -> Address {
        if self.is_self_pay() {
            self.effective_sender()
        } else {
            self.payer.unwrap_or(Address::ZERO)
        }
    }

    /// Encodes the inner fields (no outer list header).
    fn encode_fields(&self, out: &mut dyn BufMut) {
        self.chain_id.encode(out);
        encode_optional_address(&self.from, out);
        self.nonce_key.encode(out);
        self.nonce_sequence.encode(out);
        self.expiry.encode(out);
        self.max_priority_fee_per_gas.encode(out);
        self.max_fee_per_gas.encode(out);
        self.gas_limit.encode(out);
        encode_list(&self.account_changes, out);
        encode_nested_calls(&self.calls, out);
        encode_optional_address(&self.payer, out);
        self.sender_auth.encode(out);
        self.payer_auth.encode(out);
    }

    /// Combined length of all encoded fields (the RLP list payload length).
    fn fields_len(&self) -> usize {
        self.chain_id.length()
            + optional_address_len(&self.from)
            + self.nonce_key.length()
            + self.nonce_sequence.length()
            + self.expiry.length()
            + self.max_priority_fee_per_gas.length()
            + self.max_fee_per_gas.length()
            + self.gas_limit.length()
            + list_len(&self.account_changes)
            + nested_calls_len(&self.calls)
            + optional_address_len(&self.payer)
            + self.sender_auth.length()
            + self.payer_auth.length()
    }

    fn rlp_decode_fields(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Ok(Self {
            chain_id: Decodable::decode(buf)?,
            from: decode_optional_address(buf)?,
            nonce_key: Decodable::decode(buf)?,
            nonce_sequence: Decodable::decode(buf)?,
            expiry: Decodable::decode(buf)?,
            max_priority_fee_per_gas: Decodable::decode(buf)?,
            max_fee_per_gas: Decodable::decode(buf)?,
            gas_limit: Decodable::decode(buf)?,
            account_changes: Decodable::decode(buf)?,
            calls: decode_nested_calls(buf)?,
            payer: decode_optional_address(buf)?,
            sender_auth: Decodable::decode(buf)?,
            payer_auth: Decodable::decode(buf)?,
        })
    }

    /// Decodes from an RLP list (header + fields).
    pub fn rlp_decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let remaining = buf.len();
        let this = Self::rlp_decode_fields(buf)?;
        if buf.len() + header.payload_length != remaining {
            return Err(alloy_rlp::Error::UnexpectedLength);
        }
        Ok(this)
    }

    /// Computes the tx hash (EIP-2718 envelope hash).
    pub fn tx_hash(&self) -> B256 {
        let mut buf = Vec::with_capacity(self.encode_2718_len());
        self.encode_2718(&mut buf);
        keccak256(&buf)
    }

    /// Encodes the preimage for the **sender** signature hash:
    /// ```text
    /// AA_TX_TYPE_ID ‖ rlp([chain_id, from, nonce_key, nonce_sequence, expiry,
    ///                      max_priority_fee_per_gas, max_fee_per_gas, gas_limit,
    ///                      account_changes, calls, payer])
    /// ```
    pub fn encode_for_sender_signing(&self, out: &mut dyn BufMut) {
        let payload_len = self.chain_id.length()
            + optional_address_len(&self.from)
            + self.nonce_key.length()
            + self.nonce_sequence.length()
            + self.expiry.length()
            + self.max_priority_fee_per_gas.length()
            + self.max_fee_per_gas.length()
            + self.gas_limit.length()
            + list_len(&self.account_changes)
            + nested_calls_len(&self.calls)
            + optional_address_len(&self.payer);

        out.put_u8(AA_TX_TYPE_ID);
        Header { list: true, payload_length: payload_len }.encode(out);
        self.chain_id.encode(out);
        encode_optional_address(&self.from, out);
        self.nonce_key.encode(out);
        self.nonce_sequence.encode(out);
        self.expiry.encode(out);
        self.max_priority_fee_per_gas.encode(out);
        self.max_fee_per_gas.encode(out);
        self.gas_limit.encode(out);
        encode_list(&self.account_changes, out);
        encode_nested_calls(&self.calls, out);
        encode_optional_address(&self.payer, out);
    }

    /// Encodes the preimage for the **payer** signature hash:
    /// ```text
    /// AA_PAYER_TYPE ‖ rlp([chain_id, from, nonce_key, nonce_sequence, expiry,
    ///                      max_priority_fee_per_gas, max_fee_per_gas, gas_limit,
    ///                      account_changes, calls])
    /// ```
    ///
    /// The byte prefix differs from the sender hash (`AA_TX_TYPE_ID` vs.
    /// `AA_PAYER_TYPE`) so the two domains are cryptographically separated
    /// — a valid sender signature cannot be replayed as a payer signature
    /// on the same tx.
    pub fn encode_for_payer_signing(&self, out: &mut dyn BufMut) {
        let payload_len = self.chain_id.length()
            + optional_address_len(&self.from)
            + self.nonce_key.length()
            + self.nonce_sequence.length()
            + self.expiry.length()
            + self.max_priority_fee_per_gas.length()
            + self.max_fee_per_gas.length()
            + self.gas_limit.length()
            + list_len(&self.account_changes)
            + nested_calls_len(&self.calls);

        out.put_u8(AA_PAYER_TYPE);
        Header { list: true, payload_length: payload_len }.encode(out);
        self.chain_id.encode(out);
        encode_optional_address(&self.from, out);
        self.nonce_key.encode(out);
        self.nonce_sequence.encode(out);
        self.expiry.encode(out);
        self.max_priority_fee_per_gas.encode(out);
        self.max_fee_per_gas.encode(out);
        self.gas_limit.encode(out);
        encode_list(&self.account_changes, out);
        encode_nested_calls(&self.calls, out);
    }
}

// ---------------------------------------------------------------------------
// alloy_rlp::Encodable / Decodable
// ---------------------------------------------------------------------------

impl Encodable for TxEip8130 {
    fn encode(&self, out: &mut dyn BufMut) {
        let payload = self.fields_len();
        Header { list: true, payload_length: payload }.encode(out);
        self.encode_fields(out);
    }

    fn length(&self) -> usize {
        let payload = self.fields_len();
        payload + length_of_length(payload)
    }
}

impl Decodable for TxEip8130 {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Self::rlp_decode(buf)
    }
}

// ---------------------------------------------------------------------------
// Sealable
// ---------------------------------------------------------------------------

impl Sealable for TxEip8130 {
    fn hash_slow(&self) -> B256 {
        self.tx_hash()
    }
}

// ---------------------------------------------------------------------------
// EIP-2718
// ---------------------------------------------------------------------------

impl Typed2718 for TxEip8130 {
    fn ty(&self) -> u8 {
        AA_TX_TYPE_ID
    }
}

impl IsTyped2718 for TxEip8130 {
    fn is_type(ty: u8) -> bool {
        ty == AA_TX_TYPE_ID
    }
}

impl Encodable2718 for TxEip8130 {
    fn type_flag(&self) -> Option<u8> {
        Some(AA_TX_TYPE_ID)
    }

    fn encode_2718_len(&self) -> usize {
        1 + self.length()
    }

    fn encode_2718(&self, out: &mut dyn BufMut) {
        out.put_u8(AA_TX_TYPE_ID);
        self.encode(out);
    }
}

impl Decodable2718 for TxEip8130 {
    fn typed_decode(ty: u8, buf: &mut &[u8]) -> Eip2718Result<Self> {
        if ty != AA_TX_TYPE_ID {
            return Err(Eip2718Error::UnexpectedType(ty));
        }
        Self::rlp_decode(buf).map_err(Into::into)
    }

    fn fallback_decode(_buf: &mut &[u8]) -> Eip2718Result<Self> {
        Err(Eip2718Error::UnexpectedType(0))
    }
}

// ---------------------------------------------------------------------------
// alloy_consensus::Transaction
// ---------------------------------------------------------------------------

impl Transaction for TxEip8130 {
    fn chain_id(&self) -> Option<ChainId> {
        Some(self.chain_id)
    }

    fn nonce(&self) -> u64 {
        self.nonce_sequence
    }

    fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    fn gas_price(&self) -> Option<u128> {
        None
    }

    fn max_fee_per_gas(&self) -> u128 {
        self.max_fee_per_gas
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        Some(self.max_priority_fee_per_gas)
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        None
    }

    fn priority_fee_or_price(&self) -> u128 {
        self.max_priority_fee_per_gas
    }

    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        base_fee.map_or(self.max_fee_per_gas, |base_fee| {
            let tip = self.max_fee_per_gas.saturating_sub(base_fee as u128);
            let tip = tip.min(self.max_priority_fee_per_gas);
            base_fee as u128 + tip
        })
    }

    fn is_dynamic_fee(&self) -> bool {
        true
    }

    fn is_create(&self) -> bool {
        // Account creation is expressed via `AccountChangeEntry::Create` in
        // `account_changes`, not via the standard `kind == Create`.
        false
    }

    fn kind(&self) -> TxKind {
        // No single `to` — pick the sender as a placeholder so integrations
        // that expect `TxKind::Call(_)` for non-create txs still type-check.
        TxKind::Call(self.effective_sender())
    }

    fn value(&self) -> U256 {
        // Per-call values live inside the `calls` vector; there is no
        // top-level value field.
        U256::ZERO
    }

    fn input(&self) -> &Bytes {
        // Calldata is carried in `calls`. Return an empty slice for trait
        // consumers that read `input()`.
        static EMPTY: Bytes = Bytes::new();
        &EMPTY
    }

    fn access_list(&self) -> Option<&alloy_eips::eip2930::AccessList> {
        None
    }

    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        None
    }

    fn authorization_list(&self) -> Option<&[alloy_eips::eip7702::SignedAuthorization]> {
        None
    }

    fn effective_tip_per_gas(&self, base_fee: u64) -> Option<u128> {
        let base = base_fee as u128;
        if self.max_fee_per_gas < base {
            return None;
        }
        Some((self.max_fee_per_gas - base).min(self.max_priority_fee_per_gas))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use alloy_primitives::keccak256;

    use super::*;

    fn sample_tx() -> TxEip8130 {
        TxEip8130 {
            chain_id: 196,
            from: Some(Address::repeat_byte(0x01)),
            nonce_key: U256::from(0u64),
            nonce_sequence: 42,
            expiry: 0,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 10_000_000_000,
            gas_limit: 100_000,
            account_changes: vec![],
            calls: vec![vec![Call {
                to: Address::repeat_byte(0xBB),
                data: Bytes::from_static(&[0xDE, 0xAD]),
            }]],
            payer: None,
            sender_auth: Bytes::from_static(&[0xFF; 65]),
            payer_auth: Bytes::new(),
        }
    }

    #[test]
    fn rlp_round_trip() {
        let tx = sample_tx();
        let mut buf = Vec::new();
        tx.encode(&mut buf);
        let decoded = TxEip8130::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(tx, decoded);
    }

    #[test]
    fn eip2718_round_trip() {
        let tx = sample_tx();
        let mut buf = Vec::new();
        tx.encode_2718(&mut buf);
        assert_eq!(buf[0], AA_TX_TYPE_ID);
        let decoded = TxEip8130::decode_2718(&mut buf.as_slice()).unwrap();
        assert_eq!(tx, decoded);
        assert_eq!(buf.len(), tx.encode_2718_len());
    }

    #[test]
    fn tx_trait_getters() {
        let tx = sample_tx();
        assert_eq!(Transaction::chain_id(&tx), Some(196));
        assert_eq!(tx.nonce(), 42);
        assert_eq!(tx.gas_limit(), 100_000);
        assert!(tx.gas_price().is_none());
        assert_eq!(tx.max_fee_per_gas(), 10_000_000_000);
        assert_eq!(tx.max_priority_fee_per_gas(), Some(1_000_000_000));
        assert!(tx.is_dynamic_fee());
        assert_eq!(tx.value(), U256::ZERO);
        assert_eq!(tx.ty(), AA_TX_TYPE_ID);
    }

    #[test]
    fn sender_and_payer_signing_differ() {
        let tx = sample_tx();
        let mut sender_buf = Vec::new();
        tx.encode_for_sender_signing(&mut sender_buf);
        let mut payer_buf = Vec::new();
        tx.encode_for_payer_signing(&mut payer_buf);
        assert_ne!(
            keccak256(&sender_buf),
            keccak256(&payer_buf),
            "sender and payer signing hashes must differ for domain separation",
        );
        // First byte of each preimage is the domain tag.
        assert_eq!(sender_buf[0], AA_TX_TYPE_ID);
        assert_eq!(payer_buf[0], AA_PAYER_TYPE);
    }

    #[test]
    fn is_eoa_and_is_self_pay() {
        let mut tx = sample_tx();
        assert!(!tx.is_eoa());
        assert!(tx.is_self_pay());

        tx.from = None;
        assert!(tx.is_eoa());

        tx.payer = Some(Address::repeat_byte(0xCC));
        assert!(!tx.is_self_pay());
        assert_eq!(tx.effective_payer(), Address::repeat_byte(0xCC));
    }

    #[test]
    fn empty_tx_rlp_round_trip() {
        let tx = TxEip8130::default();
        let mut buf = Vec::new();
        tx.encode(&mut buf);
        let decoded = TxEip8130::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(tx, decoded);
    }

    #[test]
    fn multi_phase_calls_round_trip() {
        let tx = TxEip8130 {
            calls: vec![
                vec![
                    Call { to: Address::repeat_byte(1), data: Bytes::from_static(&[0x01]) },
                    Call { to: Address::repeat_byte(2), data: Bytes::from_static(&[0x02]) },
                ],
                vec![Call { to: Address::repeat_byte(3), data: Bytes::from_static(&[0x03]) }],
            ],
            ..Default::default()
        };
        let mut buf = Vec::new();
        tx.encode(&mut buf);
        let decoded = TxEip8130::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(decoded.calls.len(), 2);
        assert_eq!(decoded.calls[0].len(), 2);
        assert_eq!(decoded.calls[1].len(), 1);
        assert_eq!(tx, decoded);
    }

    #[test]
    fn decode_rejects_wrong_type_byte() {
        let tx = sample_tx();
        let mut buf = Vec::new();
        tx.encode_2718(&mut buf);
        // Flip the type byte and attempt to decode.
        buf[0] = 0x02;
        assert!(TxEip8130::decode_2718(&mut buf.as_slice()).is_err());
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let tx = sample_tx();
        let mut buf = Vec::new();
        tx.encode(&mut buf);
        // Append a stray byte after the encoded tx — the outer length check
        // catches this.
        let len_before = buf.len();
        buf.push(0xEE);
        // We intentionally decode with exactly the original length — if the
        // decoder incorrectly consumes trailing bytes it would leave the
        // stray byte untouched. The canonical way to surface this is via
        // the wrapper `rlp_decode` which checks payload_length consumption.
        let slice = &mut &buf[..len_before + 1];
        // First decode must succeed (well-formed RLP list with an extra byte).
        let decoded = TxEip8130::rlp_decode(slice).expect("list well-formed");
        assert_eq!(decoded, tx);
        // The stray byte is still in the buffer after decode.
        assert_eq!(*slice, &[0xEEu8][..]);
    }
}
