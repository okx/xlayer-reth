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

const DEFAULT_CONFIRM_BLOCK_CACHE_SIZE: usize = 1_000;
const DEFAULT_TX_CACHE_SIZE: usize = DEFAULT_CONFIRM_BLOCK_CACHE_SIZE * 10_000;

#[derive(Debug)]
pub struct ConfirmedBlock<N: NodePrimitives> {
    /// The locally built pending block with execution output.
    pub executed_block: ExecutedBlock<N>,
    /// The receipts for the pending block
    pub receipts: Arc<Vec<ReceiptTy<N>>>,
}

impl<N: NodePrimitives> ConfirmedBlock<N> {
    /// Returns a pair of [`RecoveredBlock`] and a vector of  [`NodePrimitives::Receipt`]s by
    /// cloning from borrowed self.
    pub fn to_block_and_receipts(&self) -> BlockAndReceipts<N> {
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
pub struct ConfirmCache<N: NodePrimitives> {
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
    pub fn new() -> Self {
        Self {
            blocks: BTreeMap::new(),
            hash_to_number: HashMap::with_capacity(DEFAULT_CONFIRM_BLOCK_CACHE_SIZE),
            tx_index: HashMap::with_capacity(DEFAULT_TX_CACHE_SIZE),
        }
    }

    /// Returns the number of cached entries.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns `true` if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Inserts a confirmed block into the cache, indexed by both block number
    /// and block hash.
    ///
    /// This is a raw insert with no reorg detection — callers are responsible
    /// for flushing invalidated entries via [`flush_from`](Self::flush_from)
    /// before inserting if a reorg is detected.
    ///
    /// Returns an error if the cache is at max capacity.
    pub fn insert(
        &mut self,
        height: u64,
        executed_block: ExecutedBlock<N>,
        receipts: Arc<Vec<ReceiptTy<N>>>,
    ) -> eyre::Result<()> {
        if self.blocks.len() >= DEFAULT_CONFIRM_BLOCK_CACHE_SIZE {
            return Err(eyre!(
                "confirm cache at max capacity ({DEFAULT_CONFIRM_BLOCK_CACHE_SIZE}), cannot insert block: {height}"
            ));
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
    pub fn clear(&mut self) {
        self.tx_index.clear();
        self.blocks.clear();
        self.hash_to_number.clear();
    }

    /// Returns the block number for the given block hash, if cached.
    pub fn number_for_hash(&self, block_hash: &B256) -> Option<u64> {
        self.hash_to_number.get(block_hash).copied()
    }

    /// Returns the block hash for the given block number, if cached.
    pub fn hash_for_number(&self, block_number: u64) -> Option<B256> {
        self.blocks.get(&block_number).map(|(hash, _)| *hash)
    }

    /// Returns the confirmed block for the given block hash, if present.
    pub fn get_block_by_hash(&self, block_hash: &B256) -> Option<BlockAndReceipts<N>> {
        self.get_block_by_number(self.number_for_hash(block_hash)?)
    }

    /// Returns the confirmed block for the given block number, if present.
    pub fn get_block_by_number(&self, block_number: u64) -> Option<BlockAndReceipts<N>> {
        self.blocks.get(&block_number).map(|(_, entry)| entry.to_block_and_receipts())
    }

    /// Returns the cached transaction info for the given tx hash, if present.
    /// Returns the cached transaction info for the given tx hash, if present.
    pub fn get_tx_info(&self, tx_hash: &TxHash) -> Option<(CachedTxInfo<N>, BlockAndReceipts<N>)> {
        let tx_info = self.tx_index.get(tx_hash).cloned()?;
        let block = self.get_block_by_number(tx_info.block_number)?;
        Some((tx_info, block))
    }

    /// Returns `true` if the cache contains a block with the given hash.
    pub fn contains_hash(&self, block_hash: &B256) -> bool {
        self.hash_to_number.contains_key(block_hash)
    }

    /// Returns `true` if the cache contains a block with the given number.
    pub fn contains_number(&self, block_number: u64) -> bool {
        self.blocks.contains_key(&block_number)
    }

    /// Removes and returns the confirmed block for the given block number.
    pub fn remove_block_by_number(&mut self, block_number: u64) -> Option<ConfirmedBlock<N>> {
        let (hash, block) = self.blocks.remove(&block_number)?;
        self.hash_to_number.remove(&hash);
        self.remove_tx_index_for_block(&block);
        Some(block)
    }

    /// Removes and returns the confirmed block for the given block hash.
    pub fn remove_block_by_hash(&mut self, block_hash: &B256) -> Option<ConfirmedBlock<N>> {
        let number = self.hash_to_number.remove(block_hash)?;
        let (_, block) = self.blocks.remove(&number)?;
        self.remove_tx_index_for_block(&block);
        Some(block)
    }

    /// Removes all tx index entries for the transactions in the given block.
    fn remove_tx_index_for_block(&mut self, block: &ConfirmedBlock<N>) {
        for tx in block.executed_block.recovered_block.body().transactions() {
            self.tx_index.remove(&*tx.tx_hash());
        }
    }

    /// Flushes all entries with block number <= `canonical_number`.
    ///
    /// Called when the canonical chain catches up to the confirmed cache.
    /// Returns the number of entries flushed.
    pub fn flush_up_to(&mut self, canonical_number: u64) -> usize {
        let retained = self.blocks.split_off(&(canonical_number + 1));
        let stale = std::mem::replace(&mut self.blocks, retained);

        let count = stale.len();
        for (hash, bar) in stale.into_values() {
            self.hash_to_number.remove(&hash);
            self.remove_tx_index_for_block(&bar);
        }
        count
    }
}
