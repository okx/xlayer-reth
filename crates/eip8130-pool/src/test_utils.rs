//! Test utilities for the EIP-8130 pool crate.
//!
//! Provides a lightweight mock pool transaction that satisfies
//! `PoolTransaction` bounds without pulling in full consensus types.

use alloy_primitives::{Address, B256, U256};

use crate::pool::{ThroughputTier, TierCheckResult};
use crate::transaction::{Eip8130SequenceId, Eip8130TxId};

/// Minimal mock transaction for pool tests.
///
/// Implements the required traits via `reth_transaction_pool::test_utils::MockTransaction`.
/// We re-export it here for convenience.
pub use reth_transaction_pool::test_utils::MockTransaction;

/// Creates a mock `Eip8130TxId`.
pub fn make_id(sender_byte: u8, nonce_key: u64, nonce_sequence: u64) -> Eip8130TxId {
    Eip8130TxId {
        sender: Address::repeat_byte(sender_byte),
        nonce_key: U256::from(nonce_key),
        nonce_sequence,
    }
}

/// Creates a mock nonce storage slot.
pub fn make_slot(sender_byte: u8, nonce_key: u64) -> B256 {
    let mut buf = [0u8; 32];
    buf[0] = sender_byte;
    buf[24..32].copy_from_slice(&nonce_key.to_be_bytes());
    B256::from(buf)
}

/// Creates a mock pool transaction with a given sender, nonce, and priority fee.
pub fn make_test_tx(sender_byte: u8, nonce: u64, priority_fee: u128) -> MockTransaction {
    MockTransaction::eip1559()
        .with_sender(Address::repeat_byte(sender_byte))
        .with_nonce(nonce)
        .with_priority_fee(priority_fee)
        .with_max_fee(1000)
        .with_gas_limit(21_000)
}

/// Default tier check that returns `ThroughputTier::Default`.
pub fn default_tier_result(_: Address) -> TierCheckResult {
    TierCheckResult { tier: ThroughputTier::Default, cache_for: None }
}

/// Creates a mock `Eip8130SequenceId`.
#[allow(dead_code)]
pub fn make_seq_id(sender_byte: u8, nonce_key: u64) -> Eip8130SequenceId {
    Eip8130SequenceId {
        sender: Address::repeat_byte(sender_byte),
        nonce_key: U256::from(nonce_key),
    }
}
