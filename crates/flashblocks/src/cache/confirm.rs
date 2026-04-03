use crate::CachedTxInfo;
use eyre::eyre;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use alloy_consensus::transaction::TxHashRef;
use alloy_primitives::{TxHash, B256};

use reth_chain_state::ExecutedBlock;
use reth_primitives_traits::{BlockBody, NodePrimitives, ReceiptTy};
use reth_rpc_eth_types::block::BlockAndReceipts;

const DEFAULT_CONFIRM_BLOCK_CACHE_SIZE: usize = 50;
const DEFAULT_TX_CACHE_SIZE: usize = DEFAULT_CONFIRM_BLOCK_CACHE_SIZE * 10_000;

#[derive(Debug)]
pub(crate) struct ConfirmedBlock<N: NodePrimitives> {
    /// The locally built pending block with execution output.
    pub(crate) executed_block: ExecutedBlock<N>,
    /// The receipts for the pending block
    pub(crate) receipts: Arc<Vec<ReceiptTy<N>>>,
}

impl<N: NodePrimitives> ConfirmedBlock<N> {
    /// Returns a pair of [`RecoveredBlock`] and a vector of  [`NodePrimitives::Receipt`]s by
    /// cloning from borrowed self.
    pub(crate) fn to_block_and_receipts(&self) -> BlockAndReceipts<N> {
        BlockAndReceipts {
            block: self.executed_block.recovered_block.clone(),
            receipts: self.receipts.clone(),
        }
    }
}

/// Confirmed flashblocks sequence cache that is ahead of the current node's canonical
/// chainstate. We optimistically commit confirmed flashblocks sequences to the cache
/// and flush them when the canonical chainstate catches up.
///
/// Block data is stored in a `BTreeMap` keyed by block number, enabling O(log n)
/// range splits in [`flush_up_to`](Self::flush_up_to).
/// A secondary `HashMap` provides O(1) block hash to block number reverse lookups.
///
/// Transaction data is stored in a `HashMap` which indexes transaction hashes to
/// [`CachedTxInfo`] for O(1) tx/receipt lookups.
#[derive(Debug)]
pub(crate) struct ConfirmCache<N: NodePrimitives> {
    /// Primary storage: block number → (block hash, block + receipts).
    /// `BTreeMap` ordering enables efficient range-based flush via `split_off`.
    blocks: BTreeMap<u64, (B256, ConfirmedBlock<N>)>,
    /// Reverse index: block hash → block number for O(1) hash-based lookups.
    hash_to_number: HashMap<B256, u64>,
    /// Transaction index: tx hash → cached tx info for O(1) tx/receipt lookups.
    tx_index: HashMap<TxHash, CachedTxInfo<N>>,
}

impl<N: NodePrimitives> Default for ConfirmCache<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: NodePrimitives> ConfirmCache<N> {
    /// Creates a new [`ConfirmCache`].
    pub(crate) fn new() -> Self {
        Self {
            blocks: BTreeMap::new(),
            hash_to_number: HashMap::with_capacity(DEFAULT_CONFIRM_BLOCK_CACHE_SIZE),
            tx_index: HashMap::with_capacity(DEFAULT_TX_CACHE_SIZE),
        }
    }

