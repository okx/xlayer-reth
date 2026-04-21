//! XLayerAA auth blob parsing and signing-hash helpers.
//!
//! The sender and payer hashes are computed from [`TxEip8130`]'s
//! domain-separated preimages (`encode_for_sender_signing` /
//! `encode_for_payer_signing`) — this module only computes `keccak256`
//! over the preimage and parses the auth blob layout.

use std::vec::Vec;

use alloy_primitives::{keccak256, Address, Bytes, B256};

use super::{types::ConfigChangeEntry, TxEip8130};

/// Hash committed by the sender's signature:
/// `keccak256(tx.encode_for_sender_signing())`.
pub fn sender_signature_hash(tx: &TxEip8130) -> B256 {
    let mut buf = Vec::with_capacity(512);
    tx.encode_for_sender_signing(&mut buf);
    keccak256(&buf)
}

/// Hash committed by the payer's signature:
/// `keccak256(tx.encode_for_payer_signing())`.
pub fn payer_signature_hash(tx: &TxEip8130) -> B256 {
    let mut buf = Vec::with_capacity(512);
    tx.encode_for_payer_signing(&mut buf);
    keccak256(&buf)
}

/// EIP-712 digest committed by a config-change authorizer. Matches the JS
/// reference `send-aa-tx.mjs :: configChangeDigest`.
///
/// Struct layout:
///
/// ```solidity
/// SignedOwnerChanges(
///     address account,
///     uint64 chainId,
///     uint64 sequence,
///     OwnerChange[] ownerChanges
/// )
/// OwnerChange(uint8 changeType, address verifier, bytes32 ownerId, uint8 scope)
/// ```
pub fn config_change_digest(account: Address, change: &ConfigChangeEntry) -> B256 {
    let typehash = keccak256(
        "SignedOwnerChanges(address account,uint64 chainId,uint64 sequence,\
         OwnerChange[] ownerChanges)\
         OwnerChange(uint8 changeType,address verifier,bytes32 ownerId,uint8 scope)",
    );

    // Hash each OwnerChange, then hash the concatenation.
    let mut change_hashes = Vec::with_capacity(change.owner_changes.len() * 32);
    for oc in &change.owner_changes {
        let mut buf = [0u8; 128]; // 4 × 32-byte words.
        buf[31] = oc.change_type;
        buf[44..64].copy_from_slice(oc.verifier.as_slice());
        buf[64..96].copy_from_slice(oc.owner_id.as_slice());
        buf[127] = oc.scope;
        change_hashes.extend_from_slice(keccak256(buf).as_slice());
    }
    let owner_changes_hash = keccak256(&change_hashes);

    let mut buf = [0u8; 160]; // 5 × 32-byte words.
    buf[0..32].copy_from_slice(typehash.as_slice());
    buf[44..64].copy_from_slice(account.as_slice());
    buf[88..96].copy_from_slice(&change.chain_id.to_be_bytes());
    buf[120..128].copy_from_slice(&change.sequence.to_be_bytes());
    buf[128..160].copy_from_slice(owner_changes_hash.as_slice());
    keccak256(buf)
}

/// Result of parsing `sender_auth` according to the tx's `from` flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSenderAuth {
    /// EOA mode (`tx.from.is_none()`): the blob is a raw 65-byte ECDSA
    /// signature `r ‖ s ‖ v`. The sender is recovered from this signature.
    Eoa {
        /// Raw 65-byte ECDSA signature.
        signature: [u8; 65],
    },
    /// Configured owner mode: `verifier(20) ‖ verifier-specific data`. The
    /// verifier address selects the native (K1 / P256 / WebAuthn / Delegate)
    /// or custom contract path.
    Configured {
        /// First 20 bytes of the blob.
        verifier: Address,
        /// Bytes after the verifier prefix.
        data: Bytes,
    },
}

/// Parses `sender_auth` given the tx's `from` flag.
///
/// Errors surface structural problems (wrong blob size) — the caller is
/// expected to propagate them as the tx-level error rather than panic.
pub fn parse_sender_auth(tx: &TxEip8130) -> Result<ParsedSenderAuth, &'static str> {
    if tx.is_eoa() {
        if tx.sender_auth.len() != 65 {
            return Err("EOA sender_auth must be exactly 65 bytes");
        }
        let mut sig = [0u8; 65];
        sig.copy_from_slice(&tx.sender_auth);
        return Ok(ParsedSenderAuth::Eoa { signature: sig });
    }

    if tx.sender_auth.len() < 20 {
        return Err("configured sender_auth must contain at least a 20-byte verifier address");
    }
    let verifier = Address::from_slice(&tx.sender_auth[..20]);
    let data = Bytes::copy_from_slice(&tx.sender_auth[20..]);
    Ok(ParsedSenderAuth::Configured { verifier, data })
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;

    use super::*;

    fn tx_template() -> TxEip8130 {
        TxEip8130 {
            chain_id: 1,
            from: Some(Address::repeat_byte(0x01)),
            nonce_key: U256::ZERO,
            nonce_sequence: 1,
            ..Default::default()
        }
    }

    #[test]
    fn parse_eoa_auth_ok() {
        let tx = TxEip8130 {
            from: None,
            sender_auth: Bytes::from_static(&[0xABu8; 65]),
            ..tx_template()
        };
        let parsed = parse_sender_auth(&tx).unwrap();
        assert!(matches!(parsed, ParsedSenderAuth::Eoa { .. }));
    }

    #[test]
    fn parse_eoa_wrong_length_rejected() {
        let tx = TxEip8130 {
            from: None,
            sender_auth: Bytes::from_static(&[0xABu8; 64]),
            ..tx_template()
        };
        assert!(parse_sender_auth(&tx).is_err());
    }

    #[test]
    fn parse_configured_k1() {
        use super::super::verifier::K1_VERIFIER_ADDRESS;
        let mut auth = Vec::new();
        auth.extend_from_slice(K1_VERIFIER_ADDRESS.as_slice());
        auth.extend_from_slice(&[0xABu8; 65]);
        let tx = TxEip8130 {
            from: Some(Address::repeat_byte(0x01)),
            sender_auth: Bytes::from(auth),
            ..tx_template()
        };
        match parse_sender_auth(&tx).unwrap() {
            ParsedSenderAuth::Configured { verifier, data } => {
                assert_eq!(verifier, K1_VERIFIER_ADDRESS);
                assert_eq!(data.len(), 65);
            }
            _ => panic!("expected Configured"),
        }
    }

    #[test]
    fn parse_configured_too_short_rejected() {
        let tx = TxEip8130 {
            from: Some(Address::repeat_byte(0x01)),
            sender_auth: Bytes::from_static(&[0x01u8; 10]),
            ..tx_template()
        };
        assert!(parse_sender_auth(&tx).is_err());
    }

    #[test]
    fn sender_and_payer_hashes_are_deterministic_and_disjoint() {
        let tx = tx_template();
        let h1 = sender_signature_hash(&tx);
        let h2 = sender_signature_hash(&tx);
        assert_eq!(h1, h2);
        let p1 = payer_signature_hash(&tx);
        let p2 = payer_signature_hash(&tx);
        assert_eq!(p1, p2);
        assert_ne!(h1, p1, "sender and payer hashes must differ");
    }
}
