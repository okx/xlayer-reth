//! Incremental flashblock access list builder.
//!
//! Accumulates per-account state changes during EVM execution, then finalizes into an
//! EIP-7928 `FlashblockAccessList`.

use crate::access_lists::types::FlashblockAccessList;
use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, U256};
use revm::{bytecode::Bytecode, primitives::StorageValue, state::Account};
use std::collections::{HashMap, HashSet};

/// Incremental builder that accumulates state changes across multiple transactions.
///
/// Each address maps to an `AccountChangesBuilder` that tracks per-field changes
/// indexed by transaction position (`block_access_index`).
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct FlashblockAccessListBuilder {
    /// Per-address change accumulators.
    pub changes: HashMap<Address, AccountChangesBuilder>,
}

impl FlashblockAccessListBuilder {
    /// Creates a new empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Merges another builder into this one.
    ///
    /// For each address in `other`, either merges into an existing entry or inserts it.
    /// This is used to combine results from separate EVM execution phases (e.g., sequencer
    /// transactions followed by pool transactions).
    pub fn merge(&mut self, other: Self) {
        for (address, builder) in other.changes {
            self.changes
                .entry(address)
                .and_modify(|existing| existing.merge(builder.clone()))
                .or_insert(builder);
        }
    }

    /// Processes a transaction's state changeset to extract access list data.
    ///
    /// Inspects the revm `Account` changeset produced by `evm.transact()` to record:
    /// - Storage reads (slots accessed but unchanged)
    /// - Storage writes (slots where `original_value != present_value`)
    /// - Balance changes (diffed against `original_info`)
    /// - Nonce changes (diffed against `original_info`)
    /// - Code changes (diffed against `original_info.code_hash`)
    ///
    /// Call this BEFORE `evm.db_mut().commit(state)` for each transaction.
    pub fn process_transaction_state<'a>(
        &mut self,
        tx_index: u64,
        state: impl IntoIterator<Item = (&'a Address, &'a Account)>,
    ) {
        for (address, account) in state {
            let entry = self.changes.entry(*address).or_default();

            let original = &account.original_info;

            // Diff balance
            if original.balance != account.info.balance {
                entry.balance_changes.insert(tx_index, account.info.balance);
            }

            // Diff nonce
            if original.nonce != account.info.nonce {
                entry.nonce_changes.insert(tx_index, account.info.nonce);
            }

            // Diff code
            if original.code_hash != account.info.code_hash
                && let Some(ref code) = account.info.code
            {
                entry.code_changes.insert(tx_index, code.clone());
            }

            // Process storage: reads and writes
            for (slot, slot_value) in &account.storage {
                let slot_u256: U256 = *slot;
                let present: StorageValue = slot_value.present_value;
                let original_val: StorageValue = slot_value.original_value;

                if original_val != present {
                    // Storage write
                    entry.storage_changes.entry(slot_u256).or_default().insert(tx_index, present);
                } else {
                    // Storage read (accessed but not modified)
                    entry.storage_reads.insert(slot_u256);
                }
            }
        }
    }

    /// Finalizes the builder into a `FlashblockAccessList`.
    ///
    /// Converts all accumulated changes into EIP-7928 `AccountChanges`, sorts by address
    /// for deterministic hashing, and computes the commitment hash.
    pub fn build(self, min_tx_index: u64, max_tx_index: u64) -> FlashblockAccessList {
        let mut account_changes: Vec<AccountChanges> =
            self.changes.into_iter().map(|(address, builder)| builder.build(address)).collect();

        // Sort by address for deterministic ordering and hash stability.
        account_changes.sort_unstable_by_key(|ac| ac.address);

        FlashblockAccessList::build(account_changes, min_tx_index, max_tx_index)
    }
}

/// Per-account change tracker accumulating state diffs indexed by transaction position.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct AccountChangesBuilder {
    /// Storage slot writes: slot -> (tx_index -> new_value).
    pub storage_changes: HashMap<U256, HashMap<u64, U256>>,
    /// Storage slots that were read (deduplicated).
    pub storage_reads: HashSet<U256>,
    /// Balance changes: tx_index -> new_balance.
    pub balance_changes: HashMap<u64, U256>,
    /// Nonce changes: tx_index -> new_nonce.
    pub nonce_changes: HashMap<u64, u64>,
    /// Code changes: tx_index -> new_bytecode.
    pub code_changes: HashMap<u64, Bytecode>,
}

impl AccountChangesBuilder {
    /// Merges another builder's changes into this one.
    pub fn merge(&mut self, other: Self) {
        for (slot, slot_changes) in other.storage_changes {
            self.storage_changes
                .entry(slot)
                .and_modify(|prev| prev.extend(slot_changes.clone()))
                .or_insert(slot_changes);
        }
        self.storage_reads.extend(other.storage_reads);
        self.balance_changes.extend(other.balance_changes);
        self.nonce_changes.extend(other.nonce_changes);
        self.code_changes.extend(other.code_changes);
    }

    /// Converts into an EIP-7928 `AccountChanges`.
    pub fn build(mut self, address: Address) -> AccountChanges {
        AccountChanges {
            address,
            storage_changes: self
                .storage_changes
                .drain()
                .map(|(slot, changes)| SlotChanges {
                    slot,
                    changes: changes
                        .into_iter()
                        .map(|(tx_idx, val)| StorageChange {
                            block_access_index: tx_idx,
                            new_value: val,
                        })
                        .collect(),
                })
                .collect(),
            storage_reads: self.storage_reads.into_iter().collect(),
            balance_changes: self
                .balance_changes
                .into_iter()
                .map(|(tx_idx, val)| BalanceChange {
                    block_access_index: tx_idx,
                    post_balance: val,
                })
                .collect(),
            nonce_changes: self
                .nonce_changes
                .into_iter()
                .map(|(tx_idx, val)| NonceChange { block_access_index: tx_idx, new_nonce: val })
                .collect(),
            code_changes: self
                .code_changes
                .into_iter()
                .map(|(tx_idx, bc)| CodeChange {
                    block_access_index: tx_idx,
                    new_code: bc.original_bytes(),
                })
                .collect(),
        }
    }
}
