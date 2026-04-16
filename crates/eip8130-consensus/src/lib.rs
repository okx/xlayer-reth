//! XLayer EIP-8130 Native Account Abstraction - Consensus Types and Logic
//!
//! This crate contains the core types, constants, and logic for EIP-8130 Native AA support.
//! It serves as the foundation layer that other AA crates depend on.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// Deps used in upcoming modules (Phase 1+), suppress unused warnings during skeleton phase.
use alloy_consensus as _;
use alloy_eips as _;
use alloy_primitives as _;
use alloy_rlp as _;

/// EIP-8130 AA transaction type identifier.
pub const AA_TX_TYPE_ID: u8 = 0x7B;

/// Returns `true` if the given transaction type byte is an AA transaction.
pub const fn is_aa_tx_type(tx_type: u8) -> bool {
    tx_type == AA_TX_TYPE_ID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aa_tx_type_id_is_0x7b() {
        assert_eq!(AA_TX_TYPE_ID, 0x7B);
        assert_eq!(AA_TX_TYPE_ID, 123);
    }

    #[test]
    fn is_aa_tx_type_matches() {
        assert!(is_aa_tx_type(0x7B));
        assert!(!is_aa_tx_type(0x00));
        assert!(!is_aa_tx_type(0x02));
        assert!(!is_aa_tx_type(0x7E)); // deposit tx type
    }
}
