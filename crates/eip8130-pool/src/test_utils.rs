//! Test utilities for the EIP-8130 pool crate.
//!
//! Provides a lightweight mock pool transaction that satisfies
//! `PoolTransaction` bounds without pulling in full consensus types.
//!
//! [`MockAaTransaction`] wraps `MockTransaction` with a configurable type byte
//! so that tests can distinguish AA transactions (type 0x7B) from standard ones.

use std::sync::Arc;
use std::time::Instant;

use alloy_consensus::{Transaction as ConsensusTx, Typed2718};
use alloy_eips::eip2930::AccessList;
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use reth_primitives_traits::{InMemorySize, Recovered};
use reth_transaction_pool::{
    identifier::{SenderId, TransactionId},
    PoolTransaction, TransactionOrigin, ValidPoolTransaction,
};
use xlayer_eip8130_consensus::AA_TX_TYPE_ID;

use crate::pool::{ThroughputTier, TierCheckResult};
use crate::transaction::{Eip8130SequenceId, Eip8130TxId};

/// Minimal mock transaction for pool tests.
///
/// Implements the required traits via `reth_transaction_pool::test_utils::MockTransaction`.
/// We re-export it here for convenience.
pub use reth_transaction_pool::test_utils::MockTransaction;

// ── Existing helpers ────────────────────────────────────────────────────

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

// ── MockAaTransaction ───────────────────────────────────────────────────

/// Mock transaction that wraps [`MockTransaction`] with a configurable type byte.
///
/// All trait methods delegate to the inner `MockTransaction` except
/// [`Typed2718::ty`], which returns the stored `type_byte`.
/// This allows simulating AA transactions (type 0x7B) in tests.
#[derive(Debug, Clone)]
pub struct MockAaTransaction {
    inner: MockTransaction,
    type_byte: u8,
}

impl Typed2718 for MockAaTransaction {
    fn ty(&self) -> u8 {
        self.type_byte
    }
}

impl ConsensusTx for MockAaTransaction {
    fn chain_id(&self) -> Option<u64> {
        self.inner.chain_id()
    }
    fn nonce(&self) -> u64 {
        self.inner.nonce()
    }
    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }
    fn gas_price(&self) -> Option<u128> {
        self.inner.gas_price()
    }
    fn max_fee_per_gas(&self) -> u128 {
        self.inner.max_fee_per_gas()
    }
    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.inner.max_priority_fee_per_gas()
    }
    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        self.inner.max_fee_per_blob_gas()
    }
    fn priority_fee_or_price(&self) -> u128 {
        self.inner.priority_fee_or_price()
    }
    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        self.inner.effective_gas_price(base_fee)
    }
    fn is_dynamic_fee(&self) -> bool {
        self.inner.is_dynamic_fee()
    }
    fn kind(&self) -> TxKind {
        self.inner.kind()
    }
    fn is_create(&self) -> bool {
        self.inner.is_create()
    }
    fn value(&self) -> U256 {
        self.inner.value()
    }
    fn input(&self) -> &Bytes {
        self.inner.input()
    }
    fn access_list(&self) -> Option<&AccessList> {
        self.inner.access_list()
    }
    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        self.inner.blob_versioned_hashes()
    }
    fn authorization_list(&self) -> Option<&[alloy_eips::eip7702::SignedAuthorization]> {
        self.inner.authorization_list()
    }
}

impl InMemorySize for MockAaTransaction {
    fn size(&self) -> usize {
        self.inner.size()
    }
}

impl PoolTransaction for MockAaTransaction {
    type TryFromConsensusError = <MockTransaction as PoolTransaction>::TryFromConsensusError;
    type Consensus = <MockTransaction as PoolTransaction>::Consensus;
    type Pooled = <MockTransaction as PoolTransaction>::Pooled;

    fn into_consensus(self) -> Recovered<Self::Consensus> {
        self.inner.into_consensus()
    }

    fn from_pooled(pooled: Recovered<Self::Pooled>) -> Self {
        Self {
            inner: MockTransaction::from_pooled(pooled),
            // Default to EIP-1559 type; callers use the factory to set AA type.
            type_byte: alloy_consensus::constants::EIP1559_TX_TYPE_ID,
        }
    }

    fn hash(&self) -> &alloy_primitives::TxHash {
        self.inner.hash()
    }

    fn sender(&self) -> Address {
        self.inner.sender()
    }

    fn sender_ref(&self) -> &Address {
        self.inner.sender_ref()
    }

    fn cost(&self) -> &U256 {
        self.inner.cost()
    }

    fn encoded_length(&self) -> usize {
        self.inner.encoded_length()
    }
}

// ── MockAaTransactionFactory ────────────────────────────────────────────

/// Factory that produces [`MockAaTransaction`]s wrapped in
/// [`ValidPoolTransaction`] with unique IDs.
#[derive(Default)]
pub struct MockAaTransactionFactory {
    counter: u64,
}

impl MockAaTransactionFactory {
    fn next_id(&mut self) -> u64 {
        let id = self.counter;
        self.counter += 1;
        id
    }

    fn wrap(
        &mut self,
        inner: MockTransaction,
        type_byte: u8,
    ) -> Arc<ValidPoolTransaction<MockAaTransaction>> {
        let id = self.next_id();
        Arc::new(ValidPoolTransaction {
            transaction: MockAaTransaction { inner, type_byte },
            transaction_id: TransactionId::new(SenderId::from(id), id),
            propagate: true,
            timestamp: Instant::now(),
            origin: TransactionOrigin::External,
            authority_ids: None,
        })
    }

    /// Creates a standard (non-AA, EIP-1559) transaction with the given priority fee.
    pub fn create_standard(
        &mut self,
        priority_fee: u128,
    ) -> Arc<ValidPoolTransaction<MockAaTransaction>> {
        let inner = MockTransaction::eip1559()
            .with_priority_fee(priority_fee)
            .with_max_fee(priority_fee + 100)
            .with_gas_limit(21_000);
        self.wrap(inner, alloy_consensus::constants::EIP1559_TX_TYPE_ID)
    }

    /// Creates an AA (type 0x7B) transaction with the given priority fee.
    pub fn create_aa(
        &mut self,
        priority_fee: u128,
    ) -> Arc<ValidPoolTransaction<MockAaTransaction>> {
        let inner = MockTransaction::eip1559()
            .with_priority_fee(priority_fee)
            .with_max_fee(priority_fee + 100)
            .with_gas_limit(21_000);
        self.wrap(inner, AA_TX_TYPE_ID)
    }
}
