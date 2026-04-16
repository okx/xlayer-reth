//! State-diff based invalidation for EIP-8130 Account Abstraction transactions.
//!
//! Tracks which storage slots each pending AA transaction depends on, so that
//! when a block's state diff reports changed slots, the affected transactions
//! can be efficiently identified and evicted from the mempool.
//!
//! The [`Eip8130InvalidationIndex`] is populated during validation and read
//! by the maintenance task when canonical state updates arrive.

use std::collections::{HashMap, HashSet};

use alloy_primitives::{Address, B256};

/// A (contract address, storage slot) pair that an AA transaction depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InvalidationKey {
    /// The contract whose storage is being watched.
    pub address: Address,
    /// The specific storage slot within that contract.
    pub slot: B256,
}

/// Index that maps invalidation keys to the set of transaction hashes that
/// depend on them. Also tracks per-payer pending counts for sponsored AA txs.
#[derive(Debug, Default)]
pub struct Eip8130InvalidationIndex {
    /// Forward map: key → transaction hashes that depend on it.
    key_to_txs: HashMap<InvalidationKey, HashSet<B256>>,
    /// Reverse map: tx hash → its invalidation keys.
    tx_to_keys: HashMap<B256, HashSet<InvalidationKey>>,
    /// Payer address for each tx (only for sponsored txs where payer != sender).
    tx_to_payer: HashMap<B256, Address>,
    /// Number of pending pool txs per payer address.
    payer_counts: HashMap<Address, usize>,
}

impl Eip8130InvalidationIndex {
    /// Creates a new, empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a transaction and its invalidation keys into the index.
    ///
    /// If `payer` is `Some`, tracks the payer for pending-count enforcement.
    /// Pass `Some(payer)` for sponsored AA txs where payer != sender.
    pub fn insert(
        &mut self,
        tx_hash: B256,
        keys: HashSet<InvalidationKey>,
        payer: Option<Address>,
    ) {
        for key in &keys {
            self.key_to_txs.entry(*key).or_default().insert(tx_hash);
        }
        self.tx_to_keys.insert(tx_hash, keys);
        if let Some(addr) = payer {
            self.tx_to_payer.insert(tx_hash, addr);
            *self.payer_counts.entry(addr).or_default() += 1;
        }
    }

    /// Returns the set of transaction hashes affected by the given key.
    pub fn lookup(&self, key: &InvalidationKey) -> Option<&HashSet<B256>> {
        self.key_to_txs.get(key)
    }

    /// Removes a transaction from the index, cleaning up all associated keys.
    pub fn remove(&mut self, tx_hash: &B256) {
        if let Some(keys) = self.tx_to_keys.remove(tx_hash) {
            for key in &keys {
                if let Some(txs) = self.key_to_txs.get_mut(key) {
                    txs.remove(tx_hash);
                    if txs.is_empty() {
                        self.key_to_txs.remove(key);
                    }
                }
            }
        }
        if let Some(payer) = self.tx_to_payer.remove(tx_hash)
            && let Some(count) = self.payer_counts.get_mut(&payer)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.payer_counts.remove(&payer);
            }
        }
    }

    /// Returns the set of transaction hashes invalidated by any of the given keys.
    pub fn invalidated_by(&self, keys: &[InvalidationKey]) -> HashSet<B256> {
        let mut result = HashSet::new();
        for key in keys {
            if let Some(txs) = self.key_to_txs.get(key) {
                result.extend(txs);
            }
        }
        result
    }

    /// Removes stale entries (txs not in the `live` set), returns the number pruned.
    pub fn prune_stale(&mut self, live: &HashSet<B256>) -> usize {
        let stale: Vec<B256> =
            self.tx_to_keys.keys().filter(|h| !live.contains(*h)).copied().collect();
        let count = stale.len();
        for hash in stale {
            self.remove(&hash);
        }
        count
    }

    /// Returns the number of transactions tracked.
    pub fn len(&self) -> usize {
        self.tx_to_keys.len()
    }

    /// Returns `true` if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.tx_to_keys.is_empty()
    }

    /// Returns the pending tx count for a given payer.
    pub fn payer_count(&self, payer: &Address) -> usize {
        self.payer_counts.get(payer).copied().unwrap_or(0)
    }
}

