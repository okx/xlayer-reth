//! Pool transaction types for EIP-8130 AA transactions.
//!
//! Defines the 2D nonce identity, pre-validated metadata, and sequence lane
//! identifier used throughout the pool.

use std::collections::HashSet;

use alloy_primitives::{Address, U256};

use crate::invalidation::InvalidationKey;

/// Full 2D identity for an AA transaction in the pool.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Eip8130TxId {
    /// Transaction sender.
    pub sender: Address,
    /// Nonce key (lane identifier in the 2D nonce model).
    pub nonce_key: U256,
    /// Nonce sequence within the key.
    pub nonce_sequence: u64,
}

impl Eip8130TxId {
    /// Returns the sequence lane this transaction belongs to.
    pub fn sequence_id(&self) -> Eip8130SequenceId {
        Eip8130SequenceId { sender: self.sender, nonce_key: self.nonce_key }
    }
}

/// Identifies a nonce sequence lane: `(sender, nonce_key)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Eip8130SequenceId {
    /// Transaction sender.
    pub sender: Address,
    /// Nonce key (non-zero for 2D nonce lanes).
    pub nonce_key: U256,
}

/// Pre-validated EIP-8130 metadata attached to a pool transaction.
///
/// Produced by mempool validation and forwarded to the builder so it can
/// skip re-deriving expensive fields like custom verifier execution and
/// invalidation key computation.
#[derive(Debug, Clone)]
pub struct Eip8130Metadata {
    /// The transaction's `nonce_key` (2D nonce lane identifier).
    pub nonce_key: U256,
    /// The sender's current nonce sequence at validation time.
    pub nonce_sequence: u64,
    /// Resolved payer address (`None` for self-pay transactions).
    pub payer: Option<Address>,
    /// Storage slot dependencies for invalidation tracking.
    pub invalidation_keys: HashSet<InvalidationKey>,
    /// Whether the sender's custom verifier execution succeeded.
    pub verifier_passed: bool,
    /// Unix timestamp after which this transaction is invalid. `0` = no expiry.
    pub expiry: u64,
}

/// Result of a successful pool insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// Transaction was inserted as a new entry.
    Added,
    /// Transaction replaced an existing entry at the same 2D nonce.
    Replaced,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_id_sequence_id_derivation() {
        let id = Eip8130TxId {
            sender: Address::repeat_byte(0x01),
            nonce_key: U256::from(42),
            nonce_sequence: 5,
        };
        let seq = id.sequence_id();
        assert_eq!(seq.sender, Address::repeat_byte(0x01));
        assert_eq!(seq.nonce_key, U256::from(42));
    }

    #[test]
    fn tx_id_equality() {
        let a = Eip8130TxId {
            sender: Address::repeat_byte(0x01),
            nonce_key: U256::from(1),
            nonce_sequence: 0,
        };
        let b = a.clone();
        assert_eq!(a, b);

        let c = Eip8130TxId {
            sender: Address::repeat_byte(0x01),
            nonce_key: U256::from(1),
            nonce_sequence: 1,
        };
        assert_ne!(a, c);
    }

    #[test]
    fn metadata_creation() {
        let meta = Eip8130Metadata {
            nonce_key: U256::from(1),
            nonce_sequence: 0,
            payer: None,
            invalidation_keys: HashSet::new(),
            verifier_passed: true,
            expiry: 0,
        };
        assert!(meta.verifier_passed);
        assert_eq!(meta.expiry, 0);
    }

    #[test]
    fn add_outcome_values() {
        assert_ne!(AddOutcome::Added, AddOutcome::Replaced);
    }
}