    /// Inserts a confirmed block into the cache, indexed by block number and block hash.
    pub(crate) fn insert(
        &mut self,
        height: u64,
        executed_block: ExecutedBlock<N>,
        receipts: Arc<Vec<ReceiptTy<N>>>,
    ) -> eyre::Result<()> {
        if self.blocks.len() >= DEFAULT_CONFIRM_BLOCK_CACHE_SIZE
            && !self.blocks.contains_key(&height)
        {
            return Err(eyre!(
                "confirm cache at max capacity ({DEFAULT_CONFIRM_BLOCK_CACHE_SIZE}), cannot insert block: {height}"
            ));
        }
        if let Some((old_hash, old_block)) = self.blocks.remove(&height) {
            // Clean up old entries at this height if exist
            self.hash_to_number.remove(&old_hash);
            self.remove_tx_index_for_block(&old_block);
        }

        // Build tx index entries for all transactions in this block
        let hash = executed_block.recovered_block.hash();
        let txs = executed_block.recovered_block.body().transactions();
        for (idx, (tx, receipt)) in txs.iter().zip(receipts.as_ref().iter()).enumerate() {
            let tx_hash = *tx.tx_hash();
            self.tx_index.insert(
                tx_hash,
                CachedTxInfo {
                    block_number: height,
                    block_hash: hash,
                    tx_index: idx as u64,
                    tx: tx.clone(),
                    receipt: receipt.clone(),
                },
            );
        }
        // Build block index entries for block data
        self.hash_to_number.insert(hash, height);
        self.blocks.insert(height, (hash, ConfirmedBlock { executed_block, receipts }));
        Ok(())
    }

    /// Clears all entries.
    pub(crate) fn clear(&mut self) {
        self.tx_index.clear();
        self.blocks.clear();
        self.hash_to_number.clear();
    }

    /// Returns the block number for the given block hash, if cached.
    pub(crate) fn number_for_hash(&self, block_hash: &B256) -> Option<u64> {
        self.hash_to_number.get(block_hash).copied()
    }

    /// Returns the confirmed block for the given block hash, if present.
    pub(crate) fn get_block_by_hash(&self, block_hash: &B256) -> Option<BlockAndReceipts<N>> {
        self.get_block_by_number(self.number_for_hash(block_hash)?)
    }

    /// Returns the confirmed block for the given block number, if present.
    pub(crate) fn get_block_by_number(&self, block_number: u64) -> Option<BlockAndReceipts<N>> {
        self.blocks.get(&block_number).map(|(_, entry)| entry.to_block_and_receipts())
    }

    /// Returns the cached transaction info for the given tx hash, if present.
    pub(crate) fn get_tx_info(
        &self,
        tx_hash: &TxHash,
    ) -> Option<(CachedTxInfo<N>, BlockAndReceipts<N>)> {
        let tx_info = self.tx_index.get(tx_hash).cloned()?;
        let block = self.get_block_by_number(tx_info.block_number)?;
        Some((tx_info, block))
    }

    /// Returns all `ExecutedBlock`s in the cache up to and including `target_height`,
    /// ordered newest to oldest (for use with `MemoryOverlayStateProvider`).
    ///
    /// Returns an error if state cache pollution detected (non-contiguous blocks).
    pub(crate) fn get_executed_blocks_up_to_height(
        &self,
        target_height: u64,
        canon_height: u64,
    ) -> eyre::Result<Vec<ExecutedBlock<N>>> {
        // Validation checks
        let entries: Vec<_> = self.blocks.range(..=target_height).collect();
        if !entries.is_empty() {
            // Verify lowest overlay block must be at most `canon_height + 1` to ensure
            // no gap between canonical state and the overlay
            let lowest = *entries[0].0;
            if lowest > canon_height + 1 {
                return Err(eyre!(
                    "gap between canonical height {canon_height} and lowest overlay block {lowest}"
                ));
            }
            // Verify contiguity
            for window in entries.windows(2) {
                let (a, _) = window[0];
                let (b, _) = window[1];
                if *b != *a + 1 {
                    return Err(eyre!(
                        "non-contiguous confirm cache: gap between blocks {a} and {b}"
                    ));
                }
            }
        }
        Ok(entries
            .into_iter()
            .rev()
            .map(|(_, (_, confirmed))| confirmed.executed_block.clone())
            .collect())
    }

    /// Returns the `ExecutedBlock` for the given block hash, if present.
    pub(crate) fn get_executed_block_by_hash(&self, block_hash: &B256) -> Option<ExecutedBlock<N>> {
        self.get_executed_block_by_number(self.number_for_hash(block_hash)?)
    }

    /// Returns the `ExecutedBlock` for the given block number, if present.
    pub(crate) fn get_executed_block_by_number(
        &self,
        block_number: u64,
    ) -> Option<ExecutedBlock<N>> {
        self.blocks.get(&block_number).map(|(_, entry)| entry.executed_block.clone())
    }

