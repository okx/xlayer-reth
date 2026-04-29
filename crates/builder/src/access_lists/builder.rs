use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, U256};
use reth_revm::db::states::TransitionState;
use revm::{
    primitives::{HashMap, HashSet, KECCAK_EMPTY},
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

    /// Records state transitions into the builder.
    pub fn record_transitions(
        &mut self,
        transitions: Option<&TransitionState>,
        block_access_index: u64,
    ) {
        let Some(transitions) = transitions else { return };
        if transitions.transitions.is_empty() {
            return;
        }
        for (address, transition) in &transitions.transitions {
            let account_changes = self.changes.entry(*address).or_default();

            // Update balance
            let prev_balance = transition.previous_info.as_ref().map(|i| i.balance);
            let new_balance = transition.info.as_ref().map(|i| i.balance);
            match (prev_balance, new_balance) {
                (Some(p), Some(n)) if p != n => {
                    account_changes.balance_changes.insert(block_access_index, n);
                }
                (None, Some(n)) if !n.is_zero() => {
                    account_changes.balance_changes.insert(block_access_index, n);
                }
                _ => {}
            }

            // Update nonce
            let prev_nonce = transition.previous_info.as_ref().map(|i| i.nonce);
            let new_nonce = transition.info.as_ref().map(|i| i.nonce);
            match (prev_nonce, new_nonce) {
                (Some(p), Some(n)) if p != n => {
                    account_changes.nonce_changes.insert(block_access_index, n);
                }
                (None, Some(n)) if n != 0 => {
                    account_changes.nonce_changes.insert(block_access_index, n);
                }
                _ => {}
            }

            // Update code
            let prev_code_hash = transition.previous_info.as_ref().map(|i| i.code_hash);
            let new_code_hash = transition.info.as_ref().map(|i| i.code_hash);
            let should_record_code = match (prev_code_hash, new_code_hash) {
                (Some(prev), Some(new)) => prev != new,
                (None, Some(new)) => new != KECCAK_EMPTY,
                _ => false,
            };
            if should_record_code
                && let Some(new_info) = transition.info.as_ref()
                && let Some(bytecode) = new_info.code.as_ref()
            {
                account_changes.code_changes.insert(block_access_index, bytecode.clone());
            }

            // Update storage
            for (slot, slot_change) in &transition.storage {
                if slot_change.previous_or_original_value != slot_change.present_value {
                    account_changes
                        .storage_changes
                        .entry(*slot)
                        .or_default()
                        .insert(block_access_index, slot_change.present_value);
                }
            }
        }
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
    use reth_revm::db::states::{StorageSlot, TransitionAccount};
    use revm::state::AccountInfo;

    fn assert_sorted_by_idx<T, F: Fn(&T) -> u64>(items: &[T], idx: F) {
        let indices: Vec<u64> = items.iter().map(idx).collect();
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        assert_eq!(indices, sorted, "expected ascending block_access_index ordering");
    }

    fn account_info(balance: u64, nonce: u64) -> AccountInfo {
        AccountInfo { balance: U256::from(balance), nonce, ..Default::default() }
    }

    const ADDR_PRE: Address = address!("0x000000000000000000000000000000000000beef");

    #[test]
    fn record_transitions_extracts_balance_nonce_storage() {
        let mut storage = HashMap::default();
        storage.insert(U256::from(7), StorageSlot::new_changed(U256::ZERO, U256::from(42)));
        // Unchanged slot — must NOT be recorded.
        storage.insert(U256::from(8), StorageSlot::new_changed(U256::from(1), U256::from(1)));

        let mut transitions = HashMap::default();
        transitions.insert(
            ADDR_PRE,
            TransitionAccount {
                info: Some(account_info(100, 5)),
                previous_info: Some(account_info(0, 0)),
                storage,
                ..Default::default()
            },
        );
        let ts = TransitionState { transitions };
        let mut builder = FlashblockAccessListBuilder::new();

        builder.record_transitions(Some(&ts), 0);

        let entry = builder.changes.get(&ADDR_PRE).expect("address recorded");
        assert_eq!(entry.balance_changes.get(&0), Some(&U256::from(100)));
        assert_eq!(entry.nonce_changes.get(&0), Some(&5));
        assert_eq!(
            entry.storage_changes.get(&U256::from(7)).and_then(|m| m.get(&0)),
            Some(&U256::from(42)),
        );
        assert!(!entry.storage_changes.contains_key(&U256::from(8)));
    }

    #[test]
    fn record_transitions_no_op_when_none_or_empty() {
        let mut builder = FlashblockAccessListBuilder::new();
        builder.record_transitions(None, 0);
        assert!(builder.changes.is_empty());

        let empty = TransitionState { transitions: HashMap::default() };
        builder.record_transitions(Some(&empty), 0);
        assert!(builder.changes.is_empty());
    }

    #[test]
    fn record_transitions_skips_unchanged_balance_and_nonce() {
        // Same prev/new info → nothing recorded for the field, but the address
        // entry is still created (matches existing FBALBuilderDb behavior on
        // a pure read).
        let mut transitions = HashMap::default();
        transitions.insert(
            ADDR_PRE,
            TransitionAccount {
                info: Some(account_info(100, 5)),
                previous_info: Some(account_info(100, 5)),
                storage: HashMap::default(),
                ..Default::default()
            },
        );
        let ts = TransitionState { transitions };
        let mut builder = FlashblockAccessListBuilder::new();

        builder.record_transitions(Some(&ts), 0);

        let entry = builder.changes.get(&ADDR_PRE).expect("address present");
        assert!(entry.balance_changes.is_empty());
        assert!(entry.nonce_changes.is_empty());
        assert!(entry.storage_changes.is_empty());
        assert!(entry.code_changes.is_empty());
    }

    #[test]
    fn record_transitions_records_code_clear_on_existing_account() {
        // Existing account whose code transitions to `KECCAK_EMPTY`
        // (SELFDESTRUCT-style). Must be recorded — the consumer derives
        // `bytecode_hash` from `new_code` bytes, and an empty `Bytecode`
        // re-keccaks to `KECCAK_EMPTY`.
        let prev = AccountInfo {
            balance: U256::from(50),
            nonce: 1,
            code_hash: alloy_primitives::keccak256([0x60u8]),
            code: Some(Bytecode::new_legacy(Bytes::from_static(&[0x60]))),
            ..Default::default()
        };
        let new = AccountInfo {
            balance: U256::from(50),
            nonce: 1,
            code_hash: KECCAK_EMPTY,
            code: Some(Bytecode::new()),
            ..Default::default()
        };
        let mut transitions = HashMap::default();
        transitions.insert(
            ADDR_PRE,
            TransitionAccount {
                info: Some(new),
                previous_info: Some(prev),
                storage: HashMap::default(),
                ..Default::default()
            },
        );
        let ts = TransitionState { transitions };
        let mut builder = FlashblockAccessListBuilder::new();

        builder.record_transitions(Some(&ts), 0);

        let entry = builder.changes.get(&ADDR_PRE).expect("address present");
        assert!(
            entry.code_changes.contains_key(&0),
            "code-hash transition to KECCAK_EMPTY must record"
        );
    }

    #[test]
    fn record_transitions_new_account_records_only_non_default_fields() {
        // `prev=None` (new-account branch): record only fields whose new value
        // differs from the new-account default (balance=0, nonce=0,
        // code=KECCAK_EMPTY). Mirrors `update_access_list`'s Branch B. Two
        // addresses cover both sides of every field's rule in one shot.
        const A: Address = address!("0x000000000000000000000000000000000000000a");
        const B: Address = address!("0x000000000000000000000000000000000000000b");

        let mut transitions = HashMap::default();
        transitions.insert(
            A,
            TransitionAccount {
                info: Some(account_info(100, 5)),
                previous_info: None,
                ..Default::default()
            },
        );
        transitions.insert(
            B,
            TransitionAccount {
                info: Some(account_info(0, 0)),
                previous_info: None,
                ..Default::default()
            },
        );
        let ts = TransitionState { transitions };
        let mut builder = FlashblockAccessListBuilder::new();

        builder.record_transitions(Some(&ts), 0);

        // A: non-default balance/nonce → recorded; default code → skipped.
        let a = builder.changes.get(&A).expect("A present");
        assert_eq!(a.balance_changes.get(&0), Some(&U256::from(100)));
        assert_eq!(a.nonce_changes.get(&0), Some(&5));
        assert!(a.code_changes.is_empty());

        // B: every field at the new-account default → nothing recorded.
        let b = builder.changes.get(&B).expect("B present");
        assert!(b.balance_changes.is_empty());
        assert!(b.nonce_changes.is_empty());
        assert!(b.code_changes.is_empty());
        assert!(b.storage_changes.is_empty());
    }

    #[test]
    fn pre_exec_at_index_zero_overwritten_by_tx_zero_commit_through_build() {
        // End-to-end: pre-exec records at idx=0, tx 0 then commits at the same
        // idx=0 (matches `FBALBuilderDb`'s `self.index = 0` for the first
        // sequencer tx). `HashMap::insert` overwrite means tx 0's value wins,
        // and after `AccountChangesBuilder::build` the sorted output Vec has a
        // single entry per field with tx 0's value. `.last()` (the consumer's
        // extraction) returns that value — which is the actual final post-state.
        let mut storage = HashMap::default();
        storage.insert(U256::from(7), StorageSlot::new_changed(U256::ZERO, U256::from(100)));

        let mut transitions = HashMap::default();
        transitions.insert(
            ADDR_PRE,
            TransitionAccount {
                info: Some(account_info(50, 2)),
                previous_info: Some(account_info(0, 0)),
                storage,
                ..Default::default()
            },
        );
        let ts = TransitionState { transitions };
        let mut builder = FlashblockAccessListBuilder::new();
        // 1. Pre-exec recording.
        builder.record_transitions(Some(&ts), 0);

        // 2. Tx 0 commit at the same idx=0 — directly mirroring what
        //    `FBALBuilderDb::update_access_list` does on `commit()`.
        let entry = builder.changes.get_mut(&ADDR_PRE).expect("pre-exec recorded");
        entry.balance_changes.insert(0, U256::from(999));
        entry.nonce_changes.insert(0, 7);
        entry.storage_changes.entry(U256::from(7)).or_default().insert(0, U256::from(555));

        // 3. After build(): one entry per field at idx=0, with tx 0's values.
        let entry = builder.changes.remove(&ADDR_PRE).unwrap();
        let ac = entry.build(ADDR_PRE);

        assert_eq!(ac.balance_changes.len(), 1);
        assert_eq!(ac.balance_changes.last().unwrap().block_access_index, 0);
        assert_eq!(ac.balance_changes.last().unwrap().post_balance, U256::from(999));

        assert_eq!(ac.nonce_changes.len(), 1);
        assert_eq!(ac.nonce_changes.last().unwrap().new_nonce, 7);

        assert_eq!(ac.storage_changes.len(), 1);
        let sc = &ac.storage_changes[0];
        assert_eq!(sc.slot, U256::from(7));
        assert_eq!(sc.changes.len(), 1);
        assert_eq!(sc.changes.last().unwrap().new_value, U256::from(555));
    }

    #[test]
    fn pre_exec_at_zero_and_later_tx_both_preserved_with_last_returning_latest() {
        // Pre-exec writes at idx=0, a later tx writes at idx=5 (no collision).
        // build() sorts by block_access_index ascending, both entries survive,
        // and `.last()` returns the idx=5 value — i.e. the actual final
        // post-state. Confirms pre-exec's idx=0 entry is *not* lost when a
        // later tx writes the same field.
        let mut transitions = HashMap::default();
        transitions.insert(
            ADDR_PRE,
            TransitionAccount {
                info: Some(account_info(50, 0)),
                previous_info: Some(account_info(0, 0)),
                storage: HashMap::default(),
                ..Default::default()
            },
        );
        let ts = TransitionState { transitions };
        let mut builder = FlashblockAccessListBuilder::new();
        builder.record_transitions(Some(&ts), 0);

        // Simulate a later tx (idx=5) modifying the same field.
        builder.changes.get_mut(&ADDR_PRE).unwrap().balance_changes.insert(5, U256::from(200));

        let entry = builder.changes.remove(&ADDR_PRE).unwrap();
        let ac = entry.build(ADDR_PRE);

        // Both entries survive, sorted ascending.
        assert_eq!(ac.balance_changes.len(), 2);
        assert_eq!(ac.balance_changes[0].block_access_index, 0);
        assert_eq!(ac.balance_changes[0].post_balance, U256::from(50));
        assert_eq!(ac.balance_changes[1].block_access_index, 5);
        assert_eq!(ac.balance_changes[1].post_balance, U256::from(200));
        // `.last()` returns the latest tx's value — the consumer-contract.
        assert_eq!(ac.balance_changes.last().unwrap().post_balance, U256::from(200));
    }

    #[test]
    fn balance_changes_are_sorted_by_block_access_index() {
        let mut b = AccountChangesBuilder::default();
        for tx_idx in [5u64, 2, 8, 1] {
            b.balance_changes.insert(tx_idx, U256::from(tx_idx * 10));
        }

        let out = b.build(Address::ZERO);

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
    fn flashblock_builder_preserves_per_account_change_ordering() {
        // Two accounts with out-of-order tx-index entries; the outer
        // `FlashblockAccessListBuilder` must preserve the per-account sort.
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
