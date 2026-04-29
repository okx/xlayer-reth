use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, U256};
use revm::{
    primitives::{HashMap, HashSet},
    state::Bytecode,
};

use crate::access_lists::FlashblockAccessList;

/// A builder type for [`FlashblockAccessList`]
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct FlashblockAccessListBuilder {
    /// Mapping from Address -> [`AccountChangesBuilder`]
    pub changes: HashMap<Address, AccountChangesBuilder>,
}

impl FlashblockAccessListBuilder {
    /// Creates a new [`FlashblockAccessListBuilder`]
    pub fn new() -> Self {
        Self { changes: Default::default() }
    }

    /// Consumes the builder and produces a [`FlashblockAccessList`]
    pub fn build(self, min_tx_index: u64, max_tx_index: u64) -> FlashblockAccessList {
        let mut changes: Vec<_> = self.changes.into_iter().map(|(k, v)| v.build(k)).collect();
        changes.sort_unstable_by_key(|a| a.address);

        FlashblockAccessList::build(changes, min_tx_index, max_tx_index)
    }
}

/// A builder type for [`AccountChanges`]
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct AccountChangesBuilder {
    /// Mapping from Storage Slot -> (Transaction Index -> New Value)
    pub storage_changes: HashMap<U256, HashMap<u64, U256>>,
    /// Set of storage slots
    pub storage_reads: HashSet<U256>,
    /// Mapping from Transaction Index -> New Balance
    pub balance_changes: HashMap<u64, U256>,
    /// Mapping from Transaction Index -> New Nonce
    pub nonce_changes: HashMap<u64, u64>,
    /// Mapping from Transaction Index -> New Code
    pub code_changes: HashMap<u64, Bytecode>,
}