/// Computes invalidation keys for an AA transaction.
///
/// Each AA tx depends on:
/// - Its nonce slot in NonceManager
/// - Owner config slots for each owner referenced in account_changes
/// - The account state slot (for sequence-based changes)
pub fn compute_invalidation_keys(
    sender: Address,
    nonce_key: alloy_primitives::U256,
    account_changes: &[xlayer_eip8130_consensus::AccountChangeEntry],
) -> HashSet<InvalidationKey> {
    use xlayer_eip8130_consensus::{
        account_state_slot, nonce_slot, owner_config_slot, AccountChangeEntry,
        ACCOUNT_CONFIG_ADDRESS, NONCE_MANAGER_ADDRESS,
    };

    let mut keys = HashSet::new();

    // Nonce slot dependency
    let nonce_s = nonce_slot(sender, nonce_key);
    keys.insert(InvalidationKey { address: NONCE_MANAGER_ADDRESS, slot: nonce_s });

    // Account state slot (for config change sequences)
    let state_s = account_state_slot(sender);
    keys.insert(InvalidationKey { address: ACCOUNT_CONFIG_ADDRESS, slot: state_s });

    // Owner config slots for each owner referenced in account changes
    for entry in account_changes {
        match entry {
            AccountChangeEntry::Create(create) => {
                for owner in &create.initial_owners {
                    let slot = owner_config_slot(sender, owner.owner_id);
                    keys.insert(InvalidationKey { address: ACCOUNT_CONFIG_ADDRESS, slot });
                }
            }
            AccountChangeEntry::ConfigChange(cc) => {
                for op in &cc.owner_changes {
                    let slot = owner_config_slot(sender, op.owner_id);
                    keys.insert(InvalidationKey { address: ACCOUNT_CONFIG_ADDRESS, slot });
                }
            }
            AccountChangeEntry::Delegation(_) => {}
        }
    }

    keys
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256};

    use super::*;

    fn make_key(addr_byte: u8, slot_byte: u8) -> InvalidationKey {
        InvalidationKey {
            address: Address::repeat_byte(addr_byte),
            slot: B256::repeat_byte(slot_byte),
        }
    }

    #[test]
    fn insert_and_lookup() {
        let mut idx = Eip8130InvalidationIndex::new();
        let hash = B256::repeat_byte(0x01);
        let key = make_key(0xAA, 0x01);

        idx.insert(hash, HashSet::from([key]), None);

        assert_eq!(idx.len(), 1);
        assert!(idx.lookup(&key).unwrap().contains(&hash));
    }

    #[test]
    fn remove_cleans_up_all_maps() {
        let mut idx = Eip8130InvalidationIndex::new();
        let hash = B256::repeat_byte(0x01);
        let key1 = make_key(0xAA, 0x01);
        let key2 = make_key(0xAA, 0x02);
        let payer = Address::repeat_byte(0xBB);

        idx.insert(hash, HashSet::from([key1, key2]), Some(payer));
        assert_eq!(idx.payer_count(&payer), 1);

        idx.remove(&hash);
        assert!(idx.is_empty());
        assert!(idx.lookup(&key1).is_none());
        assert!(idx.lookup(&key2).is_none());
        assert_eq!(idx.payer_count(&payer), 0);
    }

    #[test]
    fn invalidated_by_returns_union() {
        let mut idx = Eip8130InvalidationIndex::new();
        let hash1 = B256::repeat_byte(0x01);
        let hash2 = B256::repeat_byte(0x02);
        let key_a = make_key(0xAA, 0x01);
        let key_b = make_key(0xAA, 0x02);

        idx.insert(hash1, HashSet::from([key_a]), None);
        idx.insert(hash2, HashSet::from([key_b]), None);

        let result = idx.invalidated_by(&[key_a, key_b]);
        assert!(result.contains(&hash1));
        assert!(result.contains(&hash2));
    }

    #[test]
    fn invalidated_by_unknown_key_returns_empty() {
        let idx = Eip8130InvalidationIndex::new();
        let key = make_key(0xFF, 0xFF);
        assert!(idx.invalidated_by(&[key]).is_empty());
    }

    #[test]
    fn prune_stale_removes_dead_entries() {
        let mut idx = Eip8130InvalidationIndex::new();
        let hash1 = B256::repeat_byte(0x01);
        let hash2 = B256::repeat_byte(0x02);
        let key = make_key(0xAA, 0x01);

        idx.insert(hash1, HashSet::from([key]), None);
        idx.insert(hash2, HashSet::from([key]), None);

        let live = HashSet::from([hash1]);
        let pruned = idx.prune_stale(&live);
        assert_eq!(pruned, 1);
        assert_eq!(idx.len(), 1);
        assert!(idx.lookup(&key).unwrap().contains(&hash1));
        assert!(!idx.lookup(&key).unwrap().contains(&hash2));
    }

    #[test]
    fn payer_count_tracking() {
        let mut idx = Eip8130InvalidationIndex::new();
        let payer = Address::repeat_byte(0xBB);

        let hash1 = B256::repeat_byte(0x01);
        let hash2 = B256::repeat_byte(0x02);
        let key = make_key(0xAA, 0x01);

        idx.insert(hash1, HashSet::from([key]), Some(payer));
        idx.insert(hash2, HashSet::from([key]), Some(payer));
        assert_eq!(idx.payer_count(&payer), 2);

        idx.remove(&hash1);
        assert_eq!(idx.payer_count(&payer), 1);

        idx.remove(&hash2);
        assert_eq!(idx.payer_count(&payer), 0);
    }

    #[test]
    fn compute_invalidation_keys_includes_nonce_and_state() {
        let sender = Address::repeat_byte(0x01);
        let nonce_key = U256::from(1);
        let keys = compute_invalidation_keys(sender, nonce_key, &[]);

        // Should have at least nonce slot + account state slot
        assert!(keys.len() >= 2);

        // Verify they reference the expected contracts
        let addresses: HashSet<Address> = keys.iter().map(|k| k.address).collect();
        assert!(addresses.contains(&xlayer_eip8130_consensus::NONCE_MANAGER_ADDRESS));
        assert!(addresses.contains(&xlayer_eip8130_consensus::ACCOUNT_CONFIG_ADDRESS));
    }
}
