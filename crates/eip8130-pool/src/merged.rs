//! Merged transaction iterator for block building.
//!
//! [`MergedBestTransactions`] interleaves ready transactions from the standard
//! OP pool (non-AA) and the EIP-8130 side pool (AA, 2D-nonce ordered), sorted
//! by effective priority fee.
//!
//! AA transactions from the standard pool are skipped because the authoritative
//! ordering for AA transactions comes from the side pool (which understands 2D
//! nonce lanes). This avoids duplicate inclusion.
//!
//! An optional **AA gas budget** limits how much block gas can be consumed by
//! AA transactions. Once the budget is exhausted, only standard transactions
//! are yielded for the remainder of the block. This prevents AA traffic from
//! crowding out standard transactions.

use std::sync::Arc;

use reth_transaction_pool::{
    error::InvalidPoolTransactionError, BestTransactions, PoolTransaction, ValidPoolTransaction,
};
use xlayer_eip8130_consensus::AA_TX_TYPE_ID;

/// Merges transactions from the standard pool and the AA side pool.
///
/// - Non-AA transactions come from `standard`.
/// - AA transactions come from `aa` (respecting 2D nonce order).
/// - AA transactions encountered in `standard` are skipped.
///
/// When both sources have a pending transaction, the one with the higher
/// effective priority fee is yielded first.
///
/// If an AA gas budget is set (via [`Self::with_aa_gas_budget`]), AA
/// transactions stop being yielded once their cumulative `gas_limit` exceeds
/// the budget.
pub struct MergedBestTransactions<T, S, A>
where
    T: PoolTransaction,
    S: BestTransactions<Item = Arc<ValidPoolTransaction<T>>>,
    A: BestTransactions<Item = Arc<ValidPoolTransaction<T>>>,
{
    standard: S,
    aa: A,
    /// Buffered non-AA transaction from the standard pool.
    pending_std: Option<Arc<ValidPoolTransaction<T>>>,
    /// Buffered AA transaction from the side pool.
    pending_aa: Option<Arc<ValidPoolTransaction<T>>>,
    /// Remaining gas budget for AA transactions. `None` means unlimited.
    aa_gas_remaining: Option<u64>,
}

impl<T, S, A> MergedBestTransactions<T, S, A>
where
    T: PoolTransaction,
    S: BestTransactions<Item = Arc<ValidPoolTransaction<T>>>,
    A: BestTransactions<Item = Arc<ValidPoolTransaction<T>>>,
{
    /// Creates a new merged iterator from the standard and AA best iterators.
    ///
    /// By default the AA gas budget is unlimited.
    pub fn new(standard: S, aa: A) -> Self {
        Self { standard, aa, pending_std: None, pending_aa: None, aa_gas_remaining: None }
    }

    /// Sets a maximum gas budget for AA transactions within this block.
    ///
    /// Once cumulative `gas_limit` of yielded AA transactions exceeds
    /// `budget`, no more AA transactions will be returned.
    pub fn with_aa_gas_budget(mut self, budget: u64) -> Self {
        self.aa_gas_remaining = Some(budget);
        self
    }

    /// Returns `true` if the AA gas budget has been exhausted.
    fn aa_budget_exhausted(&self) -> bool {
        self.aa_gas_remaining.is_some_and(|remaining| remaining == 0)
    }

    /// Deducts the gas limit of the given AA transaction from the budget.
    /// Returns `true` if the transaction fits within the remaining budget.
    fn charge_aa_gas(&mut self, tx: &ValidPoolTransaction<T>) -> bool {
        if let Some(remaining) = self.aa_gas_remaining.as_mut() {
            let gas = tx.transaction.gas_limit();
            if gas > *remaining {
                *remaining = 0;
                return false;
            }
            *remaining -= gas;
        }
        true
    }

    /// Advances the standard iterator, skipping AA transactions (type 0x7B).
    fn advance_standard(&mut self) -> Option<Arc<ValidPoolTransaction<T>>> {
        loop {
            let tx = self.standard.next()?;
            if tx.transaction.ty() == AA_TX_TYPE_ID {
                continue;
            }
            return Some(tx);
        }
    }

    /// Advances the AA iterator, returning `None` if the budget is exhausted.
    fn advance_aa(&mut self) -> Option<Arc<ValidPoolTransaction<T>>> {
        if self.aa_budget_exhausted() {
            return None;
        }
        self.aa.next()
    }
}