impl AccountChangesBuilder {
    /// Consumes the builder and produces [`AccountChanges`]
    pub fn build(self, address: Address) -> AccountChanges {
        let mut balance_changes: Vec<_> = self
            .balance_changes
            .into_iter()
            .map(|(tx_idx, val)| BalanceChange { block_access_index: tx_idx, post_balance: val })
            .collect();
        balance_changes.sort_unstable_by_key(|c| c.block_access_index);

        let mut nonce_changes: Vec<_> = self
            .nonce_changes
            .into_iter()
            .map(|(tx_idx, val)| NonceChange { block_access_index: tx_idx, new_nonce: val })
            .collect();
        nonce_changes.sort_unstable_by_key(|c| c.block_access_index);

        let mut code_changes: Vec<_> = self
            .code_changes
            .into_iter()
            .map(|(tx_idx, bc)| CodeChange {
                block_access_index: tx_idx,
                new_code: bc.original_bytes(),
            })
            .collect();
        code_changes.sort_unstable_by_key(|c| c.block_access_index);

        AccountChanges {
            address,
            storage_changes: self
                .storage_changes
                .into_iter()
                .map(|(slot, sc)| {
                    let mut changes: Vec<_> = sc
                        .into_iter()
                        .map(|(tx_idx, val)| StorageChange {
                            block_access_index: tx_idx,
                            new_value: val,
                        })
                        .collect();
                    changes.sort_unstable_by_key(|c| c.block_access_index);
                    SlotChanges { slot, changes }
                })
                .collect(),
            storage_reads: self.storage_reads.into_iter().collect(),
            balance_changes,
            nonce_changes,
            code_changes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Bytes};

    fn assert_sorted_by_idx<T, F: Fn(&T) -> u64>(items: &[T], idx: F) {
        let indices: Vec<u64> = items.iter().map(idx).collect();
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        assert_eq!(indices, sorted, "expected ascending block_access_index ordering");
    }

    #[test]
    fn balance_changes_are_sorted_by_block_access_index() {
        let mut b = AccountChangesBuilder::default();
        for tx_idx in [5u64, 2, 8, 1] {
            b.balance_changes.insert(tx_idx, U256::from(tx_idx * 10));
        }

        let out = b.build(Address::ZERO);

        assert_eq!(out.balance_changes.len(), 4);
        assert_sorted_by_idx(&out.balance_changes, |c| c.block_access_index);
        // EIP-7928 consumer contract: `.last()` returns the entry with the
        // largest block_access_index.
        let last = out.balance_changes.last().unwrap();
        assert_eq!(last.block_access_index, 8);
        assert_eq!(last.post_balance, U256::from(80));
    }

    #[test]
    fn nonce_changes_are_sorted_by_block_access_index() {
        let mut b = AccountChangesBuilder::default();
        for (tx_idx, nonce) in [(5u64, 7u64), (2, 4), (8, 9), (1, 3)] {
            b.nonce_changes.insert(tx_idx, nonce);
        }

        let out = b.build(Address::ZERO);

        assert_sorted_by_idx(&out.nonce_changes, |c| c.block_access_index);
        let last = out.nonce_changes.last().unwrap();
        assert_eq!(last.block_access_index, 8);
        assert_eq!(last.new_nonce, 9);
    }

    #[test]
    fn code_changes_are_sorted_by_block_access_index() {
        let mut b = AccountChangesBuilder::default();
        let bc_2 = Bytecode::new_legacy(Bytes::from_static(&[0x60]));
        let bc_5 = Bytecode::new_legacy(Bytes::from_static(&[0x61]));
        let bc_8 = Bytecode::new_legacy(Bytes::from_static(&[0x62]));
        b.code_changes.insert(5, bc_5);
        b.code_changes.insert(2, bc_2);
        b.code_changes.insert(8, bc_8.clone());

        let out = b.build(Address::ZERO);

        assert_sorted_by_idx(&out.code_changes, |c| c.block_access_index);
        let last = out.code_changes.last().unwrap();
        assert_eq!(last.block_access_index, 8);
        assert_eq!(last.new_code, bc_8.original_bytes());
    }

    #[test]
    fn per_slot_storage_changes_are_sorted_by_block_access_index() {
        let mut b = AccountChangesBuilder::default();
        let slot = U256::from(42);
        let inner = b.storage_changes.entry(slot).or_default();
        // Insert in deliberately unordered fashion.
        for tx_idx in [9u64, 3, 12, 1, 7] {
            inner.insert(tx_idx, U256::from(tx_idx * 100));
        }

        let out = b.build(Address::ZERO);

        assert_eq!(out.storage_changes.len(), 1);
        let sc = &out.storage_changes[0];
        assert_eq!(sc.slot, slot);
        assert_sorted_by_idx(&sc.changes, |c| c.block_access_index);
        let last = sc.changes.last().unwrap();
        assert_eq!(last.block_access_index, 12);
        assert_eq!(last.new_value, U256::from(1200));
    }

    #[test]
    fn multi_slot_each_slot_independently_sorted() {
        let mut b = AccountChangesBuilder::default();
        let slot_a = U256::from(1);
        let slot_b = U256::from(2);
        let inner_a = b.storage_changes.entry(slot_a).or_default();
        for tx_idx in [4u64, 2, 6] {
            inner_a.insert(tx_idx, U256::from(tx_idx));
        }
        let inner_b = b.storage_changes.entry(slot_b).or_default();
        for tx_idx in [10u64, 5, 8, 3] {
            inner_b.insert(tx_idx, U256::from(tx_idx * 1000));
        }

        let out = b.build(Address::ZERO);

        assert_eq!(out.storage_changes.len(), 2);
        for sc in &out.storage_changes {
            assert_sorted_by_idx(&sc.changes, |c| c.block_access_index);
        }
        let by_slot: HashMap<U256, &SlotChanges> =
            out.storage_changes.iter().map(|sc| (sc.slot, sc)).collect();
        assert_eq!(by_slot[&slot_a].changes.last().unwrap().block_access_index, 6);
        assert_eq!(by_slot[&slot_b].changes.last().unwrap().block_access_index, 10);
    }

    #[test]
    fn flashblock_builder_round_trip_preserves_per_account_ordering() {
        // Two accounts, each with multiple out-of-order tx-index entries;
        // verify that aggregating through `FlashblockAccessListBuilder` still
        // yields sorted per-tx-index vectors per account.
        let addr_a = address!("0x1111111111111111111111111111111111111111");
        let addr_b = address!("0x2222222222222222222222222222222222222222");

        let mut top = FlashblockAccessListBuilder::new();

        let acc_a = top.changes.entry(addr_a).or_default();
        for (tx_idx, bal) in [(7u64, 70u64), (1, 10), (4, 40)] {
            acc_a.balance_changes.insert(tx_idx, U256::from(bal));
        }

        let acc_b = top.changes.entry(addr_b).or_default();
        for (tx_idx, nonce) in [(5u64, 5u64), (2, 2), (9, 9)] {
            acc_b.nonce_changes.insert(tx_idx, nonce);
        }

        let fal = top.build(0, 9);
        let by_addr: HashMap<Address, &AccountChanges> =
            fal.account_changes.iter().map(|ac| (ac.address, ac)).collect();

        let a = by_addr[&addr_a];
        assert_sorted_by_idx(&a.balance_changes, |c| c.block_access_index);
        assert_eq!(a.balance_changes.last().unwrap().block_access_index, 7);
        assert_eq!(a.balance_changes.last().unwrap().post_balance, U256::from(70));

        let b = by_addr[&addr_b];
        assert_sorted_by_idx(&b.nonce_changes, |c| c.block_access_index);
        assert_eq!(b.nonce_changes.last().unwrap().block_access_index, 9);
        assert_eq!(b.nonce_changes.last().unwrap().new_nonce, 9);
    }
}
