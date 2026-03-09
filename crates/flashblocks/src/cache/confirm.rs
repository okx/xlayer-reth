use std::collections::{BTreeMap, HashMap};

use alloy_primitives::B256;
use eyre::eyre;
use reth_primitives_traits::NodePrimitives;
use reth_rpc_eth_types::block::BlockAndReceipts;

const DEFAULT_CONFIRM_CACHE_SIZE: usize = 5_000;

/// Confirmed flashblocks sequence cache that is ahead of the current
/// canonical chain. We optimistically commit confirmed flashblocks sequences to
/// the cache and flush them when the canonical chain catches up.
///
/// Block data is stored in a `BTreeMap` keyed by block number, enabling O(log n)
/// range splits in [`flush_up_to`](Self::flush_up_to). A secondary `HashMap`
/// provides O(1) block hash to block number reverse lookups.
#[derive(Debug)]
pub struct ConfirmCache<N: NodePrimitives> {
    /// Primary storage: block number → (block hash, block + receipts).
    /// `BTreeMap` ordering enables efficient range-based flush via `split_off`.
    blocks: BTreeMap<u64, (B256, BlockAndReceipts<N>)>,
    /// Reverse index: block hash → block number for O(1) hash-based lookups.
    hash_to_number: HashMap<B256, u64>,
}

impl<N: NodePrimitives> Default for ConfirmCache<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: NodePrimitives> ConfirmCache<N> {
    /// Creates a new [`ConfirmCache`].
    pub fn new() -> Self {
        Self { blocks: BTreeMap::new(), hash_to_number: HashMap::new() }
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
        hash: B256,
        block: BlockAndReceipts<N>,
    ) -> eyre::Result<()> {
        if self.blocks.len() >= DEFAULT_CONFIRM_CACHE_SIZE {
            return Err(eyre!(
                "confirm cache at max capacity ({DEFAULT_CONFIRM_CACHE_SIZE}), cannot insert block: {height}"
            ));
        }
        self.hash_to_number.insert(hash, height);
        self.blocks.insert(height, (hash, block));
        Ok(())
    }

    /// Returns the confirmed block for the given block hash, if present.
    pub fn get_by_hash(&self, block_hash: &B256) -> Option<BlockAndReceipts<N>> {
        let number = self.hash_to_number.get(block_hash)?;
        self.blocks.get(number).map(|(_, block)| block.clone())
    }

    /// Returns the confirmed block for the given block number, if present.
    pub fn get_by_number(&self, block_number: u64) -> Option<BlockAndReceipts<N>> {
        self.blocks.get(&block_number).map(|(_, block)| block.clone())
    }

    /// Returns the block hash for the given block number, if cached.
    pub fn hash_for_number(&self, block_number: u64) -> Option<B256> {
        self.blocks.get(&block_number).map(|(hash, _)| *hash)
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
    pub fn remove_by_number(&mut self, block_number: u64) -> Option<BlockAndReceipts<N>> {
        let (hash, block) = self.blocks.remove(&block_number)?;
        self.hash_to_number.remove(&hash);
        Some(block)
    }

    /// Removes and returns the confirmed block for the given block hash.
    pub fn remove_by_hash(&mut self, block_hash: &B256) -> Option<BlockAndReceipts<N>> {
        let number = self.hash_to_number.remove(block_hash)?;
        self.blocks.remove(&number).map(|(_, block)| block)
    }

    /// Flushes all entries with block number >= `from` (the reorged range).
    /// Returns the number of entries flushed.
    pub fn flush_from(&mut self, from: u64) -> usize {
        let reorged = self.blocks.split_off(&from);
        let count = reorged.len();
        for (hash, _) in reorged.into_values() {
            self.hash_to_number.remove(&hash);
        }
        count
    }

    /// Flushes all entries with block number <= `canonical_number`.
    ///
    /// Called when the canonical chain catches up to the confirmed cache.
    /// Returns the number of entries flushed.
    pub fn flush_up_to(&mut self, canonical_number: u64) -> usize {
        let retained = self.blocks.split_off(&(canonical_number + 1));
        let stale = std::mem::replace(&mut self.blocks, retained);

        let count = stale.len();
        for (hash, _) in stale.into_values() {
            self.hash_to_number.remove(&hash);
        }
        count
    }

    /// Returns the highest cached block number, or `None` if empty.
    pub fn latest_block_number(&self) -> Option<u64> {
        self.blocks.keys().next_back().copied()
    }

    /// Clears all entries.
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.hash_to_number.clear();
    }
}
