//! Heavily influenced by [reth](https://github.com/paradigmxyz/reth/blob/1e965caf5fa176f244a31c0d2662ba1b590938db/crates/optimism/payload/src/builder.rs#L570)
use alloy_primitives::{Address, U256};
use derive_more::Display;
use op_revm::OpTransactionError;
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_revm::state::bal::Bal;

#[derive(Debug, Display)]
pub enum TxnExecutionResult {
    TransactionDALimitExceeded,
    #[display("BlockDALimitExceeded: total_da_used={_0} tx_da_size={_1} block_da_limit={_2}")]
    BlockDALimitExceeded(u64, u64, u64),
    #[display("TransactionGasLimitExceeded: total_gas_used={_0} tx_gas_limit={_1}")]
    TransactionGasLimitExceeded(u64, u64, u64),
    SequencerTransaction,
    NonceTooLow,
    InteropFailed,
    #[display("InternalError({_0})")]
    InternalError(OpTransactionError),
    EvmError,
    Success,
    Reverted,
    RevertedAndExcluded,
    MaxGasUsageExceeded,
}

#[derive(Default, Debug)]
pub struct ExecutionInfo {
    /// All executed transactions (unrecovered).
    pub executed_transactions: Vec<OpTransactionSigned>,
    /// The recovered senders for the executed transactions.
    pub executed_senders: Vec<Address>,
    /// The transaction receipts
    pub receipts: Vec<OpReceipt>,
    /// All gas used so far
    pub cumulative_gas_used: u64,
    /// Estimated DA size
    pub cumulative_da_bytes_used: u64,
    /// Tracks fees from executed mempool transactions
    pub total_fees: U256,
    /// DA Footprint Scalar for Jovian
    pub da_footprint_scalar: Option<u16>,
    /// Optional blob fields for payload validation
    pub optional_blob_fields: Option<(Option<u64>, Option<u64>)>,
    /// Accumulated block access list
    pub cumulative_bal: Bal,
}

