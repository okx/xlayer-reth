use parking_lot::RwLock;
use ringbuffer::{AllocRingBuffer, RingBuffer};
use std::{collections::BTreeMap, sync::Arc};
use tracing::*;

use alloy_eips::eip2718::WithEncoded;
use alloy_primitives::B256;
use alloy_rpc_types_engine::PayloadId;
use op_alloy_rpc_types_engine::OpFlashblockPayload;

use reth_primitives_traits::{transaction::TxHashRef, Recovered, SignedTransaction};

const MAX_RAW_CACHE_SIZE: usize = 10;

/// The raw flashblocks sequence cache for new incoming flashblocks from the sequencer.
/// The cache accumulates last two flashblocks sequences in memory, to handle scenario
/// when flashblocks received are out-of-order, and committing the previous sequence
/// state to the state cache is not yet possible due to parent hash mismatch (we still
/// need the previous flashblocks sequence to compute the state root).
///
/// The raw cache is used to:
/// 1. Track the next best sequence to build, based on cache state (consecutive flashblocks
///    required)
/// 2. Re-org detection when a new flashblock is received
pub struct RawFlashblocksCache<T: SignedTransaction> {
    inner: Arc<RwLock<RawFlashblocksCacheInner<T>>>,
}

impl<T: SignedTransaction> RawFlashblocksCache<T> {
    pub fn new() -> Self {
        let inner = Arc::new(RwLock::new(RawFlashblocksCacheInner::new()));
        Self { inner }
    }

    pub fn handle_canonical_height(&mut self, height: u64) {
        self.inner.write().handle_canonical_height(height);
    }

    pub fn handle_flashblock(&mut self, flashblock: OpFlashblockPayload) -> eyre::Result<()> {
        self.inner.write().handle_flashblock(flashblock)
    }
}

#[derive(Debug, Clone)]
pub struct RawFlashblocksCacheInner<T: SignedTransaction> {
    cache: AllocRingBuffer<RawFlashblocksEntry<T>>,
    canon_height: u64,
}

impl<T: SignedTransaction> RawFlashblocksCacheInner<T> {
    fn new() -> Self {
        Self { cache: AllocRingBuffer::new(MAX_RAW_CACHE_SIZE), canon_height: 0 }
    }

    pub fn handle_canonical_height(&mut self, height: u64) {
        self.canon_height = height;
        // Evict entries from the front (oldest) whose block number is at or
        // below the new canonical height.
        while self
            .cache
            .front()
            .is_some_and(|entry| entry.block_number().is_some_and(|n| n <= height))
        {
            self.cache.dequeue();
        }
    }

    pub fn handle_flashblock(&mut self, flashblock: OpFlashblockPayload) -> eyre::Result<()> {
        if flashblock.block_number() <= self.canon_height {
            debug!(
                target: "flashblocks",
                flashblock_number = flashblock.block_number(),
                canon_height = self.canon_height,
                "Received old flashblock behind canonical height, skip adding",
            );
            return Ok(());
        }

        // Search for an existing entry matching this payload_id.
        let existing =
            self.cache.iter_mut().find(|entry| entry.payload_id() == Some(flashblock.payload_id));

        if let Some(entry) = existing {
            entry.insert_flashblock(flashblock)?;
        } else {
            // New sequence — push to ring buffer, evicting the oldest entry
            // when the cache is full.
            let mut entry = RawFlashblocksEntry::new();
            entry.insert_flashblock(flashblock)?;
            self.cache.push(entry);
        }
        Ok(())
    }
}

/// Raw flashblocks sequence keeps track of the flashblocks sequence based on their
/// `payload_id`.
#[derive(Debug, Clone)]
struct RawFlashblocksEntry<T: SignedTransaction> {
    /// Tracks the individual flashblocks in order
    payloads: BTreeMap<u64, OpFlashblockPayload>,
    /// Tracks the recovered transactions by index
    recovered_transactions_by_index: BTreeMap<u64, Vec<WithEncoded<Recovered<T>>>>,
    /// Tracks if the accumulated sequence has received the first base flashblock
    has_base: bool,
}

impl<T: SignedTransaction> RawFlashblocksEntry<T> {
    fn new() -> Self {
        Self {
            payloads: BTreeMap::new(),
            recovered_transactions_by_index: BTreeMap::new(),
            has_base: false,
        }
    }

    /// Inserts a flashblock into the sequence.
    fn insert_flashblock(&mut self, flashblock: OpFlashblockPayload) -> eyre::Result<()> {
        if !self.can_accept(&flashblock) {
            warn!(
                target: "flashblocks",
                incoming_id = ?flashblock.payload_id,
                current_id = ?self.payload_id(),
                incoming_height = %flashblock.block_number(),
                current_height = ?self.block_number(),
                "Incoming flashblock failed to be accepted into the sequence, possible re-org detected",
            );
            return Err(eyre::eyre!("incoming flashblock failed to be accepted into the sequence, possible re-org detected"));
        }

        if flashblock.index == 0 {
            self.has_base = true;
        }
        let flashblock_index = flashblock.index;
        let recovered_txs = flashblock.recover_transactions().collect::<Result<Vec<_>, _>>()?;
        self.payloads.insert(flashblock_index, flashblock);
        self.recovered_transactions_by_index.insert(flashblock_index, recovered_txs);
        Ok(())
    }

    /// Returns whether this flashblock would be accepted into the current sequence.
    fn can_accept(&self, flashblock: &OpFlashblockPayload) -> bool {
        if self.payloads.is_empty() {
            return true;
        }
        self.block_number() == Some(flashblock.block_number())
            && self.payload_id() == Some(flashblock.payload_id)
            && self.payloads.get(&flashblock.index).is_none()
    }

    fn get_best_revision(&self) -> Option<u64> {
        if !self.has_base || self.payloads.is_empty() {
            return None;
        }
        let mut new_revision = 0;
        for (index, _) in self.payloads.iter() {
            if *index == 0 {
                continue;
            }
            // If the index is not consecutive, break the loop
            if new_revision != *index - 1 {
                break;
            }
            new_revision = *index;
        }
        Some(new_revision)
    }

    pub fn block_number(&self) -> Option<u64> {
        Some(self.payloads.values().next()?.block_number())
    }

    pub fn payload_id(&self) -> Option<PayloadId> {
        Some(self.payloads.values().next()?.payload_id)
    }

    fn transactions(&self) -> Vec<WithEncoded<Recovered<T>>> {
        self.recovered_transactions_by_index.values().flatten().cloned().collect()
    }

    fn tx_hashes(&self) -> Vec<B256> {
        self.recovered_transactions_by_index.values().flatten().map(|tx| *tx.tx_hash()).collect()
    }

    #[cfg(test)]
    fn transaction_count(&self) -> usize {
        self.recovered_transactions_by_index.values().map(Vec::len).sum()
    }
}