impl<T, S, A> Iterator for MergedBestTransactions<T, S, A>
where
    T: PoolTransaction,
    S: BestTransactions<Item = Arc<ValidPoolTransaction<T>>>,
    A: BestTransactions<Item = Arc<ValidPoolTransaction<T>>>,
{
    type Item = Arc<ValidPoolTransaction<T>>;

    fn next(&mut self) -> Option<Self::Item> {
        // Fill pending slots if empty.
        if self.pending_std.is_none() {
            self.pending_std = self.advance_standard();
        }
        if self.pending_aa.is_none() {
            self.pending_aa = self.advance_aa();
        }

        match (self.pending_std.take(), self.pending_aa.take()) {
            (Some(std_tx), Some(aa_tx)) => {
                // Check if the AA transaction fits in the gas budget.
                if !self.charge_aa_gas(&aa_tx) {
                    // Budget exhausted — yield standard, discard AA.
                    return Some(std_tx);
                }

                let std_prio = std_tx.transaction.max_priority_fee_per_gas().unwrap_or_default();
                let aa_prio = aa_tx.transaction.max_priority_fee_per_gas().unwrap_or_default();

                if aa_prio >= std_prio {
                    // Yield AA transaction, buffer the standard one.
                    self.pending_std = Some(std_tx);
                    Some(aa_tx)
                } else {
                    // Yield standard transaction, buffer the AA one.
                    self.pending_aa = Some(aa_tx);
                    Some(std_tx)
                }
            }
            (Some(std_tx), None) => Some(std_tx),
            (None, Some(aa_tx)) => {
                if self.charge_aa_gas(&aa_tx) {
                    Some(aa_tx)
                } else {
                    None
                }
            }
            (None, None) => None,
        }
    }
}