impl ExecutionInfo {
    /// Create a new instance with allocated slots.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            executed_transactions: Vec::with_capacity(capacity),
            executed_senders: Vec::with_capacity(capacity),
            receipts: Vec::with_capacity(capacity),
            cumulative_gas_used: 0,
            cumulative_da_bytes_used: 0,
            total_fees: U256::ZERO,
            da_footprint_scalar: None,
            optional_blob_fields: None,
            cumulative_bal: Bal::new(),
        }
    }

    /// Returns true if the transaction would exceed the block limits:
    /// - block gas limit: ensures the transaction still fits into the block.
    /// - tx DA limit: if configured, ensures the tx does not exceed the maximum allowed DA limit
    ///   per tx.
    /// - block DA limit: if configured, ensures the transaction's DA size does not exceed the
    ///   maximum allowed DA limit per block.
    #[allow(clippy::too_many_arguments)]
    pub fn is_tx_over_limits(
        &self,
        tx_da_size: u64,
        block_gas_limit: u64,
        tx_data_limit: Option<u64>,
        block_data_limit: Option<u64>,
        tx_gas_limit: u64,
        da_footprint_gas_scalar: Option<u16>,
        block_da_footprint_limit: Option<u64>,
    ) -> Result<(), TxnExecutionResult> {
        if tx_data_limit.is_some_and(|da_limit| tx_da_size > da_limit) {
            return Err(TxnExecutionResult::TransactionDALimitExceeded);
        }
        let total_da_bytes_used = self.cumulative_da_bytes_used.saturating_add(tx_da_size);
        if block_data_limit.is_some_and(|da_limit| total_da_bytes_used > da_limit) {
            return Err(TxnExecutionResult::BlockDALimitExceeded(
                self.cumulative_da_bytes_used,
                tx_da_size,
                block_data_limit.unwrap_or_default(),
            ));
        }

        // Post Jovian: the tx DA footprint must be less than the block gas limit
        if let Some(da_footprint_gas_scalar) = da_footprint_gas_scalar {
            let tx_da_footprint =
                total_da_bytes_used.saturating_mul(da_footprint_gas_scalar as u64);
            if tx_da_footprint > block_da_footprint_limit.unwrap_or(block_gas_limit) {
                return Err(TxnExecutionResult::BlockDALimitExceeded(
                    total_da_bytes_used,
                    tx_da_size,
                    tx_da_footprint,
                ));
            }
        }

        if self.cumulative_gas_used + tx_gas_limit > block_gas_limit {
            return Err(TxnExecutionResult::TransactionGasLimitExceeded(
                self.cumulative_gas_used,
                tx_gas_limit,
                block_gas_limit,
            ));
        }
        Ok(())
    }

    /// Merges a single flashblock's access list into the accumulated block access list.
    pub fn merge_access_list(&mut self, fbal: Option<Bal>) {
        let Some(bal) = fbal else { return };
        for (addr, incoming) in bal.accounts {
            match self.cumulative_bal.accounts.get_mut(&addr) {
                Some(existing) => {
                    existing.account_info.extend(incoming.account_info);
                    existing.storage.extend(incoming.storage);
                }
                None => {
                    self.cumulative_bal.accounts.insert(addr, incoming);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eip7928::{
        AccountChanges, BalanceChange, BlockAccessList, CodeChange, NonceChange, SlotChanges,
        StorageChange,
    };
    use alloy_primitives::{address, Bytes};
    use reth_revm::state::bal::AccountBal;

    const ADDR_A: Address = address!("0000000000000000000000000000000000000001");
    const ADDR_B: Address = address!("0000000000000000000000000000000000000002");

    type SlotChangeSpec = (U256, Vec<(u64, U256)>);

    fn make_account_changes(
        addr: Address,
        balance_changes: Vec<(u64, u64)>,
        storage_reads: Vec<u64>,
    ) -> AccountChanges {
        let mut bc: Vec<BalanceChange> = balance_changes
            .into_iter()
            .map(|(tx_idx, bal)| BalanceChange {
                block_access_index: tx_idx,
                post_balance: U256::from(bal),
            })
            .collect();
        bc.sort_unstable_by_key(|c| c.block_access_index);

        AccountChanges {
            address: addr,
            balance_changes: bc,
            storage_reads: storage_reads.into_iter().map(U256::from).collect(),
            ..Default::default()
        }
    }

    fn make_account_changes_full(
        addr: Address,
        balance_changes: Vec<(u64, U256)>,
        nonce_changes: Vec<(u64, u64)>,
        storage_changes: Vec<SlotChangeSpec>,
    ) -> AccountChanges {
        let mut bc: Vec<BalanceChange> = balance_changes
            .into_iter()
            .map(|(tx_idx, val)| BalanceChange { block_access_index: tx_idx, post_balance: val })
            .collect();
        bc.sort_unstable_by_key(|c| c.block_access_index);

        let mut nc: Vec<NonceChange> = nonce_changes
            .into_iter()
            .map(|(tx_idx, val)| NonceChange { block_access_index: tx_idx, new_nonce: val })
            .collect();
        nc.sort_unstable_by_key(|c| c.block_access_index);

        let storage_changes: Vec<SlotChanges> = storage_changes
            .into_iter()
            .map(|(slot, changes)| {
                let mut changes: Vec<StorageChange> = changes
                    .into_iter()
                    .map(|(tx_idx, val)| StorageChange {
                        block_access_index: tx_idx,
                        new_value: val,
                    })
                    .collect();
                changes.sort_unstable_by_key(|c| c.block_access_index);
                SlotChanges { slot, changes }
            })
            .collect();

        AccountChanges {
            address: addr,
            balance_changes: bc,
            nonce_changes: nc,
            storage_changes,
            ..Default::default()
        }
    }

    fn alloy_to_revm(list: BlockAccessList) -> Bal {
        list.into_iter()
            .map(|ac| AccountBal::try_from_alloy(ac).expect("valid alloy account changes"))
            .collect()
    }

    fn merge_alloy(info: &mut ExecutionInfo, list: BlockAccessList) {
        info.merge_access_list(Some(alloy_to_revm(list)));
    }

    fn account_in(list: &BlockAccessList, addr: Address) -> &AccountChanges {
        list.iter().find(|ac| ac.address == addr).expect("address present")
    }

    #[test]
    fn merge_none_is_noop() {
        let mut info = ExecutionInfo::with_capacity(0);
        info.merge_access_list(None);
        assert!(info.cumulative_bal.accounts.is_empty());
    }

    #[test]
    fn merge_empty_bal_is_noop() {
        let mut info = ExecutionInfo::with_capacity(0);
        info.merge_access_list(Some(Bal::new()));
        assert!(info.cumulative_bal.accounts.is_empty());
    }

    #[test]
    fn merge_single_flashblock_populates_cumulative() {
        let mut info = ExecutionInfo::with_capacity(0);
        let ac = make_account_changes(ADDR_A, vec![(0, 100)], vec![]);
        merge_alloy(&mut info, vec![ac]);

        let out = info.cumulative_bal.into_alloy_bal();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].address, ADDR_A);
        assert_eq!(out[0].balance_changes.len(), 1);
        assert_eq!(out[0].balance_changes[0].block_access_index, 0);
        assert_eq!(out[0].balance_changes[0].post_balance, U256::from(100));
    }

    #[test]
    fn merge_across_flashblocks_same_address_extends_balance() {
        let mut info = ExecutionInfo::with_capacity(0);
        merge_alloy(&mut info, vec![make_account_changes(ADDR_A, vec![(0, 100)], vec![])]);
        merge_alloy(
            &mut info,
            vec![
                make_account_changes(ADDR_A, vec![(5, 200)], vec![]),
                make_account_changes(ADDR_B, vec![(6, 300)], vec![]),
            ],
        );

        let out = info.cumulative_bal.into_alloy_bal();
        assert_eq!(out.len(), 2, "ADDR_A merged into one entry, ADDR_B separate");
        assert_eq!(out[0].address, ADDR_A);
        assert_eq!(out[1].address, ADDR_B);

        let a_changes: Vec<(u64, U256)> =
            out[0].balance_changes.iter().map(|c| (c.block_access_index, c.post_balance)).collect();
        assert_eq!(a_changes.len(), 2);
        assert!(a_changes.contains(&(0, U256::from(100))));
        assert!(a_changes.contains(&(5, U256::from(200))));

        assert_eq!(out[1].balance_changes.len(), 1);
        assert_eq!(out[1].balance_changes[0].block_access_index, 6);
        assert_eq!(out[1].balance_changes[0].post_balance, U256::from(300));
    }

    #[test]
    fn merge_dedupes_storage_reads() {
        let mut info = ExecutionInfo::with_capacity(0);
        merge_alloy(&mut info, vec![make_account_changes(ADDR_A, vec![], vec![1, 2])]);
        merge_alloy(&mut info, vec![make_account_changes(ADDR_A, vec![], vec![1, 3])]);

        let out = info.cumulative_bal.into_alloy_bal();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].storage_reads,
            vec![U256::from(1), U256::from(2), U256::from(3)],
            "duplicate slot 1 collapsed; result sorted ascending",
        );
    }

    #[test]
    fn merge_last_balance_change_resolves_across_three_flashblocks() {
        let mut info = ExecutionInfo::with_capacity(0);
        let mk = |bal: Vec<(u64, U256)>| make_account_changes_full(ADDR_A, bal, vec![], vec![]);
        merge_alloy(&mut info, vec![mk(vec![(2, U256::from(20)), (5, U256::from(50))])]);
        merge_alloy(&mut info, vec![mk(vec![(8, U256::from(80)), (11, U256::from(110))])]);
        merge_alloy(&mut info, vec![mk(vec![(15, U256::from(150)), (20, U256::from(200))])]);

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        assert_eq!(a.balance_changes.last().unwrap().post_balance, U256::from(200));
    }

    #[test]
    fn merge_last_nonce_change_resolves_across_three_flashblocks() {
        let mut info = ExecutionInfo::with_capacity(0);
        let mk = |n: Vec<(u64, u64)>| make_account_changes_full(ADDR_A, vec![], n, vec![]);
        merge_alloy(&mut info, vec![mk(vec![(1, 1), (3, 2)])]);
        merge_alloy(&mut info, vec![mk(vec![(7, 3), (9, 4)])]);
        merge_alloy(&mut info, vec![mk(vec![(12, 5), (18, 6)])]);

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        assert_eq!(a.nonce_changes.last().unwrap().new_nonce, 6);
    }

    #[test]
    fn merge_same_slot_modified_in_every_flashblock_resolves_to_latest_write() {
        let mut info = ExecutionInfo::with_capacity(0);
        let slot = U256::from(0x1);
        let mk = |c: Vec<(u64, U256)>| {
            make_account_changes_full(ADDR_A, vec![], vec![], vec![(slot, c)])
        };
        merge_alloy(&mut info, vec![mk(vec![(2, U256::from(200)), (4, U256::from(400))])]);
        merge_alloy(&mut info, vec![mk(vec![(8, U256::from(800)), (10, U256::from(1000))])]);
        merge_alloy(&mut info, vec![mk(vec![(15, U256::from(1500)), (20, U256::from(2000))])]);

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        assert_eq!(a.storage_changes.len(), 1, "duplicate slots are deduped on aggregation");
        let sc = &a.storage_changes[0];
        assert_eq!(sc.slot, slot);
        assert_eq!(sc.changes.len(), 6, "all 6 writes flattened into one sorted vec");
        assert_eq!(sc.changes.last().unwrap().new_value, U256::from(2000));
    }

    #[test]
    fn merge_slot_modified_with_gap_resolves_to_latest_touching_flashblock() {
        let mut info = ExecutionInfo::with_capacity(0);
        let slot = U256::from(0x1);
        merge_alloy(
            &mut info,
            vec![make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(slot, vec![(3, U256::from(100))])],
            )],
        );
        merge_alloy(
            &mut info,
            vec![make_account_changes_full(ADDR_A, vec![(7, U256::from(7777))], vec![], vec![])],
        );
        merge_alloy(
            &mut info,
            vec![make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(slot, vec![(13, U256::from(999))])],
            )],
        );

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        let sc = a.storage_changes.iter().find(|sc| sc.slot == slot).expect("slot present");
        assert_eq!(sc.changes.last().unwrap().new_value, U256::from(999));
        assert_eq!(a.balance_changes.last().unwrap().post_balance, U256::from(7777));
    }

    #[test]
    fn merge_multi_slot_each_resolves_independently_across_flashblocks() {
        let mut info = ExecutionInfo::with_capacity(0);
        let (s1, s2, s3) = (U256::from(0x1), U256::from(0x2), U256::from(0x3));
        let mk = |c: Vec<SlotChangeSpec>| make_account_changes_full(ADDR_A, vec![], vec![], c);
        merge_alloy(
            &mut info,
            vec![mk(vec![(s1, vec![(2, U256::from(11))]), (s3, vec![(4, U256::from(33))])])],
        );
        merge_alloy(
            &mut info,
            vec![mk(vec![
                (s2, vec![(6, U256::from(60)), (8, U256::from(80))]),
                (s3, vec![(7, U256::from(70))]),
            ])],
        );
        merge_alloy(
            &mut info,
            vec![mk(vec![(s1, vec![(14, U256::from(140))]), (s3, vec![(12, U256::from(120))])])],
        );

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        let final_for = |slot| {
            a.storage_changes
                .iter()
                .find(|sc| sc.slot == slot)
                .and_then(|sc| sc.changes.last())
                .map(|c| c.new_value)
        };
        assert_eq!(final_for(s1), Some(U256::from(140)));
        assert_eq!(final_for(s2), Some(U256::from(80)));
        assert_eq!(final_for(s3), Some(U256::from(120)));
    }

    #[test]
    fn merge_read_then_write_across_flashblocks_promotes_to_changes() {
        let mut info = ExecutionInfo::with_capacity(0);
        let slot = U256::from(0x42);
        merge_alloy(&mut info, vec![make_account_changes(ADDR_A, vec![], vec![0x42])]);
        merge_alloy(
            &mut info,
            vec![make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(slot, vec![(5, U256::from(100))])],
            )],
        );

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        assert!(a.storage_reads.is_empty(), "later write must promote slot out of storage_reads");
        assert_eq!(a.storage_changes.len(), 1);
        let sc = &a.storage_changes[0];
        assert_eq!(sc.slot, slot);
        assert_eq!(sc.changes.len(), 1);
        assert_eq!(sc.changes[0].block_access_index, 5);
        assert_eq!(sc.changes[0].new_value, U256::from(100));
    }

    #[test]
    fn merge_write_then_read_across_flashblocks_stays_as_changes() {
        let mut info = ExecutionInfo::with_capacity(0);
        let slot = U256::from(0x99);
        merge_alloy(
            &mut info,
            vec![make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(slot, vec![(3, U256::from(777))])],
            )],
        );
        merge_alloy(&mut info, vec![make_account_changes(ADDR_A, vec![], vec![0x99])]);

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        assert!(
            a.storage_reads.is_empty(),
            "slot already has writes — must not appear as bare read"
        );
        assert_eq!(a.storage_changes.len(), 1);
        let sc = &a.storage_changes[0];
        assert_eq!(sc.slot, slot);
        assert_eq!(sc.changes.len(), 1, "FB0 write must survive across the FB1 read");
        assert_eq!(sc.changes[0].block_access_index, 3);
        assert_eq!(sc.changes[0].new_value, U256::from(777));
    }

    #[test]
    fn merge_code_changes_across_flashblocks_with_last_extraction() {
        let mut info = ExecutionInfo::with_capacity(0);
        let code_v1 = Bytes::from(vec![0x60, 0x00]);
        let code_v2 = Bytes::from(vec![0x61, 0xff, 0xff]);

        let mk = |bai: u64, code: Bytes| AccountChanges {
            address: ADDR_A,
            code_changes: vec![CodeChange { block_access_index: bai, new_code: code }],
            ..Default::default()
        };

        merge_alloy(&mut info, vec![mk(2, code_v1.clone())]);
        merge_alloy(&mut info, vec![mk(8, code_v2.clone())]);

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        assert_eq!(a.code_changes.len(), 2);
        assert_eq!(a.code_changes[0].block_access_index, 2);
        assert_eq!(a.code_changes[0].new_code, code_v1);
        assert_eq!(a.code_changes[1].block_access_index, 8);
        assert_eq!(a.code_changes[1].new_code, code_v2);
        assert_eq!(a.code_changes.last().unwrap().new_code, code_v2);
    }

    #[test]
    fn merge_output_is_sorted_by_address() {
        let mut info = ExecutionInfo::with_capacity(0);
        let addr_z = address!("00000000000000000000000000000000000000ff");
        let addr_m = address!("0000000000000000000000000000000000000080");
        let addr_a = address!("0000000000000000000000000000000000000001");

        merge_alloy(&mut info, vec![make_account_changes(addr_z, vec![(1, 1)], vec![])]);
        merge_alloy(&mut info, vec![make_account_changes(addr_a, vec![(2, 2)], vec![])]);
        merge_alloy(&mut info, vec![make_account_changes(addr_m, vec![(3, 3)], vec![])]);

        let out = info.cumulative_bal.into_alloy_bal();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].address, addr_a);
        assert_eq!(out[1].address, addr_m);
        assert_eq!(out[2].address, addr_z);
    }

    #[test]
    fn merge_preserves_bal_index_ordering_per_slot() {
        let mut info = ExecutionInfo::with_capacity(0);
        let slot = U256::from(0xab);
        let mk = |writes: Vec<(u64, U256)>| {
            make_account_changes_full(ADDR_A, vec![], vec![], vec![(slot, writes)])
        };
        merge_alloy(&mut info, vec![mk(vec![(1, U256::from(11)), (3, U256::from(33))])]);
        merge_alloy(&mut info, vec![mk(vec![(5, U256::from(55)), (9, U256::from(99))])]);
        merge_alloy(&mut info, vec![mk(vec![(12, U256::from(120)), (20, U256::from(200))])]);

        let out = info.cumulative_bal.into_alloy_bal();
        let sc = &account_in(&out, ADDR_A).storage_changes[0];
        let indices: Vec<u64> = sc.changes.iter().map(|c| c.block_access_index).collect();
        assert_eq!(
            indices,
            vec![1, 3, 5, 9, 12, 20],
            "writes must remain in non-decreasing bal_index order across flashblock merges",
        );
    }

    // EIP-7928 §"BlockAccessList canonical form" — focused compliance pins.

    #[test]
    fn eip7928_storage_changes_lex_sorted_by_slot() {
        let mut info = ExecutionInfo::with_capacity(0);
        let s_high = U256::from(0xff);
        let s_mid = U256::from(0x80);
        let s_low = U256::from(0x01);
        let mk = |slot: U256, idx: u64, val: u64| {
            make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(slot, vec![(idx, U256::from(val))])],
            )
        };
        merge_alloy(&mut info, vec![mk(s_high, 1, 10)]);
        merge_alloy(&mut info, vec![mk(s_low, 2, 20)]);
        merge_alloy(&mut info, vec![mk(s_mid, 3, 30)]);

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        let slots: Vec<U256> = a.storage_changes.iter().map(|sc| sc.slot).collect();
        assert_eq!(slots, vec![s_low, s_mid, s_high], "storage_changes must be lex-sorted by slot");
    }

    #[test]
    fn eip7928_balance_changes_ascending_index_within_account() {
        let mut info = ExecutionInfo::with_capacity(0);
        let mk = |bc: Vec<(u64, U256)>| make_account_changes_full(ADDR_A, bc, vec![], vec![]);
        merge_alloy(&mut info, vec![mk(vec![(1, U256::from(1)), (4, U256::from(4))])]);
        merge_alloy(&mut info, vec![mk(vec![(7, U256::from(7))])]);
        merge_alloy(&mut info, vec![mk(vec![(11, U256::from(11)), (15, U256::from(15))])]);

        let out = info.cumulative_bal.into_alloy_bal();
        let indices: Vec<u64> =
            account_in(&out, ADDR_A).balance_changes.iter().map(|c| c.block_access_index).collect();
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        assert_eq!(indices, sorted, "balance_changes must be ascending by block_access_index");
    }

    #[test]
    fn eip7928_no_duplicate_addresses_in_output() {
        let mut info = ExecutionInfo::with_capacity(0);
        for idx in 0..5 {
            merge_alloy(
                &mut info,
                vec![
                    make_account_changes(ADDR_A, vec![(idx, idx * 10)], vec![]),
                    make_account_changes(ADDR_B, vec![(idx, idx * 100)], vec![]),
                ],
            );
        }

        let out = info.cumulative_bal.into_alloy_bal();
        let mut addrs: Vec<Address> = out.iter().map(|ac| ac.address).collect();
        let total = addrs.len();
        addrs.sort_unstable();
        addrs.dedup();
        assert_eq!(total, addrs.len(), "each address must appear exactly once in the output");
        assert_eq!(total, 2);
    }

    #[test]
    fn eip7928_storage_changes_and_storage_reads_are_disjoint_per_account() {
        let mut info = ExecutionInfo::with_capacity(0);
        let written_slot = U256::from(0x10);
        let read_slot = U256::from(0x20);

        merge_alloy(
            &mut info,
            vec![make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(written_slot, vec![(2, U256::from(99))])],
            )],
        );
        merge_alloy(&mut info, vec![make_account_changes(ADDR_A, vec![], vec![0x20])]);
        merge_alloy(&mut info, vec![make_account_changes(ADDR_A, vec![], vec![0x10])]);

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);
        let change_slots: std::collections::HashSet<U256> =
            a.storage_changes.iter().map(|sc| sc.slot).collect();
        let read_slots: std::collections::HashSet<U256> = a.storage_reads.iter().copied().collect();
        assert!(
            change_slots.intersection(&read_slots).next().is_none(),
            "storage_changes ∩ storage_reads must be empty per EIP-7928",
        );
        assert!(change_slots.contains(&written_slot));
        assert!(read_slots.contains(&read_slot));
    }

    #[test]
    fn eip7928_no_duplicate_block_access_index_in_change_lists() {
        // Across non-overlapping flashblock index ranges (the builder invariant), no two
        // entries can share a `block_access_index`.
        let mut info = ExecutionInfo::with_capacity(0);
        let mk_balance =
            |bc: Vec<(u64, U256)>| make_account_changes_full(ADDR_A, bc, vec![], vec![]);
        merge_alloy(&mut info, vec![mk_balance(vec![(1, U256::from(1)), (2, U256::from(2))])]);
        merge_alloy(&mut info, vec![mk_balance(vec![(3, U256::from(3)), (4, U256::from(4))])]);

        let slot = U256::from(0xff);
        let mk_storage = |sc: Vec<(u64, U256)>| {
            make_account_changes_full(ADDR_A, vec![], vec![], vec![(slot, sc)])
        };
        merge_alloy(&mut info, vec![mk_storage(vec![(5, U256::from(50))])]);
        merge_alloy(&mut info, vec![mk_storage(vec![(6, U256::from(60))])]);

        let out = info.cumulative_bal.into_alloy_bal();
        let a = account_in(&out, ADDR_A);

        let bal_indices: Vec<u64> =
            a.balance_changes.iter().map(|c| c.block_access_index).collect();
        let mut bal_unique = bal_indices.clone();
        bal_unique.sort_unstable();
        bal_unique.dedup();
        assert_eq!(bal_indices.len(), bal_unique.len(), "no duplicate index in balance_changes");

        let sc = &a.storage_changes[0];
        let slot_indices: Vec<u64> = sc.changes.iter().map(|c| c.block_access_index).collect();
        let mut slot_unique = slot_indices.clone();
        slot_unique.sort_unstable();
        slot_unique.dedup();
        assert_eq!(slot_indices.len(), slot_unique.len(), "no duplicate index per slot");
    }

    #[test]
    fn eip7928_empty_change_lists_preserved_for_touched_accounts() {
        // Account touched only via reads must still surface in the output with all change lists
        // empty, satisfying "accounts with no state changes MUST still be present".
        let mut info = ExecutionInfo::with_capacity(0);
        merge_alloy(&mut info, vec![make_account_changes(ADDR_A, vec![], vec![1, 2, 3])]);

        let out = info.cumulative_bal.into_alloy_bal();
        assert_eq!(out.len(), 1);
        let a = &out[0];
        assert_eq!(a.address, ADDR_A);
        assert!(a.balance_changes.is_empty());
        assert!(a.nonce_changes.is_empty());
        assert!(a.code_changes.is_empty());
        assert!(a.storage_changes.is_empty());
        assert_eq!(a.storage_reads, vec![U256::from(1), U256::from(2), U256::from(3)]);
    }
}