    /// Removes all tx index entries for the transactions in the given block.
    fn remove_tx_index_for_block(&mut self, block: &ConfirmedBlock<N>) {
        for tx in block.executed_block.recovered_block.body().transactions() {
            self.tx_index.remove(tx.tx_hash());
        }
    }

    /// Flushes all entries with block number <= `canonical_number`.
    ///
    /// Called when the canonical chain catches up to the confirmed cache. Returns
    /// the number of entries flushed.
    pub(crate) fn flush_up_to_height(&mut self, canon_height: u64) -> usize {
        let retained = self.blocks.split_off(&(canon_height + 1));
        let stale = std::mem::replace(&mut self.blocks, retained);
        let count = stale.len();
        for (hash, bar) in stale.into_values() {
            self.hash_to_number.remove(&hash);
            self.remove_tx_index_for_block(&bar);
        }
        count
    }

    /// Returns the number of cached entries.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns `true` if the cache is empty.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{empty_receipts, make_executed_block, make_executed_block_with_txs};
    use alloy_consensus::BlockHeader;
    use reth_optimism_primitives::OpPrimitives;

    #[test]
    fn test_confirm_cache_new_is_empty() {
        let cache = ConfirmCache::<OpPrimitives>::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_confirm_cache_insert_single_block_increases_len() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block = make_executed_block(1, B256::ZERO);
        cache.insert(1, block, empty_receipts()).expect("insert should succeed");
        assert_eq!(cache.len(), 1);
        assert!(!cache.is_empty());
    }