impl<T, S, A> BestTransactions for MergedBestTransactions<T, S, A>
where
    T: PoolTransaction,
    S: BestTransactions<Item = Arc<ValidPoolTransaction<T>>>,
    A: BestTransactions<Item = Arc<ValidPoolTransaction<T>>>,
{
    fn mark_invalid(&mut self, transaction: &Self::Item, kind: &InvalidPoolTransactionError) {
        if transaction.transaction.ty() == AA_TX_TYPE_ID {
            self.aa.mark_invalid(transaction, kind);
            // Clear pending AA if it was the one invalidated.
            if self.pending_aa.as_ref().is_some_and(|t| t.hash() == transaction.hash()) {
                self.pending_aa = None;
            }
        } else {
            self.standard.mark_invalid(transaction, kind);
            if self.pending_std.as_ref().is_some_and(|t| t.hash() == transaction.hash()) {
                self.pending_std = None;
            }
        }
    }

    fn no_updates(&mut self) {
        self.standard.no_updates();
        self.aa.no_updates();
    }

    fn skip_blobs(&mut self) {
        self.standard.skip_blobs();
    }

    fn set_skip_blobs(&mut self, skip: bool) {
        self.standard.set_skip_blobs(skip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::Transaction;
    use reth_transaction_pool::PoolTransaction;

    use crate::test_utils::{MockAaTransaction, MockAaTransactionFactory};

    /// A trivial `BestTransactions` that yields from a `Vec`.
    struct VecBest<T: PoolTransaction> {
        txs: std::collections::VecDeque<Arc<ValidPoolTransaction<T>>>,
    }

    impl<T: PoolTransaction> VecBest<T> {
        fn new(txs: Vec<Arc<ValidPoolTransaction<T>>>) -> Self {
            Self { txs: txs.into() }
        }
    }

    impl<T: PoolTransaction + Send + Sync> Iterator for VecBest<T> {
        type Item = Arc<ValidPoolTransaction<T>>;
        fn next(&mut self) -> Option<Self::Item> {
            self.txs.pop_front()
        }
    }

    impl<T: PoolTransaction + Send + Sync> BestTransactions for VecBest<T> {
        fn mark_invalid(&mut self, _tx: &Self::Item, _kind: &InvalidPoolTransactionError) {}
        fn no_updates(&mut self) {}
        fn skip_blobs(&mut self) {}
        fn set_skip_blobs(&mut self, _skip: bool) {}
    }

    #[test]
    fn yields_both_standard_and_aa_transactions() {
        let mut f = MockAaTransactionFactory::default();
        let std_tx = f.create_standard(10); // priority fee 10
        let aa_tx = f.create_aa(5); // priority fee 5

        let standard = VecBest::new(vec![std_tx.clone()]);
        let aa = VecBest::new(vec![aa_tx.clone()]);

        let merged: Vec<_> = MergedBestTransactions::new(standard, aa).collect();
        assert_eq!(merged.len(), 2);
        // Higher priority first
        assert_eq!(merged[0].hash(), std_tx.hash());
        assert_eq!(merged[1].hash(), aa_tx.hash());
    }

    #[test]
    fn aa_from_standard_pool_is_skipped() {
        let mut f = MockAaTransactionFactory::default();
        let std_tx = f.create_standard(10);
        let aa_in_std = f.create_aa(8); // AA tx in standard pool (should be skipped)
        let aa_from_side = f.create_aa(8); // same-priority AA tx from side pool

        // Standard pool has both standard and AA (dual placement)
        let standard = VecBest::new(vec![std_tx.clone(), aa_in_std]);
        // AA pool has the same AA tx
        let aa = VecBest::new(vec![aa_from_side.clone()]);

        let merged: Vec<_> = MergedBestTransactions::new(standard, aa).collect();
        assert_eq!(merged.len(), 2);
        // Standard first (higher priority), then AA from side pool
        assert_eq!(merged[0].hash(), std_tx.hash());
        assert_eq!(merged[1].hash(), aa_from_side.hash());
    }

    #[test]
    fn empty_aa_pool_yields_only_standard() {
        let mut f = MockAaTransactionFactory::default();
        let std1 = f.create_standard(10);
        let std2 = f.create_standard(5);

        let standard = VecBest::new(vec![std1.clone(), std2.clone()]);
        let aa = VecBest::<MockAaTransaction>::new(vec![]);

        let merged: Vec<_> = MergedBestTransactions::new(standard, aa).collect();
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn empty_standard_pool_yields_only_aa() {
        let mut f = MockAaTransactionFactory::default();
        let aa1 = f.create_aa(10);
        let aa2 = f.create_aa(5);

        let standard = VecBest::<MockAaTransaction>::new(vec![]);
        let aa = VecBest::new(vec![aa1.clone(), aa2.clone()]);

        let merged: Vec<_> = MergedBestTransactions::new(standard, aa).collect();
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn both_empty_yields_none() {
        let standard = VecBest::<MockAaTransaction>::new(vec![]);
        let aa = VecBest::<MockAaTransaction>::new(vec![]);

        let merged: Vec<_> = MergedBestTransactions::new(standard, aa).collect();
        assert!(merged.is_empty());
    }

    #[test]
    fn priority_fee_ordering() {
        let mut f = MockAaTransactionFactory::default();
        // AA has higher priority
        let aa_high = f.create_aa(20);
        let std_mid = f.create_standard(15);
        let aa_low = f.create_aa(10);
        let std_low = f.create_standard(5);

        let standard = VecBest::new(vec![std_mid.clone(), std_low.clone()]);
        let aa = VecBest::new(vec![aa_high.clone(), aa_low.clone()]);

        let merged: Vec<_> = MergedBestTransactions::new(standard, aa).collect();
        assert_eq!(merged.len(), 4);
        // Should be ordered by priority fee
        let prios: Vec<u128> = merged
            .iter()
            .map(|tx| tx.transaction.max_priority_fee_per_gas().unwrap_or_default())
            .collect();
        assert!(prios.windows(2).all(|w| w[0] >= w[1]), "prios not descending: {prios:?}");
    }

    // ── AA gas budget tests ─────────────────────────────────────────

    #[test]
    fn aa_gas_budget_limits_aa_transactions() {
        let mut f = MockAaTransactionFactory::default();
        // Each tx has gas_limit = 21_000
        let aa1 = f.create_aa(20);
        let aa2 = f.create_aa(15);
        let std1 = f.create_standard(10);

        let standard = VecBest::new(vec![std1.clone()]);
        let aa = VecBest::new(vec![aa1.clone(), aa2.clone()]);

        // Budget allows exactly one AA transaction (21_000).
        let merged: Vec<_> =
            MergedBestTransactions::new(standard, aa).with_aa_gas_budget(21_000).collect();

        // Should yield: aa1 (uses 21k, budget = 0), std1
        // aa2 is dropped because budget is exhausted.
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].hash(), aa1.hash());
        assert_eq!(merged[1].hash(), std1.hash());
    }

    #[test]
    fn zero_aa_gas_budget_yields_only_standard() {
        let mut f = MockAaTransactionFactory::default();
        let aa1 = f.create_aa(20);
        let std1 = f.create_standard(10);

        let standard = VecBest::new(vec![std1.clone()]);
        let aa = VecBest::new(vec![aa1.clone()]);

        let merged: Vec<_> =
            MergedBestTransactions::new(standard, aa).with_aa_gas_budget(0).collect();

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].hash(), std1.hash());
    }

    #[test]
    fn unlimited_budget_yields_all_aa() {
        let mut f = MockAaTransactionFactory::default();
        let aa1 = f.create_aa(20);
        let aa2 = f.create_aa(15);
        let aa3 = f.create_aa(10);

        let standard = VecBest::<MockAaTransaction>::new(vec![]);
        let aa = VecBest::new(vec![aa1.clone(), aa2.clone(), aa3.clone()]);

        // No gas budget set (default unlimited).
        let merged: Vec<_> = MergedBestTransactions::new(standard, aa).collect();
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn aa_gas_budget_does_not_affect_standard() {
        let mut f = MockAaTransactionFactory::default();
        let std1 = f.create_standard(20);
        let std2 = f.create_standard(15);
        let std3 = f.create_standard(10);

        let standard = VecBest::new(vec![std1.clone(), std2.clone(), std3.clone()]);
        let aa = VecBest::<MockAaTransaction>::new(vec![]);

        // Even with zero AA budget, all standard txs are yielded.
        let merged: Vec<_> =
            MergedBestTransactions::new(standard, aa).with_aa_gas_budget(0).collect();
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn aa_budget_partial_allows_some_aa() {
        let mut f = MockAaTransactionFactory::default();
        // Each tx has gas_limit = 21_000
        let aa1 = f.create_aa(20);
        let aa2 = f.create_aa(15);
        let aa3 = f.create_aa(10);
        let std1 = f.create_standard(5);

        let standard = VecBest::new(vec![std1.clone()]);
        let aa = VecBest::new(vec![aa1.clone(), aa2.clone(), aa3.clone()]);

        // Budget allows exactly two AA transactions (42_000).
        let merged: Vec<_> =
            MergedBestTransactions::new(standard, aa).with_aa_gas_budget(42_000).collect();

        // aa1 (21k), aa2 (21k), std1 — aa3 dropped.
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].hash(), aa1.hash());
        assert_eq!(merged[1].hash(), aa2.hash());
        assert_eq!(merged[2].hash(), std1.hash());
    }
}