    #[test]
    fn test_confirm_cache_insert_fails_at_max_capacity() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let mut parent = B256::ZERO;
        for height in 1..=(DEFAULT_CONFIRM_BLOCK_CACHE_SIZE as u64) {
            let block = make_executed_block(height, parent);
            let hash = block.recovered_block.hash();
            cache.insert(height, block, empty_receipts()).expect("insert within capacity");
            parent = hash;
        }
        let overflow = make_executed_block(DEFAULT_CONFIRM_BLOCK_CACHE_SIZE as u64 + 1, parent);
        let result =
            cache.insert(DEFAULT_CONFIRM_BLOCK_CACHE_SIZE as u64 + 1, overflow, empty_receipts());
        assert!(result.is_err());
    }

    #[test]
    fn test_confirm_cache_get_block_by_number_returns_correct_block() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block = make_executed_block(42, B256::ZERO);
        cache.insert(42, block, empty_receipts()).expect("insert");
        let result = cache.get_block_by_number(42);
        assert!(result.is_some());
        assert_eq!(result.unwrap().block.number(), 42);
    }

    #[test]
    fn test_confirm_cache_get_block_by_number_returns_none_for_wrong_number() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block = make_executed_block(42, B256::ZERO);
        cache.insert(42, block, empty_receipts()).expect("insert");
        assert!(cache.get_block_by_number(43).is_none());
        assert!(cache.get_block_by_number(0).is_none());
    }

    #[test]
    fn test_confirm_cache_get_block_by_hash_returns_correct_block() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block = make_executed_block(42, B256::ZERO);
        let block_hash = block.recovered_block.hash();
        cache.insert(42, block, empty_receipts()).expect("insert");
        let result = cache.get_block_by_hash(&block_hash);
        assert!(result.is_some());
        assert_eq!(result.unwrap().block.number(), 42);
    }

    #[test]
    fn test_confirm_cache_get_block_by_hash_returns_none_for_unknown_hash() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block = make_executed_block(42, B256::ZERO);
        cache.insert(42, block, empty_receipts()).expect("insert");
        assert!(cache.get_block_by_hash(&B256::repeat_byte(0xFF)).is_none());
    }

    #[test]
    fn test_confirm_cache_number_for_hash_returns_correct_mapping() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block = make_executed_block(10, B256::ZERO);
        let hash = block.recovered_block.hash();
        cache.insert(10, block, empty_receipts()).expect("insert");
        assert_eq!(cache.number_for_hash(&hash), Some(10));
    }

    #[test]
    fn test_confirm_cache_clear_removes_all_entries() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block = make_executed_block(1, B256::ZERO);
        cache.insert(1, block, empty_receipts()).expect("insert");
        cache.clear();
        assert!(cache.is_empty());
        assert!(cache.get_block_by_number(1).is_none());
    }

    #[test]
    fn test_confirm_cache_flush_up_to_height_removes_entries_at_or_below_height() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let mut parent = B256::ZERO;
        for height in 1..=5 {
            let block = make_executed_block(height, parent);
            parent = block.recovered_block.hash();
            cache.insert(height, block, empty_receipts()).expect("insert");
        }
        let count = cache.flush_up_to_height(3);
        assert_eq!(count, 3);
        assert_eq!(cache.len(), 2);
        assert!(cache.get_block_by_number(3).is_none());
        assert!(cache.get_block_by_number(4).is_some());
        assert!(cache.get_block_by_number(5).is_some());
    }

    #[test]
    fn test_confirm_cache_flush_up_to_height_higher_than_all_removes_all() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let mut parent = B256::ZERO;
        for height in 1..=3 {
            let block = make_executed_block(height, parent);
            parent = block.recovered_block.hash();
            cache.insert(height, block, empty_receipts()).expect("insert");
        }
        let count = cache.flush_up_to_height(100);
        assert_eq!(count, 3);
        assert!(cache.is_empty());
    }

    #[test]
    fn test_confirm_cache_flush_up_to_height_zero_removes_nothing() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block = make_executed_block(1, B256::ZERO);
        cache.insert(1, block, empty_receipts()).expect("insert");
        let count = cache.flush_up_to_height(0);
        assert_eq!(count, 0);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_confirm_cache_flush_removes_hash_indices_for_all_flushed_blocks() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let mut parent = B256::ZERO;
        let mut hashes = vec![];
        for height in 1..=3 {
            let block = make_executed_block(height, parent);
            let hash = block.recovered_block.hash();
            hashes.push(hash);
            cache.insert(height, block, empty_receipts()).expect("insert");
            parent = hash;
        }
        cache.flush_up_to_height(2);
        assert!(cache.number_for_hash(&hashes[0]).is_none());
        assert!(cache.number_for_hash(&hashes[1]).is_none());
        assert!(cache.number_for_hash(&hashes[2]).is_some());
    }

    #[test]
    fn test_confirm_cache_get_executed_blocks_up_to_height_returns_contiguous_blocks_newest_first()
    {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block2 = make_executed_block(2, B256::repeat_byte(0x01));
        let block3 = make_executed_block(3, block2.recovered_block.hash());
        let block4 = make_executed_block(4, block3.recovered_block.hash());
        cache.insert(2, block2, empty_receipts()).expect("insert 2");
        cache.insert(3, block3, empty_receipts()).expect("insert 3");
        cache.insert(4, block4, empty_receipts()).expect("insert 4");
        let blocks = cache.get_executed_blocks_up_to_height(4, 1).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].recovered_block.number(), 4);
        assert_eq!(blocks[1].recovered_block.number(), 3);
        assert_eq!(blocks[2].recovered_block.number(), 2);
    }

    #[test]
    fn test_confirm_cache_get_executed_blocks_up_to_height_returns_empty_on_empty_cache() {
        let cache = ConfirmCache::<OpPrimitives>::new();
        let result = cache.get_executed_blocks_up_to_height(5, 1);
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_confirm_cache_get_executed_blocks_detects_gap_between_canonical_and_overlay() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block3 = make_executed_block(3, B256::ZERO);
        cache.insert(3, block3, empty_receipts()).expect("insert 3");
        assert!(cache.get_executed_blocks_up_to_height(3, 1).is_err());
    }

    #[test]
    fn test_confirm_cache_get_executed_blocks_detects_non_contiguous_overlay() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block2 = make_executed_block(2, B256::repeat_byte(0x01));
        let block4 = make_executed_block(4, B256::repeat_byte(0x03));
        cache.insert(2, block2, empty_receipts()).expect("insert 2");
        cache.insert(4, block4, empty_receipts()).expect("insert 4");
        assert!(cache.get_executed_blocks_up_to_height(4, 1).is_err());
    }

    #[test]
    fn test_confirm_cache_get_executed_blocks_allows_redundant_overlap_with_canonical() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block2 = make_executed_block(2, B256::repeat_byte(0x01));
        let block3 = make_executed_block(3, block2.recovered_block.hash());
        cache.insert(2, block2, empty_receipts()).expect("insert 2");
        cache.insert(3, block3, empty_receipts()).expect("insert 3");
        let blocks = cache.get_executed_blocks_up_to_height(3, 2).unwrap();
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_confirm_cache_get_executed_blocks_single_block_contiguous_with_canonical() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block5 = make_executed_block(5, B256::repeat_byte(0x04));
        cache.insert(5, block5, empty_receipts()).expect("insert 5");
        let blocks = cache.get_executed_blocks_up_to_height(5, 4).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].recovered_block.number(), 5);
    }

    #[test]
    fn test_confirm_cache_get_executed_blocks_returns_subset_up_to_target() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block2 = make_executed_block(2, B256::repeat_byte(0x01));
        let block3 = make_executed_block(3, block2.recovered_block.hash());
        let block4 = make_executed_block(4, block3.recovered_block.hash());
        let block5 = make_executed_block(5, block4.recovered_block.hash());
        cache.insert(2, block2, empty_receipts()).expect("insert 2");
        cache.insert(3, block3, empty_receipts()).expect("insert 3");
        cache.insert(4, block4, empty_receipts()).expect("insert 4");
        cache.insert(5, block5, empty_receipts()).expect("insert 5");
        let blocks = cache.get_executed_blocks_up_to_height(3, 1).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].recovered_block.number(), 3);
        assert_eq!(blocks[1].recovered_block.number(), 2);
    }

    #[test]
    fn test_confirm_cache_insert_same_height_twice_keeps_cache_len_at_one() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block_a = make_executed_block(10, B256::ZERO);
        let block_b = make_executed_block(10, B256::repeat_byte(0xFF));
        cache.insert(10, block_a, empty_receipts()).expect("first insert");
        cache.insert(10, block_b, empty_receipts()).expect("second insert");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_confirm_cache_get_tx_info_returns_none_for_unknown_hash() {
        let cache = ConfirmCache::<OpPrimitives>::new();
        assert!(cache.get_tx_info(&B256::repeat_byte(0xAA)).is_none());
    }

    #[test]
    fn test_confirm_cache_insert_builds_tx_index_correctly() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let (block, receipts) = make_executed_block_with_txs(1, B256::ZERO, 0, 3);
        let block_hash = block.recovered_block.hash();
        let tx_hashes: Vec<_> =
            block.recovered_block.body().transactions().map(|tx| tx.tx_hash()).collect();
        cache.insert(1, block, receipts).expect("insert");

        for (i, tx_hash) in tx_hashes.iter().enumerate() {
            let (info, bar) = cache.get_tx_info(tx_hash).expect("tx should be in tx_index");
            assert_eq!(info.block_number, 1);
            assert_eq!(info.block_hash, block_hash);
            assert_eq!(info.tx_index, i as u64);
            assert_eq!(bar.block.number(), 1);
        }
    }

    #[test]
    fn test_confirm_cache_flush_cleans_tx_index() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let (block, receipts) = make_executed_block_with_txs(1, B256::ZERO, 0, 2);
        let tx_hashes: Vec<_> =
            block.recovered_block.body().transactions().map(|tx| tx.tx_hash()).collect();
        cache.insert(1, block, receipts).expect("insert");

        cache.flush_up_to_height(1);
        for tx_hash in tx_hashes.iter() {
            assert!(cache.get_tx_info(tx_hash).is_none());
        }
    }

    #[test]
    fn test_confirm_cache_insert_duplicate_height_cleans_stale_indexes() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let block_a = make_executed_block(10, B256::ZERO);
        let hash_a = block_a.recovered_block.hash();
        let block_b = make_executed_block(10, B256::repeat_byte(0xFF));
        let hash_b = block_b.recovered_block.hash();

        cache.insert(10, block_a, empty_receipts()).expect("first insert");
        cache.insert(10, block_b, empty_receipts()).expect("second insert");

        assert_eq!(cache.number_for_hash(&hash_b), Some(10));
        // Stale hash_to_number entry is cleaned up on overwrite.
        assert_eq!(
            cache.number_for_hash(&hash_a),
            None,
            "stale hash_to_number entry should be removed on duplicate height insert"
        );
    }

    #[test]
    fn test_confirm_cache_insert_duplicate_height_retains_shared_tx_entries() {
        // Two blocks at the same height share a transaction (same nonce → same hash).
        // After replacing, the shared tx must still be present in the index.
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        // block_a has txs with nonces [0, 1]
        let (block_a, receipts_a) = make_executed_block_with_txs(10, B256::ZERO, 0, 2);
        let shared_tx_hash: TxHash =
            (*block_a.recovered_block.body().transactions().next().unwrap().tx_hash()).into();
        // block_b has txs with nonces [0, 2] — nonce 0 is shared with block_a
        let (block_b, receipts_b) = make_executed_block_with_txs(10, B256::repeat_byte(0xFF), 0, 2);
        let block_b_tx_hashes: Vec<TxHash> = block_b
            .recovered_block
            .body()
            .transactions()
            .map(|tx| (*tx.tx_hash()).into())
            .collect();

        cache.insert(10, block_a, receipts_a).expect("first insert");
        assert!(cache.get_tx_info(&shared_tx_hash).is_some());

        cache.insert(10, block_b, receipts_b).expect("second insert");
        // The shared tx (nonce 0) must still be in the index, pointing to block_b
        let info = cache.get_tx_info(&shared_tx_hash);
        assert!(info.is_some(), "shared tx should be retained after replacement");
        // All block_b txs should be present
        for tx_hash in &block_b_tx_hashes {
            assert!(cache.get_tx_info(tx_hash).is_some(), "block_b tx should be in index");
        }
    }

    #[test]
    fn test_confirm_cache_flush_cleans_tx_index_for_partial_flush() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let (block1, receipts1) = make_executed_block_with_txs(1, B256::ZERO, 0, 2);
        let tx_hashes_1: Vec<_> =
            block1.recovered_block.body().transactions().map(|tx| tx.tx_hash()).collect();
        let parent = block1.recovered_block.hash();
        cache.insert(1, block1, receipts1).expect("insert 1");

        let (block2, receipts2) = make_executed_block_with_txs(2, parent, 100, 2);
        let tx_hashes_2: Vec<_> =
            block2.recovered_block.body().transactions().map(|tx| tx.tx_hash()).collect();
        cache.insert(2, block2, receipts2).expect("insert 2");

        cache.flush_up_to_height(1);
        for tx_hash in tx_hashes_1.iter() {
            assert!(cache.get_tx_info(tx_hash).is_none(), "block 1 tx should be gone");
        }
        for tx_hash in tx_hashes_2.iter() {
            assert!(cache.get_tx_info(tx_hash).is_some(), "block 2 tx should remain");
        }
    }

    #[test]
    fn test_confirm_cache_clear_cleans_tx_index() {
        let mut cache = ConfirmCache::<OpPrimitives>::new();
        let (block, receipts) = make_executed_block_with_txs(1, B256::ZERO, 0, 2);
        let tx_hashes: Vec<_> =
            block.recovered_block.body().transactions().map(|tx| tx.tx_hash()).collect();
        cache.insert(1, block, receipts).expect("insert");

        cache.clear();
        for tx_hash in tx_hashes.iter() {
            assert!(cache.get_tx_info(tx_hash).is_none());
        }
    }
}
