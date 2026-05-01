use crate::execution::BuildArgs;
use parking_lot::RwLock;
use ringbuffer::{AllocRingBuffer, RingBuffer};
use std::{collections::BTreeMap, sync::Arc};

use alloy_eip7928::BlockAccessList;
use alloy_eips::{eip2718::WithEncoded, eip4895::Withdrawal};
use alloy_rpc_types_engine::PayloadId;
use op_alloy_rpc_types_engine::{OpFlashblockPayload, OpFlashblockPayloadBase};
use reth_primitives_traits::{Recovered, SignedTransaction};
use reth_revm::state::bal::{AccountBal, Bal};

use xlayer_builder::broadcast::{XLayerFlashblockMessage, XLayerFlashblockPayload};

const MAX_RAW_CACHE_SIZE: usize = 50;

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
    pub fn new(disable_access_list: bool) -> Self {
        let inner = Arc::new(RwLock::new(RawFlashblocksCacheInner::new(disable_access_list)));
        Self { inner }
    }

    pub fn handle_canonical_height(&self, height: u64) {
        self.inner.write().handle_canonical_height(height);
    }

    pub fn handle_message(&self, payload: XLayerFlashblockMessage) -> eyre::Result<u64> {
        match payload {
            XLayerFlashblockMessage::Payload(payload) => {
                self.inner.write().handle_flashblock(*payload)
            }
            XLayerFlashblockMessage::PayloadEnd(payload) => {
                self.inner.write().handle_end_sequence(payload.payload_id)
            }
        }
    }

    pub(crate) fn try_get_buildable_args(
        &self,
        height: u64,
    ) -> Option<BuildArgs<Vec<WithEncoded<Recovered<T>>>>> {
        self.inner.read().try_get_buildable_args(height)
    }
}

#[derive(Debug, Clone)]
pub struct RawFlashblocksCacheInner<T: SignedTransaction> {
    cache: AllocRingBuffer<RawFlashblocksEntry<T>>,
    canon_height: u64,
    disable_access_list: bool,
}

impl<T: SignedTransaction> RawFlashblocksCacheInner<T> {
    fn new(disable_access_list: bool) -> Self {
        Self {
            cache: AllocRingBuffer::new(MAX_RAW_CACHE_SIZE),
            canon_height: 0,
            disable_access_list,
        }
    }

    pub fn handle_canonical_height(&mut self, height: u64) {
        self.canon_height = height;
        // Evict all entries whose height is at or below the new canonical height.
        let retained: Vec<_> = self
            .cache
            .drain()
            .filter(|entry| entry.block_number().is_none_or(|n| n > height))
            .collect();
        for entry in retained {
            self.cache.enqueue(entry);
        }
    }

    pub fn handle_flashblock(&mut self, payload: XLayerFlashblockPayload) -> eyre::Result<u64> {
        let XLayerFlashblockPayload { inner: flashblock, target_index } = payload;
        let incoming_height = flashblock.block_number();
        if incoming_height <= self.canon_height {
            return Err(eyre::eyre!(
                "Received old flashblock behind canonical height, skip adding to raw cache: flashblock_number={}, canon_height={}",
                incoming_height,
                self.canon_height,
            ));
        }

        // Search for an existing entry matching this payload_id.
        let existing =
            self.cache.iter_mut().find(|entry| entry.payload_id() == Some(flashblock.payload_id));

        if let Some(entry) = existing {
            entry.insert_flashblock(flashblock, target_index)?;
        } else {
            // New sequence — push to ring buffer, evicting the oldest entry
            // when the cache is full.
            let mut entry = RawFlashblocksEntry::new();
            entry.insert_flashblock(flashblock, target_index)?;
            self.cache.enqueue(entry);
        }
        Ok(incoming_height)
    }

    fn handle_end_sequence(&mut self, payload_id: PayloadId) -> eyre::Result<u64> {
        // Search for an existing entry matching this payload_id.
        let existing = self.cache.iter_mut().find(|entry| entry.payload_id() == Some(payload_id));
        existing
            .and_then(|entry| entry.set_end_sequence())
            .ok_or(eyre::eyre!("no entry found for payload end, payload_id: {:?}", payload_id))
    }

    fn try_get_buildable_args(
        &self,
        height: u64,
    ) -> Option<BuildArgs<Vec<WithEncoded<Recovered<T>>>>> {
        // Iterate newest-first so that the most recent entry is always picked first
        // (same height, different payload_id).
        self.cache
            .iter()
            .rev()
            .find(|entry| entry.block_number() == Some(height))
            .and_then(|entry| entry.try_to_buildable_args(self.disable_access_list))
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
    /// Tracks the decoded EIP-7928 [`BlockAccessList`] by index
    block_access_lists: BTreeMap<u64, Bal>,
    /// Tracks if the accumulated sequence has received the first base flashblock
    has_base: bool,
    /// The sequencer's target flashblock index. Zero if unset.
    target_index: u64,
    /// Tracks if the sequence has received the end-of-sequence signal
    sequence_end: bool,
}

impl<T: SignedTransaction> RawFlashblocksEntry<T> {
    fn new() -> Self {
        Self {
            payloads: BTreeMap::new(),
            recovered_transactions_by_index: BTreeMap::new(),
            block_access_lists: BTreeMap::new(),
            has_base: false,
            target_index: 0,
            sequence_end: false,
        }
    }

    /// Inserts a flashblock into the sequence.
    fn insert_flashblock(
        &mut self,
        flashblock: OpFlashblockPayload,
        target_index: u64,
    ) -> eyre::Result<()> {
        if !self.can_accept(&flashblock) {
            return Err(eyre::eyre!(
                "Incoming flashblock failed to be accepted into the sequence, possible re-org detected: incoming_id={:?}, current_id={:?}, incoming_height={}, current_height={:?}",
                flashblock.payload_id,
                self.payload_id(),
                flashblock.block_number(),
                self.block_number(),
            ));
        }

        if flashblock.index == 0 {
            self.has_base = true;
        }
        if target_index > 0 {
            self.target_index = target_index;
        }
        let flashblock_index = flashblock.index;
        let recovered_txs = flashblock.recover_transactions().collect::<Result<Vec<_>, _>>()?;

        match flashblock.metadata.block_access_list() {
            None => {}
            Some(Ok(list)) => {
                let mut bal = Bal::new();
                for alloy_account in list {
                    let address = alloy_account.address;
                    match AccountBal::try_from_alloy(alloy_account) {
                        Ok((addr, incoming)) => match bal.accounts.get_mut(&addr) {
                            Some(existing) => {
                                existing.account_info.extend(incoming.account_info);
                                existing.storage.extend(incoming.storage);
                            }
                            None => {
                                bal.accounts.insert(addr, incoming);
                            }
                        },
                        Err(err) => tracing::warn!(
                            target: "flashblocks",
                            flashblock_index,
                            ?address,
                            ?err,
                            "BAL account decode failed at insert, account skipped",
                        ),
                    }
                }
                if !bal.accounts.is_empty() {
                    self.block_access_lists.insert(flashblock_index, bal);
                }
            }
            Some(Err(e)) => {
                tracing::warn!(
                    target: "flashblocks",
                    flashblock_index,
                    block_number = flashblock.metadata.block_number,
                    error = %e,
                    "Failed to decode RLP access list at insert, flashblock retained without it",
                );
            }
        }

        self.payloads.insert(flashblock_index, flashblock);
        self.recovered_transactions_by_index.insert(flashblock_index, recovered_txs);
        Ok(())
    }

    fn set_end_sequence(&mut self) -> Option<u64> {
        self.sequence_end = true;
        self.block_number()
    }

    /// Returns whether this flashblock would be accepted into the current sequence.
    fn can_accept(&self, flashblock: &OpFlashblockPayload) -> bool {
        if self.payloads.is_empty() {
            return true;
        }
        self.block_number() == Some(flashblock.block_number())
            && self.payload_id() == Some(flashblock.payload_id)
            && !self.payloads.contains_key(&flashblock.index)
    }

    fn try_get_best_revision(&self) -> Option<u64> {
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

    fn base(&self) -> Option<&OpFlashblockPayloadBase> {
        self.payloads.get(&0)?.base.as_ref()
    }

    fn withdrawals_at(&self, index: u64) -> Vec<Withdrawal> {
        // Per the OP Stack flashblocks spec, each diff's `withdrawals` field is cumulative
        // (the complete list for the entire block), not incremental
        self.payloads.get(&index).map(|p| p.diff.withdrawals.clone()).unwrap_or_default()
    }

    fn transactions_up_to(&self, up_to: u64) -> Vec<WithEncoded<Recovered<T>>> {
        self.recovered_transactions_by_index
            .range(..=up_to)
            .flat_map(|(_, txs)| txs.iter().cloned())
            .collect()
    }

    /// Aggregates per-flashblock BALs into a single block-wide [`BlockAccessList`]
    /// covering flashblocks `[0..=up_to]`.
    fn access_list_up_to(&self, up_to: u64) -> Option<BlockAccessList> {
        let mut merged = Bal::new();
        for (_, bal) in self.block_access_lists.range(..=up_to) {
            for (addr, incoming) in &bal.accounts {
                match merged.accounts.get_mut(addr) {
                    Some(existing) => {
                        existing.account_info.extend(incoming.account_info.clone());
                        existing.storage.extend(incoming.storage.clone());
                    }
                    None => {
                        merged.accounts.insert(*addr, incoming.clone());
                    }
                }
            }
        }

        if merged.accounts.is_empty() {
            return None;
        }
        Some(merged.into_alloy_bal())
    }

    fn try_to_buildable_args(
        &self,
        disable_access_list: bool,
    ) -> Option<BuildArgs<Vec<WithEncoded<Recovered<T>>>>> {
        let best_revision = self.try_get_best_revision()?;
        Some(BuildArgs {
            base: self.base()?.clone(),
            payload_id: self.payload_id()?,
            transactions: self.transactions_up_to(best_revision),
            access_list: if !disable_access_list {
                self.access_list_up_to(best_revision)
            } else {
                None
            },
            withdrawals: self.withdrawals_at(best_revision),
            last_flashblock_index: best_revision,
            target_index: self.target_index,
            sequence_end: self.sequence_end,
        })
    }

    #[cfg(test)]
    fn transaction_count(&self) -> usize {
        self.recovered_transactions_by_index.values().map(Vec::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TestFlashBlockFactory;
    use reth_optimism_primitives::OpTransactionSigned;

    type TestRawCache = RawFlashblocksCacheInner<OpTransactionSigned>;

    /// Wraps an [`OpFlashblockPayload`] into an [`XLayerFlashblockPayload`] with
    /// `target_index: 0` for tests that don't care about the target count.
    fn wrap(fb: OpFlashblockPayload) -> XLayerFlashblockPayload {
        XLayerFlashblockPayload::new(fb, 0)
    }

    #[test]
    fn test_raw_entry_can_accept_first_flashblock_on_empty_entry() {
        // Arrange
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let mut cache = TestRawCache::new(false);

        // Act
        let result = cache.handle_flashblock(wrap(fb0));

        // Assert: empty entry accepts anything without error
        assert!(result.is_ok(), "empty entry should accept first flashblock");
        assert_eq!(cache.cache.len(), 1);
    }

    #[test]
    fn test_raw_entry_rejects_duplicate_index_in_same_sequence() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let fb0_dup = factory.flashblock_at(0).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("first flashblock should succeed");
        let result = cache.handle_flashblock(wrap(fb0_dup));
        assert!(result.is_err(), "duplicate index within same sequence should be rejected");
    }

    #[test]
    fn test_raw_entry_rejects_mismatched_block_number() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let payload_id = fb0.payload_id;
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("first flashblock should succeed");
        let fb_wrong_block = factory
            .builder()
            .index(1)
            .block_number(999) // different block number
            .payload_id(payload_id)
            .build();
        let result = cache.handle_flashblock(wrap(fb_wrong_block));
        assert!(result.is_err(), "mismatched block number with same payload_id should be rejected");
        assert_eq!(cache.cache.len(), 1, "rejected flashblock should not create a new entry");
    }

    #[test]
    fn test_raw_entry_accepts_out_of_order_flashblocks_within_same_sequence() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let payload_id = fb0.payload_id;
        let fb2 = factory.builder().index(2).block_number(100).payload_id(payload_id).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        let result = cache.handle_flashblock(wrap(fb2));
        assert!(result.is_ok(), "out-of-order unique index should be accepted");
    }

    #[test]
    fn test_raw_entry_get_best_revision_returns_none_without_base() {
        let factory = TestFlashBlockFactory::new();
        let fb1 = factory.builder().index(1).block_number(100).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb1)).expect("fb1 insert");
        let entry = cache.cache.iter().next().expect("entry should exist");
        let best = entry.try_get_best_revision();
        assert!(best.is_none(), "get_best_revision should return None without base (index 0)");
    }

    #[test]
    fn test_raw_entry_get_best_revision_returns_zero_with_only_base() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();

        let mut cache = TestRawCache::new(false);
        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        let entry = cache.cache.iter().next().expect("entry should exist");
        let best = entry.try_get_best_revision();
        assert_eq!(best, Some(0), "only index 0 → best revision is 0");
    }

    #[test]
    fn test_raw_entry_get_best_revision_with_consecutive_sequence() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let fb1 = factory.flashblock_after(&fb0).build();
        let fb2 = factory.flashblock_after(&fb1).build();
        let fb3 = factory.flashblock_after(&fb2).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0");
        cache.handle_flashblock(wrap(fb1)).expect("fb1");
        cache.handle_flashblock(wrap(fb2)).expect("fb2");
        cache.handle_flashblock(wrap(fb3)).expect("fb3");
        let entry = cache.cache.iter().next().expect("entry should exist");
        let best = entry.try_get_best_revision();
        assert_eq!(best, Some(3), "consecutive 0..3 → best revision 3");
    }

    #[test]
    fn test_raw_entry_get_best_revision_stops_at_gap() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let payload_id = fb0.payload_id;
        let fb1 = factory.flashblock_after(&fb0).build();
        let fb3 = factory.builder().index(3).block_number(100).payload_id(payload_id).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0");
        cache.handle_flashblock(wrap(fb1)).expect("fb1");
        cache.handle_flashblock(wrap(fb3)).expect("fb3 (gap after index 1)");
        let entry = cache.cache.iter().next().expect("entry should exist");
        let best = entry.try_get_best_revision();
        assert_eq!(best, Some(1), "gap between 1 and 3 → best revision is 1");
    }

    #[test]
    fn test_raw_cache_handle_canonical_height_evicts_entries_at_or_below_height() {
        let factory = TestFlashBlockFactory::new();
        let fb100 = factory.flashblock_at(0).build();
        let fb101 = factory.flashblock_for_next_block(&fb100).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb100)).expect("fb100");
        cache.handle_flashblock(wrap(fb101)).expect("fb101");
        assert_eq!(cache.cache.len(), 2);
        cache.handle_canonical_height(100);
        assert_eq!(cache.cache.len(), 1, "block 100 entry should be evicted");
        let remaining = cache.cache.iter().next().expect("one entry should remain");
        assert_eq!(remaining.block_number(), Some(101), "remaining entry should be for block 101");
    }

    #[test]
    fn test_raw_cache_handle_canonical_height_evicts_multiple_entries() {
        // Arrange: insert flashblocks for blocks 100, 101, 102
        let factory = TestFlashBlockFactory::new();
        let fb100 = factory.flashblock_at(0).build();
        let fb101 = factory.flashblock_for_next_block(&fb100).build();
        let fb102 = factory.flashblock_for_next_block(&fb101).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb100)).expect("fb100");
        cache.handle_flashblock(wrap(fb101)).expect("fb101");
        cache.handle_flashblock(wrap(fb102)).expect("fb102");
        assert_eq!(cache.cache.len(), 3);
        cache.handle_canonical_height(102);
        assert_eq!(cache.cache.len(), 0, "all entries at or below height 102 should be evicted");
    }

    #[test]
    fn test_raw_cache_handle_canonical_height_keeps_entries_above_height() {
        let factory = TestFlashBlockFactory::new();
        let fb100 = factory.flashblock_at(0).build();
        let fb101 = factory.flashblock_for_next_block(&fb100).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb100)).expect("fb100");
        cache.handle_flashblock(wrap(fb101)).expect("fb101");
        cache.handle_canonical_height(99);
        assert_eq!(cache.cache.len(), 2, "no entries should be evicted below their block numbers");
    }

    #[test]
    fn test_raw_cache_rejects_flashblock_at_or_below_canonical_height() {
        let factory = TestFlashBlockFactory::new();
        let fb100 = factory.flashblock_at(0).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_canonical_height(100);
        let result = cache.handle_flashblock(wrap(fb100));
        assert!(result.is_err(), "flashblock at canonical height should be rejected");
        assert_eq!(cache.cache.len(), 0, "flashblock at canonical height should not be inserted");
    }

    #[test]
    fn test_raw_cache_groups_flashblocks_by_payload_id() {
        let factory = TestFlashBlockFactory::new();
        let fb0_seq1 = factory.flashblock_at(0).build();
        let fb0_seq2 = factory.flashblock_for_next_block(&fb0_seq1).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0_seq1.clone())).expect("seq1 fb0");
        cache.handle_flashblock(wrap(fb0_seq2.clone())).expect("seq2 fb0");
        let fb1_seq1 = factory.flashblock_after(&fb0_seq1).build();
        cache.handle_flashblock(wrap(fb1_seq1)).expect("seq1 fb1");
        let entries: Vec<_> = cache.cache.iter().collect();
        assert_eq!(entries.len(), 2, "should have two separate entries for two payload_ids");
    }

    #[test]
    fn test_raw_cache_ring_buffer_evicts_oldest_entry_when_full() {
        let factory = TestFlashBlockFactory::new();
        let mut prev_fb = factory.flashblock_at(0).build();
        let first_block_num = prev_fb.metadata.block_number;
        let mut cache = TestRawCache::new(false);
        cache.handle_flashblock(wrap(prev_fb.clone())).expect("first fb");

        // Fill up to MAX_RAW_CACHE_SIZE (10) unique sequences
        for _ in 1..MAX_RAW_CACHE_SIZE {
            let next_fb = factory.flashblock_for_next_block(&prev_fb).build();
            cache.handle_flashblock(wrap(next_fb.clone())).expect("fill fb");
            prev_fb = next_fb;
        }
        assert_eq!(cache.cache.len(), MAX_RAW_CACHE_SIZE, "cache should be at max capacity");

        // Insert one more sequence to trigger FIFO eviction
        let overflow_fb = factory.flashblock_for_next_block(&prev_fb).build();
        let overflow_block_num = overflow_fb.metadata.block_number;
        cache.handle_flashblock(wrap(overflow_fb)).expect("overflow fb");

        // Assert: cache is still at max size (oldest entry evicted)
        assert_eq!(cache.cache.len(), MAX_RAW_CACHE_SIZE, "cache size should remain at max");
        // The oldest entry (first_block_num) should have been evicted
        let has_first = cache.cache.iter().any(|e| e.block_number() == Some(first_block_num));
        let has_overflow = cache.cache.iter().any(|e| e.block_number() == Some(overflow_block_num));
        assert!(!has_first, "oldest entry should have been evicted");
        assert!(has_overflow, "newest entry should be present");
    }

    #[test]
    fn test_raw_entry_block_number_returns_none_on_empty() {
        let entry = RawFlashblocksEntry::<OpTransactionSigned>::new();
        assert!(entry.block_number().is_none());
    }

    #[test]
    fn test_raw_entry_block_number_returns_correct_value_after_insert() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let expected_block = fb0.metadata.block_number;
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        let entry = cache.cache.iter().next().expect("entry should exist");
        assert_eq!(entry.block_number(), Some(expected_block));
    }

    #[test]
    fn test_raw_entry_transaction_count_is_zero_on_empty_flashblock() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build(); // no transactions set
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        let entry = cache.cache.iter().next().expect("entry should exist");
        assert_eq!(entry.transaction_count(), 0, "flashblock with no txs should have count 0");
    }

    #[test]
    fn test_raw_entry_has_base_set_after_inserting_index_zero() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        let entry = cache.cache.iter().next().expect("entry should exist");
        assert!(entry.has_base, "has_base should be true after inserting index 0");
    }

    #[test]
    fn test_raw_entry_has_base_not_set_when_only_non_zero_index_inserted() {
        let factory = TestFlashBlockFactory::new();
        let fb1 = factory.builder().index(1).block_number(100).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb1)).expect("fb1 insert");
        let entry = cache.cache.iter().next().expect("entry should exist");
        assert!(!entry.has_base, "has_base should be false when only index 1 inserted");
    }

    #[test]
    fn test_raw_flashblocks_cache_handle_flashblock_inserts_via_arc_rwlock() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let cache = RawFlashblocksCache::<OpTransactionSigned>::new(false);

        let result =
            cache.handle_message(XLayerFlashblockMessage::from_flashblock_payload(wrap(fb0)));
        assert!(result.is_ok(), "handle_flashblock via Arc<RwLock> wrapper should succeed");
    }

    #[test]
    fn test_raw_entry_get_best_revision_with_only_index_one_no_base() {
        let factory = TestFlashBlockFactory::new();
        let fb1 = factory.builder().index(1).block_number(100).build();

        let mut cache = TestRawCache::new(false);
        cache.handle_flashblock(wrap(fb1)).expect("fb1 insert");
        let entry = cache.cache.iter().next().expect("entry should exist");
        let best = entry.try_get_best_revision();
        // Assert: no base → None, even though index 1 exists
        assert!(best.is_none(), "no base means get_best_revision must return None");
    }

    #[test]
    fn test_raw_entry_get_best_revision_gap_immediately_after_base() {
        // Arrange: only index 0 and index 2, no index 1
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let payload_id = fb0.payload_id;
        let block_number = fb0.metadata.block_number;
        let fb2 =
            factory.builder().index(2).block_number(block_number).payload_id(payload_id).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0");
        cache.handle_flashblock(wrap(fb2)).expect("fb2");
        let entry = cache.cache.iter().next().expect("entry should exist");
        let best = entry.try_get_best_revision();
        // Assert: gap immediately after base (index 1 missing) → best revision is 0
        assert_eq!(best, Some(0), "gap at index 1 means best revision stays at 0");
    }

    // --- can_accept edge cases ---

    #[test]
    fn test_raw_entry_can_accept_rejects_mismatched_payload_id_with_same_block_number() {
        // Arrange: insert fb with payload_id A at block 100
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let different_payload_id = alloy_rpc_types_engine::PayloadId::new([
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]);
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0.clone())).expect("fb0 insert");
        let fb_diff = factory
            .builder()
            .index(0)
            .block_number(fb0.metadata.block_number)
            .payload_id(different_payload_id)
            .build();
        let result = cache.handle_flashblock(wrap(fb_diff));
        // Assert: new entry created (no error), but we now have 2 entries
        assert!(result.is_ok(), "different payload_id with same block creates new entry");
        assert_eq!(
            cache.cache.len(),
            2,
            "different payload_id should produce a second cache entry"
        );
    }

    #[test]
    fn test_raw_cache_accumulates_flashblocks_into_single_entry_for_same_payload_id() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let fb1 = factory.flashblock_after(&fb0).build();
        let fb2 = factory.flashblock_after(&fb1).build();
        let fb3 = factory.flashblock_after(&fb2).build();
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0");
        cache.handle_flashblock(wrap(fb1)).expect("fb1");
        cache.handle_flashblock(wrap(fb2)).expect("fb2");
        cache.handle_flashblock(wrap(fb3)).expect("fb3");
        // Assert: all four go into a single entry (same payload_id)
        assert_eq!(
            cache.cache.len(),
            1,
            "all flashblocks with the same payload_id should accumulate into one entry"
        );
        let entry = cache.cache.iter().next().expect("entry should exist");
        assert_eq!(entry.payloads.len(), 4, "entry should contain 4 payloads");
    }

    #[test]
    fn test_handle_end_sequence_sets_flag_and_returns_block_number() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let payload_id = fb0.payload_id;
        let expected_block = fb0.metadata.block_number;
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        let block_number = cache.handle_end_sequence(payload_id).expect("should succeed");
        assert_eq!(block_number, expected_block);

        let entry = cache.cache.iter().next().expect("entry should exist");
        assert!(entry.sequence_end);
    }

    #[test]
    fn test_handle_end_sequence_errors_for_unknown_payload_id() {
        let mut cache = TestRawCache::new(false);
        let result = cache.handle_end_sequence(PayloadId::new([0xAA; 8]));
        assert!(result.is_err());
    }

    #[test]
    fn test_buildable_args_reflects_sequence_end_flag() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let payload_id = fb0.payload_id;
        let block_number = fb0.metadata.block_number;
        let mut cache = TestRawCache::new(false);

        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        let args_before = cache.try_get_buildable_args(block_number).expect("should be buildable");
        assert!(!args_before.sequence_end);

        cache.handle_end_sequence(payload_id).expect("end sequence");
        let args_after = cache.try_get_buildable_args(block_number).expect("should be buildable");
        assert!(args_after.sequence_end);
    }

    #[test]
    fn test_handle_message_payload_end_via_public_api() {
        let factory = TestFlashBlockFactory::new();
        let fb0 = factory.flashblock_at(0).build();
        let payload_id = fb0.payload_id;
        let expected_block = fb0.metadata.block_number;
        let cache = RawFlashblocksCache::<OpTransactionSigned>::new(false);

        cache
            .handle_message(XLayerFlashblockMessage::from_flashblock_payload(wrap(fb0)))
            .expect("insert");
        let result = cache.handle_message(XLayerFlashblockMessage::from_flashblock_end(payload_id));
        assert_eq!(result.expect("should succeed"), expected_block);
    }

    #[test]
    fn test_flashblock_serde_roundtrip() {
        let raw = r#"{
  "diff": {
    "block_hash": "0x2d902e3fcb5bd57e0bf878cbbda1386e7fb8968d518912d58678a35e58261c46",
    "gas_used": "0x2907796",
    "logs_bloom": "0x5c21065292452cfcd5175abfee20e796773da578307356043ba4f62692aca01204e8908f97ab9df43f1e9c57f586b1c9a7df8b66ffa7746dfeeb538617fea5eb75ad87f8b6653f597d86814dc5ad6de404e5a48aeffcc4b1e170c2bdbc7a334936c66166ba0faa6517597b676ef65c588342756f280f7d610aa3ed35c5d877449bfacbdb9b40d98c457f974ab264ec40e4edd6e9fab4c0cb794bf75f10ea20dab75a1f9fd1c441d4c365d1476841e8593f1d1b9a1c52919a0fcf9fc5eef2ef82fe80971a72d1cde1cb195db4806058a229e88acfddfe1a1308adb6f69afa3aaf67f4bd49e93e9f9532ea30bd891a8ff08de61fb645bec678db816950b47fcef0",
    "receipts_root": "0x2c4203e9aa87258627bf23ab4d5f9d92da30285ea11dc0b3e140a5a8d4b63e26",
    "state_root": "0x0000000000000000000000000000000000000000000000000000000000000000",
    "transactions": [
      "0x02f8c2822105830b0c58840b677c0f840c93fb5a834c4b4094d599955d17a1378651e76557ffc406c71300fcb080b851020026000100271000c8e9d514f85b57b70de033e841d788ab4df1acd691802acc26dcd13fb9e38fa8e10001004e2000c8e9d55bd42770e29cb76904377ffdb22737fc9f5eb36fde875fcbfa687b1c3023c080a07e8486ab3db9f07588a3f37bd8ffb9b349ba9bb738a2500d78a4583e1e54a6f9a068d0b3c729a6777c81dd49bd0c2dc3a079f0ceed4e778fbfe79176e8b70d68d8",
      "0xf90fae820248840158a3c58307291a94bbbfd134e9b44bfb5123898ba36b01de7ab93d9880b90f443087505600000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000e0000000000000000000000000000000000000000000000000000000000000012000000000000000000000000001c2c79343de52f99538cd2cbbd67ba0813f403000000000000000000000000001c2c79343de52f99538cd2cbbd67ba0813f40300000000000000000000000000000000000000000000000000000000000000001000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda0291300000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000004b2ee6f00000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000000000000000018000000000000000000000000000000000000000000000000000000000000003600000000000000000000000000000000000000000000000000000000000000c00000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda029130000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000044095ea7b30000000000000000000000000000000000001ff3684f28c67538d4d072c227340000000000000000000000000000000000000000000000000000000004b2ee6f00000000000000000000000000000000000000000000000000000000000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e22200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000001243b2253c8000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000e00000000000000000000000000000000000000000000000000000000000000001000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda029130000000000000000000000000000000000000000000000000000000000000001000000000000000000000000f70da97812cb96acdf810712aa562db8dfa3dbef000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000133f4000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001ff3684f28c67538d4d072c2273400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000007e42213bc0b000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda029130000000000000000000000000000000000000000000000000000000004b1ba7b000000000000000000000000ea758cac6115309b325c582fd0782d79e350217700000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000007041fff991f000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e222000000000000000000000000d9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca0000000000000000000000000000000000000000000000000000000004b06d9200000000000000000000000000000000000000000000000000000000000000a0d311e79cd2099f6f1f0607040000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000300000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000000000180000000000000000000000000000000000000000000000000000000000000058000000000000000000000000000000000000000000000000000000000000000e4c1fb425e000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda029130000000000000000000000000000000000000000000000000000000004b1ba7b00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000069073bb900000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003c438c9c147000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda029130000000000000000000000000000000000000000000000000000000000002710000000000000000000000000ba12222222228d8ba445958a75a0704d566bf2c800000000000000000000000000000000000000000000000000000000000001c400000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000002e4945bcec9000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001200000000000000000000000000000000000000000000000000000000000000220000000000000000000000000ea758cac6115309b325c582fd0782d79e35021770000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002800000000000000000000000000000000000000000000000000000000069073bb9000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000208f360baf899845441eccdc46525e26bb8860752a0002000000000000000001cd000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000004b1ba7b00000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913000000000000000000000000d9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca00000000000000000000000000000000000000000000000000000000000000027fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008434ee90ca000000000000000000000000f5c4f3dc02c3fb9279495a8fef7b0741da956157000000000000000000000000d9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca0000000000000000000000000000000000000000000000000000000004b1a7880000000000000000000000000000000000000000000000000000000000002710000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e22200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000001243b2253c8000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000e00000000000000000000000000000000000000000000000000000000000000001000000000000000000000000d9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca000000000000000000000000000000000000000000000000000000000000000100000000000000000000000001c2c79343de52f99538cd2cbbd67ba0813f403000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002887696e8edbbcbd7306955512ff6f2d8426403eef4762157da3e9c5a89d78f682422da0c8d8b1aa1c9bfd1fe1e4a10c6123caa2fe582294aa5798c54546faa4c09590a9a012a1c78fca9cfefd281c1e44682de3c4420299da5cf2ae498f67d7de7dcf166c",
      "0x02f8f582210582a649831db02984026c1a34833d090094f2cb4e685946beecbc9ce5f318b68edc583bcfa080b88600000000000069073af31c4289d066d04f33681f6686155c8243dff963557765630a39bdd8c54e6b7dbe5d4b689e9d536608db03163882cf005f7b5813e41d2fdec75161c8470a410c4c9201000202b6e39c63c7e4ebc01d51f845dfc9cff3f5adf9ef2710000000000103cd1f9777571493aeacb7eae45cd30a226d3e612d4e200000000000c080a088fd1a2b2e5891109afc3845b2c8b0ca76ea8306190dcb80a703a2451f7bab25a0718ae373e36c8ddb2b934ca936ed824db22c0625cfea29be3d408ff41787fc8c",
      "0x02f9030b822105830536f9830f58ab84025c6b93833d090094c90d989d809e26b2d95fb72eb3288fef72af8c2f80b9029a00000000000069073af31c3d4d0646e102b6f958428cd8ed562efa6efb234f629b5f6ca52a15fd2e33aea76eb64fb04cae81b3e5b769dbdc681dcfd4b7a802a2cacdf1ccb65276a722c67607000202b6e39c63c7e4ebc01d51f845dfc9cff3f5adf9ef2710000000000103cd1f9777571493aeacb7eae45cd30a226d3e612d4e200000000000010206777762d3eb91810b15526c2c9102864d722ef7a9ed24e77271c1dcbf0fdcba68138800000000010698c8f03094a9e65ccedc14c40130e4a5dd0ce14fb12ea58cbeac11f662b458b9271000000000000003045a9ad2bb92b0b3e5c571fdd5125114e04e02be1a0bb80000000001036e55486ea6b8691ba58224f3cae35505add86c372710000000000003681d6e4b0b020656ca04956ddaf76add7ef022f60dac00000000010003028be0fcdd7cf0b53b7b82b8f6ea8586d07c53359f2710000000000006c30e25679d5c77b257ac3a61ad08603b11e7afe77ac9222a5386c27d08b6b6c3ea6000000000010696d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a01f400000000000002020d028b2d7a29d2e57efc6405a1dce1437180e3ce27100000000001068a71465e76d736564b0c90f5cf3d0d7b69c461c36f69250ae27dbead147cc8f80bb80000000000000206354def8b7e6b2ee04bf85c00f5e79f173d0b76d5017bab3a90c7ba62e1722699000000000000010245f3ad9e63f629be6e278cc4cf34d3b0a79a4a0b27100000000000010404b154dbcd3c75580382c2353082df4390613d93c627120000000001011500cc7d9c2b460720a48cc7444d7e7dfe43f6050bb80a03000000015c8dec5f0eedf1f8934815ef8fb8cb8198eac6520bb80a030000010286f3dd3b4d08de718d7909b0fdc16f4cbdf94ef527100000000000c001a0d4c12f6433ff6ea0573633364c030d8b46ed5764494f80eb434f27060c39f315a034df82c4ac185a666280d578992feee0c05fc75d93e3e2286726c85fba1bb0a0",
      "0x02f8f68221058305c7b3830f4ef58401a5485d832dc6c094f2cb4e685946beecbc9ce5f318b68edc583bcfa080b88600000000000069073af31b777ac6b2082fc399fde92a814114b7896ca0b0503106910ea099d5e32c93bfc0013ed2850534c3f8583ab7276414416c0d15ac021126f6cb6ca1ed091ddc01eb01000202b6e39c63c7e4ebc01d51f845dfc9cff3f5adf9ef2710000000000103cd1f9777571493aeacb7eae45cd30a226d3e612d4e200000000000c080a09694b95dc893bed698ede415c188db3530ccc98a01d79bb9f11d783de7dddde9a0275b0165ab21ea0e6f721c624aa2270a3f98276ca0c95381d90e3f9d434b4881",
      "0x02f8f682210583034573830f4ef58401a5485d832dc6c094f2cb4e685946beecbc9ce5f318b68edc583bcfa080b88600000000000069073af31c970da8f2adb8bafe6d254ec4428f8342508e169f75e8450f6ff8488813dfa638395e16787966f01731fddffd0e7352cde07fd24bba283bd27f1828fb2a0c700701000202b6e39c63c7e4ebc01d51f845dfc9cff3f5adf9ef2710000000000103cd1f9777571493aeacb7eae45cd30a226d3e612d4e200000000000c080a00181afe4bedab67692a9c1ff30a89fde6b3d3c8407a47a2777efcd6bdc0c39d2a022d6a4219e72eebdbc5d31ae998243ccec1b192c5c7c586308ccddb4838cd631",
      "0x02f8c1822105830b0cfd830f4ed084013bce1b834c4b4094d599955d17a1378651e76557ffc406c71300fcb080b851020026000100271000c8e9d514f85b57b70de033e841d788ab4df1acd691802acc26dcd13fb9e38fa8e10001004e2000c8e9d55bd42770e29cb76904377ffdb22737fc9f5eb36fde875fcbfa687b1c3023c001a0d87c4e16986db55b8846bccfe7bca824b75216e72d8f92369c46681800285cb2a00ec53251be3c2a0d19884747d123ddb0ada3c0a917b21882e297e95c2294d52a",
      "0x02f901d58221058306361d830f4240840163efbc8301546194833589fcd6edb6e08f4c7c32d4f71b54bda0291380b90164cf092995000000000000000000000000d723d9f752c19faf88a5fd2111a38d0cc5d395b00000000000000000000000000b55712de2ce8f93be30d53c03d48ea275cd14d000000000000000000000000000000000000000000000000000000000000003e8000000000000000000000000000000000000000000000000000000006907385e0000000000000000000000000000000000000000000000000000000069073be2bef9866b70d0bb74d8763996eb5967b1b24cd48f7801f94ad80cb49431df6b1d00000000000000000000000000000000000000000000000000000000000000e000000000000000000000000000000000000000000000000000000000000000417c9c2382c6c3f029aa3dcbf1df075366fae7bc9fba7f3729713e0bf4d518951f5340350208db96af23686d9985ce552e3588244456a23ca99ecbcae779ea11e71c00000000000000000000000000000000000000000000000000000000000000c080a0b1090c8c67ca9a49ba3591c72c8851f187bbfc39b1920dff2f6c0157ed1ada39a0265b7f704f4c1b5c2c5ca57f1a4040e1e48878c9ad5f2cca9c4e6669d12989f2",
      "0x02f8c1822105830b0c98830f424084013bc18b834c4b4094d599955d17a1378651e76557ffc406c71300fcb080b851020026000100271000c8e9d514f85b57b70de033e841d788ab4df1acd691802acc26dcd13fb9e38fa8e10001004e2000c8e9d55bd42770e29cb76904377ffdb22737fc9f5eb36fde875fcbfa687b1c3023c001a080a96d18ae46b58d9a470846a05b394ab4a49a2e379de1941205684e1ac291f9a01e6d4d2c6bab5bf8b89f1df2d6beb85d9f1b3f3be73ca2b72e4ad2d9da0d12d2",
      "0x02f901d48221058231e0830f4240840163efbc8301544d94833589fcd6edb6e08f4c7c32d4f71b54bda0291380b90164cf0929950000000000000000000000001de8dbc2409c4bbf14445b0d404bb894f0c6cff70000000000000000000000008d8fa42584a727488eeb0e29405ad794a105bb9b0000000000000000000000000000000000000000000000000000000000002710000000000000000000000000000000000000000000000000000000006907385d0000000000000000000000000000000000000000000000000000000069073af16b129c414484e011621c44e0b32451fdbd69e63ef4919f427dde08c16cb199b100000000000000000000000000000000000000000000000000000000000000e00000000000000000000000000000000000000000000000000000000000000041ae0a4b618c30f0e5d92d7fe99bb435413b2201711427699fd285f69666396cee76199d4e901cfb298612cb3b8ad06178cefb4136a8bc1be07c01b5fea80e5ec11b00000000000000000000000000000000000000000000000000000000000000c080a0af315068084aae367f00263dbd872908bbb9ceaefd6b792fc48dd357e6bdf8afa01e7f0e5913570394b9648939ef71fc5ac34fe320a2757ec388316731a335e69f",
      "0x02f9022f82210583052d0b830f423f84025c5527833d090094c90d989d809e26b2d95fb72eb3288fef72af8c2f80b901be00000000000069073af31cf0f932cecc8c4c6ffffa554a63e8fba251434483ed3903966d2ba5a70121618a1c45bd9ee158192ab8d7e12ce0f447f2848a48aedaa89e0efa8637bb931745de05000202b6e39c63c7e4ebc01d51f845dfc9cff3f5adf9ef2710000000000103cd1f9777571493aeacb7eae45cd30a226d3e612d4e2000000000000003045a9ad2bb92b0b3e5c571fdd5125114e04e02be1a0bb80000000001036e55486ea6b8691ba58224f3cae35505add86c372710000000000003681d6e4b0b020656ca04956ddaf76add7ef022f60dac0000000001010206777762d3eb91810b15526c2c9102864d722ef7a9ed24e77271c1dcbf0fdcba68138800000000010698c8f03094a9e65ccedc14c40130e4a5dd0ce14fb12ea58cbeac11f662b458b9271000000000000002005554419ccd0293d9383901f461c7c3e0c66e925f0bb80000000001028eb9437532fac8d6a7870f3f887b7978d20355fc271000000000000003035d28f920c9d23100e4a38b2ba2d8ae617c3b261501f4000000000102bc51db8aec659027ae0b0e468c0735418161a7800bb8000000000003dbc6998296caa1652a810dc8d3baf4a8294330f100500000000000c080a040000b130b1759df897a9573691a3d1cafacc6d95d0db1826f275afc30e2ff63a0400a7514f8d5383970c4412205ec8e9c6ca06acea504acabd2d3c36e9cb5003d"
    ],
    "withdrawals": [],
    "withdrawals_root": "0x81864c23f426ad807d66c9fdde33213e1fdbac06c1b751d279901d1ce13670ac"
  },
  "index": 10,
  "metadata": {
    "block_number": 37646058,
    "new_account_balances": {
      "0x000000000022d473030f116ddee9f6b43ac78ba3": "0x0",
      "0x0000000071727de22e5e9d8baf0edac6f37da032": "0x23281e39594556899",
      "0x0000f90827f1c53a10cb7a02335b175320002935": "0x0",
      "0x000f3df6d732807ef1319fb7b8bb8522d0beac02": "0x0"
    },
    "receipts": {
      "0x1a766690fd6d0febffc488f12fbd7385c43fbe1e07113a1316f22f176355297e": {
        "Legacy": {
          "cumulativeGasUsed": "0x2868d76",
          "logs": [
            {
              "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
              "data": "0x0000000000000000000000000000000000000000000000000000000004b2ee6f",
              "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                "0x00000000000000000000000001c2c79343de52f99538cd2cbbd67ba0813f4030",
                "0x000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e222"
              ]
            },
            {
              "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
              "data": "0x0000000000000000000000000000000000000000000000000000000004b2ee6f",
              "topics": [
                "0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925",
                "0x000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e222",
                "0x0000000000000000000000000000000000001ff3684f28c67538d4d072c22734"
              ]
            },
            {
              "address": "0xf5042e6ffac5a625d4e7848e0b01373d8eb9e222",
              "data": "0x000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000044095ea7b30000000000000000000000000000000000001ff3684f28c67538d4d072c227340000000000000000000000000000000000000000000000000000000004b2ee6f00000000000000000000000000000000000000000000000000000000",
              "topics": [
                "0x93485dcd31a905e3ffd7b012abe3438fa8fa77f98ddc9f50e879d3fa7ccdc324"
              ]
            },
            {
              "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
              "data": "0x00000000000000000000000000000000000000000000000000000000000133f4",
              "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                "0x000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e222",
                "0x000000000000000000000000f70da97812cb96acdf810712aa562db8dfa3dbef"
              ]
            },
            {
              "address": "0xf5042e6ffac5a625d4e7848e0b01373d8eb9e222",
              "data": "0x000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e2220000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001243b2253c8000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000e00000000000000000000000000000000000000000000000000000000000000001000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda029130000000000000000000000000000000000000000000000000000000000000001000000000000000000000000f70da97812cb96acdf810712aa562db8dfa3dbef000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000133f400000000000000000000000000000000000000000000000000000000",
              "topics": [
                "0x93485dcd31a905e3ffd7b012abe3438fa8fa77f98ddc9f50e879d3fa7ccdc324"
              ]
            },
            {
              "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
              "data": "0x0000000000000000000000000000000000000000000000000000000004b1ba7b",
              "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                "0x000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e222",
                "0x000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177"
              ]
            },
            {
              "address": "0x8f360baf899845441eccdc46525e26bb8860752a",
              "data": "0x00000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000001957cc57b7a9959c0000000000000000000000000000000000000000000000001957cc57b7a9959800000000000000000000000000000000000000000000000444e308096a22c339000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000092458cc3a866f04600000000000000000000000000000000000000000000000025f3e27916e84b59000",
              "topics": [
                "0x4e1d56f7310a8c32b2267f756b19ba65019b4890068ce114a25009abe54de5ba"
              ]
            },
            {
              "address": "0xba12222222228d8ba445958a75a0704d566bf2c8",
              "data": "0x0000000000000000000000000000000000000000000000000000000004b1ba7b0000000000000000000000000000000000000000000000000000000004b1a44c",
              "topics": [
                "0x2170c741c41531aec20e7c107c24eecfdd15e69c9bb0a8dd37b1840b9e0b207b",
                "0x8f360baf899845441eccdc46525e26bb8860752a0002000000000000000001cd",
                "0x000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                "0x000000000000000000000000d9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca"
              ]
            },
            {
              "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
              "data": "0x0000000000000000000000000000000000000000000000000000000004b1ba7b",
              "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                "0x000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177",
                "0x000000000000000000000000ba12222222228d8ba445958a75a0704d566bf2c8"
              ]
            },
            {
              "address": "0xd9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca",
              "data": "0x0000000000000000000000000000000000000000000000000000000004b1a44c",
              "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                "0x000000000000000000000000ba12222222228d8ba445958a75a0704d566bf2c8",
                "0x000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177"
              ]
            },
            {
              "address": "0xd9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca",
              "data": "0x0000000000000000000000000000000000000000000000000000000004b1a44c",
              "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                "0x000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177",
                "0x000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e222"
              ]
            },
            {
              "address": "0xf5042e6ffac5a625d4e7848e0b01373d8eb9e222",
              "data": "0x0000000000000000000000000000000000001ff3684f28c67538d4d072c227340000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000007e42213bc0b000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda029130000000000000000000000000000000000000000000000000000000004b1ba7b000000000000000000000000ea758cac6115309b325c582fd0782d79e350217700000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000007041fff991f000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e222000000000000000000000000d9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca0000000000000000000000000000000000000000000000000000000004b06d9200000000000000000000000000000000000000000000000000000000000000a0d311e79cd2099f6f1f0607040000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000300000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000000000180000000000000000000000000000000000000000000000000000000000000058000000000000000000000000000000000000000000000000000000000000000e4c1fb425e000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda029130000000000000000000000000000000000000000000000000000000004b1ba7b00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000069073bb900000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003c438c9c147000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda029130000000000000000000000000000000000000000000000000000000000002710000000000000000000000000ba12222222228d8ba445958a75a0704d566bf2c800000000000000000000000000000000000000000000000000000000000001c400000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000002e4945bcec9000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001200000000000000000000000000000000000000000000000000000000000000220000000000000000000000000ea758cac6115309b325c582fd0782d79e35021770000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ea758cac6115309b325c582fd0782d79e3502177000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002800000000000000000000000000000000000000000000000000000000069073bb9000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000208f360baf899845441eccdc46525e26bb8860752a0002000000000000000001cd000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000004b1ba7b00000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913000000000000000000000000d9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca00000000000000000000000000000000000000000000000000000000000000027fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008434ee90ca000000000000000000000000f5c4f3dc02c3fb9279495a8fef7b0741da956157000000000000000000000000d9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca0000000000000000000000000000000000000000000000000000000004b1a7880000000000000000000000000000000000000000000000000000000000002710000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
              "topics": [
                "0x93485dcd31a905e3ffd7b012abe3438fa8fa77f98ddc9f50e879d3fa7ccdc324"
              ]
            },
            {
              "address": "0xd9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca",
              "data": "0x0000000000000000000000000000000000000000000000000000000004b1a44c",
              "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                "0x000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e222",
                "0x00000000000000000000000001c2c79343de52f99538cd2cbbd67ba0813f4030"
              ]
            },
            {
              "address": "0xf5042e6ffac5a625d4e7848e0b01373d8eb9e222",
              "data": "0x000000000000000000000000f5042e6ffac5a625d4e7848e0b01373d8eb9e2220000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001243b2253c8000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000e00000000000000000000000000000000000000000000000000000000000000001000000000000000000000000d9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca000000000000000000000000000000000000000000000000000000000000000100000000000000000000000001c2c79343de52f99538cd2cbbd67ba0813f40300000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
              "topics": [
                "0x93485dcd31a905e3ffd7b012abe3438fa8fa77f98ddc9f50e879d3fa7ccdc324"
              ]
            }
          ],
          "status": "0x1"
        }
      },
      "0x2cd6b4825b5ee40b703c947e15630336dceda97825b70412da54ccc27f484496": {
        "Eip1559": {
          "cumulativeGasUsed": "0x28cca69",
          "logs": [
            {
              "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
              "data": "0x",
              "topics": [
                "0x98de503528ee59b575ef0c0a2576a82497bfc029a5685b209e9ec333479b10a5",
                "0x000000000000000000000000d723d9f752c19faf88a5fd2111a38d0cc5d395b0",
                "0xbef9866b70d0bb74d8763996eb5967b1b24cd48f7801f94ad80cb49431df6b1d"
              ]
            },
            {
              "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
              "data": "0x00000000000000000000000000000000000000000000000000000000000003e8",
              "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                "0x000000000000000000000000d723d9f752c19faf88a5fd2111a38d0cc5d395b0",
                "0x0000000000000000000000000b55712de2ce8f93be30d53c03d48ea275cd14d0"
              ]
            }
          ],
          "status": "0x1"
        }
      }
    }
  },
  "payload_id": "0x0316ecb1aa1671b5"
}"#;

        let flashblock: OpFlashblockPayload = serde_json::from_str(raw).expect("deserialize");
        let serialized = serde_json::to_string(&flashblock).expect("serialize");
        let roundtrip: OpFlashblockPayload = serde_json::from_str(&serialized).expect("roundtrip");

        assert_eq!(flashblock, roundtrip);
    }

    // ========================================================================
    // Tests for `access_list_up_to` and `try_to_buildable_args` access-list flow
    // ========================================================================

    use alloy_eip7928::{AccountChanges, BalanceChange};
    use alloy_primitives::{address, Address, U256};

    /// Helper: minimal `AccountChanges` with balance changes and storage reads.
    fn make_account_changes(
        addr: Address,
        balance_changes: Vec<(u64, u64)>,
        storage_reads: Vec<u64>,
    ) -> AccountChanges {
        AccountChanges {
            address: addr,
            balance_changes: balance_changes
                .into_iter()
                .map(|(tx_idx, bal)| BalanceChange {
                    block_access_index: tx_idx,
                    post_balance: U256::from(bal),
                })
                .collect(),
            storage_reads: storage_reads.into_iter().map(U256::from).collect(),
            ..Default::default()
        }
    }

    const ADDR_A: Address = address!("0000000000000000000000000000000000000001");
    const ADDR_B: Address = address!("0000000000000000000000000000000000000002");

    #[test]
    fn test_access_list_up_to_merges_across_flashblocks() {
        // Covers: same-address merge with disjoint tx indices preserved, and
        // separate entry for a distinct address in a later flashblock.
        let factory = TestFlashBlockFactory::new();
        let ac0_a = make_account_changes(ADDR_A, vec![(0, 100)], vec![]);
        let ac1_a = make_account_changes(ADDR_A, vec![(5, 200)], vec![]);
        let ac1_b = make_account_changes(ADDR_B, vec![(6, 300)], vec![]);
        let fb0 = factory.flashblock_at(0).access_list(Some(vec![ac0_a])).build();
        let fb1 = factory.flashblock_after(&fb0).access_list(Some(vec![ac1_a, ac1_b])).build();

        let mut cache = TestRawCache::new(false);
        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        cache.handle_flashblock(wrap(fb1)).expect("fb1 insert");

        let args = cache.try_get_buildable_args(100).expect("should be buildable");
        let al = args.access_list.expect("access list present");
        assert_eq!(al.len(), 2, "ADDR_A merged into one entry, ADDR_B separate");
        // Output sorted by address ascending.
        assert_eq!(al[0].address, ADDR_A);
        assert_eq!(al[1].address, ADDR_B);

        // ADDR_A carries both flashblocks' balance changes with exact values preserved.
        assert_eq!(al[0].balance_changes.len(), 2);
        let a_changes: Vec<(u64, U256)> =
            al[0].balance_changes.iter().map(|c| (c.block_access_index, c.post_balance)).collect();
        assert!(a_changes.contains(&(0, U256::from(100))));
        assert!(a_changes.contains(&(5, U256::from(200))));

        // ADDR_B carries only fb1's single change.
        assert_eq!(al[1].balance_changes.len(), 1);
        assert_eq!(al[1].balance_changes[0].block_access_index, 6);
        assert_eq!(al[1].balance_changes[0].post_balance, U256::from(300));
    }

    #[test]
    fn test_access_list_up_to_dedupes_storage_reads() {
        // Same slot read in two flashblocks — must appear once in storage_reads.
        let factory = TestFlashBlockFactory::new();
        let ac0 = make_account_changes(ADDR_A, vec![], vec![1, 2]);
        let ac1 = make_account_changes(ADDR_A, vec![], vec![1, 3]);
        let fb0 = factory.flashblock_at(0).access_list(Some(vec![ac0])).build();
        let fb1 = factory.flashblock_after(&fb0).access_list(Some(vec![ac1])).build();

        let mut cache = TestRawCache::new(false);
        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        cache.handle_flashblock(wrap(fb1)).expect("fb1 insert");

        let args = cache.try_get_buildable_args(100).expect("should be buildable");
        let entry = &args.access_list.expect("present")[0];
        // Exact dedup + sort: slot 1 once, 2 and 3 retained, ascending order.
        assert_eq!(entry.storage_reads, vec![U256::from(1), U256::from(2), U256::from(3)]);
    }

    #[test]
    fn test_access_list_up_to_skips_missing_and_returns_none_when_all_missing() {
        // Covers two behaviors: a missing middle flashblock is skipped (not
        // treated as an abort), and if ALL flashblocks lack access list data
        // the aggregated output is `None`.
        let factory = TestFlashBlockFactory::new();
        let ac0 = make_account_changes(ADDR_A, vec![(0, 100)], vec![]);
        let fb0 = factory.flashblock_at(0).access_list(Some(vec![ac0])).build();
        let fb1 = factory.flashblock_after(&fb0).access_list(None).build();

        let mut cache = TestRawCache::new(false);
        cache.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        cache.handle_flashblock(wrap(fb1)).expect("fb1 insert");

        // Partial data preserved with exact value survived fb1's absence.
        let args = cache.try_get_buildable_args(100).expect("should be buildable");
        let al = args.access_list.expect("fb0 data preserved despite fb1 having None");
        assert_eq!(al.len(), 1);
        assert_eq!(al[0].address, ADDR_A);
        assert_eq!(al[0].balance_changes.len(), 1);
        assert_eq!(al[0].balance_changes[0].block_access_index, 0);
        assert_eq!(al[0].balance_changes[0].post_balance, U256::from(100));

        // Swap in a cache where both flashblocks lack access list data.
        let fb0_empty = factory.flashblock_at(0).access_list(None).build();
        let fb1_empty = factory.flashblock_after(&fb0_empty).access_list(None).build();
        let mut empty_cache = TestRawCache::new(false);
        empty_cache.handle_flashblock(wrap(fb0_empty)).expect("fb0 insert");
        empty_cache.handle_flashblock(wrap(fb1_empty)).expect("fb1 insert");
        let args = empty_cache.try_get_buildable_args(100).expect("should be buildable");
        assert!(args.access_list.is_none(), "no data at all → None");
    }

    #[test]
    fn test_buildable_args_disable_flag_strips_access_list() {
        // With the disable flag set, even a flashblock carrying access list data
        // must produce `BuildArgs.access_list == None`.
        let factory = TestFlashBlockFactory::new();
        let ac = make_account_changes(ADDR_A, vec![(0, 100)], vec![]);
        let fb0 = factory.flashblock_at(0).access_list(Some(vec![ac])).build();

        let mut cache_enabled = TestRawCache::new(false);
        cache_enabled.handle_flashblock(wrap(fb0.clone())).expect("fb0 insert");
        let args = cache_enabled.try_get_buildable_args(100).expect("should be buildable");
        let al = args.access_list.expect("flag off + data → access list surfaced");
        assert_eq!(al.len(), 1);
        assert_eq!(al[0].address, ADDR_A);
        assert_eq!(al[0].balance_changes.len(), 1);
        assert_eq!(al[0].balance_changes[0].block_access_index, 0);
        assert_eq!(al[0].balance_changes[0].post_balance, U256::from(100));

        let mut cache_disabled = TestRawCache::new(true);
        cache_disabled.handle_flashblock(wrap(fb0)).expect("fb0 insert");
        let args = cache_disabled.try_get_buildable_args(100).expect("should be buildable");
        assert!(args.access_list.is_none(), "flag on → access list stripped");
    }

    // ========================================================================
    // Verification tests for `access_list_up_to` against reth's
    // `bal_to_hashed_post_state` consumer contract:
    //   * per-tx-index vectors: `.last()` returns the entry with the largest
    //     `block_access_index`.
    //   * per-slot final value: linear iter + insert keyed by slot
    //     (last-iteration-wins) yields the actual final post-state.
    // ========================================================================

    use alloy_eip7928::{CodeChange, NonceChange, SlotChanges, StorageChange};
    use alloy_primitives::Bytes;
    use std::collections::HashMap;

    /// Slot/value list per slot used by [`make_account_changes_full`].
    type SlotChangeSpec = (U256, Vec<(u64, U256)>);

    /// Richer counterpart to [`make_account_changes`] — accepts nonce and
    /// storage in addition to balance. Per-tx-index vectors are emitted
    /// already sorted ascending by `block_access_index` to mirror what the
    /// builder produces on the wire post `f699366`.
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

    /// Mimics reth's `bal_to_hashed_post_state` per-slot extraction: iterate
    /// outer `storage_changes` linearly, insert keyed by slot, last wins.
    fn extract_final_storage(ac: &AccountChanges) -> HashMap<U256, U256> {
        let mut out = HashMap::new();
        for sc in &ac.storage_changes {
            if let Some(last) = sc.changes.last() {
                out.insert(sc.slot, last.new_value);
            }
        }
        out
    }

    fn aggregate_for(addr: Address, fbs: Vec<OpFlashblockPayload>) -> AccountChanges {
        let mut cache = TestRawCache::new(false);
        for fb in fbs {
            cache.handle_flashblock(wrap(fb)).expect("flashblock insert");
        }
        cache
            .try_get_buildable_args(100)
            .expect("buildable")
            .access_list
            .expect("access list present")
            .into_iter()
            .find(|ac| ac.address == addr)
            .expect("address present in aggregated access list")
    }

    #[test]
    fn last_balance_change_resolves_across_three_flashblocks() {
        let factory = TestFlashBlockFactory::new();
        let mk = |bal: Vec<(u64, U256)>| make_account_changes_full(ADDR_A, bal, vec![], vec![]);
        let fb0 = factory
            .flashblock_at(0)
            .access_list(Some(vec![mk(vec![(2, U256::from(20)), (5, U256::from(50))])]))
            .build();
        let fb1 = factory
            .flashblock_after(&fb0)
            .access_list(Some(vec![mk(vec![(8, U256::from(80)), (11, U256::from(110))])]))
            .build();
        let fb2 = factory
            .flashblock_after(&fb1)
            .access_list(Some(vec![mk(vec![(15, U256::from(150)), (20, U256::from(200))])]))
            .build();

        let merged = aggregate_for(ADDR_A, vec![fb0, fb1, fb2]);

        // .last() must return tx_idx 20's value (the global max).
        assert_eq!(merged.balance_changes.last().unwrap().post_balance, U256::from(200));
    }

    #[test]
    fn last_nonce_change_resolves_across_three_flashblocks() {
        let factory = TestFlashBlockFactory::new();
        let mk = |n: Vec<(u64, u64)>| make_account_changes_full(ADDR_A, vec![], n, vec![]);
        let fb0 =
            factory.flashblock_at(0).access_list(Some(vec![mk(vec![(1, 1), (3, 2)])])).build();
        let fb1 = factory
            .flashblock_after(&fb0)
            .access_list(Some(vec![mk(vec![(7, 3), (9, 4)])]))
            .build();
        let fb2 = factory
            .flashblock_after(&fb1)
            .access_list(Some(vec![mk(vec![(12, 5), (18, 6)])]))
            .build();

        let merged = aggregate_for(ADDR_A, vec![fb0, fb1, fb2]);

        assert_eq!(merged.nonce_changes.last().unwrap().new_nonce, 6);
    }

    #[test]
    fn same_slot_modified_in_every_flashblock_resolves_to_latest_write() {
        // Same slot in every FB → outer vec carries 3 SlotChanges entries
        // for that slot. Last-iteration-wins must converge to FB2's tx 20.
        let factory = TestFlashBlockFactory::new();
        let slot = U256::from(0x1);
        let mk = |c: Vec<(u64, U256)>| {
            make_account_changes_full(ADDR_A, vec![], vec![], vec![(slot, c)])
        };
        let fb0 = factory
            .flashblock_at(0)
            .access_list(Some(vec![mk(vec![(2, U256::from(200)), (4, U256::from(400))])]))
            .build();
        let fb1 = factory
            .flashblock_after(&fb0)
            .access_list(Some(vec![mk(vec![(8, U256::from(800)), (10, U256::from(1000))])]))
            .build();
        let fb2 = factory
            .flashblock_after(&fb1)
            .access_list(Some(vec![mk(vec![(15, U256::from(1500)), (20, U256::from(2000))])]))
            .build();

        let merged = aggregate_for(ADDR_A, vec![fb0, fb1, fb2]);

        // Upstream `StorageBal::extend` is `BTreeMap`-backed and merges per-slot
        // writes — one `SlotChanges` entry per slot, regardless of how many
        // flashblocks touched it.
        assert_eq!(merged.storage_changes.len(), 1, "duplicate slots are deduped on aggregation");
        let sc = &merged.storage_changes[0];
        assert_eq!(sc.slot, slot);
        // All 6 writes (2 per FB across 3 FBs) are flattened into one sorted vec.
        assert_eq!(sc.changes.len(), 6);
        assert_eq!(sc.changes.last().unwrap().new_value, U256::from(2000));
        assert_eq!(extract_final_storage(&merged).get(&slot).copied(), Some(U256::from(2000)));
    }

    #[test]
    fn slot_modified_with_gap_resolves_to_latest_touching_flashblock() {
        // Slot in FB0 and FB2 only; FB1 touches the address via balance.
        let factory = TestFlashBlockFactory::new();
        let slot = U256::from(0x1);
        let fb0 = factory
            .flashblock_at(0)
            .access_list(Some(vec![make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(slot, vec![(3, U256::from(100))])],
            )]))
            .build();
        let fb1 = factory
            .flashblock_after(&fb0)
            .access_list(Some(vec![make_account_changes_full(
                ADDR_A,
                vec![(7, U256::from(7777))],
                vec![],
                vec![],
            )]))
            .build();
        let fb2 = factory
            .flashblock_after(&fb1)
            .access_list(Some(vec![make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(slot, vec![(13, U256::from(999))])],
            )]))
            .build();

        let merged = aggregate_for(ADDR_A, vec![fb0, fb1, fb2]);

        // FB2 (tx 13) wins for storage; FB1 (tx 7) wins for balance.
        assert_eq!(extract_final_storage(&merged).get(&slot).copied(), Some(U256::from(999)));
        assert_eq!(merged.balance_changes.last().unwrap().post_balance, U256::from(7777));
    }

    #[test]
    fn multi_slot_each_resolves_independently_across_flashblocks() {
        // s1: FB0(tx2) + FB2(tx14)              → FB2 wins
        // s2: FB1 only (tx6, tx8)               → FB1 tx8 wins
        // s3: FB0(tx4) + FB1(tx7) + FB2(tx12)   → FB2 wins
        let factory = TestFlashBlockFactory::new();
        let (s1, s2, s3) = (U256::from(0x1), U256::from(0x2), U256::from(0x3));
        let mk = |c: Vec<SlotChangeSpec>| make_account_changes_full(ADDR_A, vec![], vec![], c);
        let fb0 = factory
            .flashblock_at(0)
            .access_list(Some(vec![mk(vec![
                (s1, vec![(2, U256::from(11))]),
                (s3, vec![(4, U256::from(33))]),
            ])]))
            .build();
        let fb1 = factory
            .flashblock_after(&fb0)
            .access_list(Some(vec![mk(vec![
                (s2, vec![(6, U256::from(60)), (8, U256::from(80))]),
                (s3, vec![(7, U256::from(70))]),
            ])]))
            .build();
        let fb2 = factory
            .flashblock_after(&fb1)
            .access_list(Some(vec![mk(vec![
                (s1, vec![(14, U256::from(140))]),
                (s3, vec![(12, U256::from(120))]),
            ])]))
            .build();

        let storage = extract_final_storage(&aggregate_for(ADDR_A, vec![fb0, fb1, fb2]));

        assert_eq!(storage.get(&s1).copied(), Some(U256::from(140)));
        assert_eq!(storage.get(&s2).copied(), Some(U256::from(80)));
        assert_eq!(storage.get(&s3).copied(), Some(U256::from(120)));
    }

    #[test]
    fn aggregator_storage_read_then_write_across_flashblocks_promotes_to_changes() {
        // FB0 reads `slot` (no writes); FB1 writes `slot` at tx 5.
        // Aggregated output: `slot` lives in storage_changes, not storage_reads.
        let factory = TestFlashBlockFactory::new();
        let slot = U256::from(0x42);
        let fb0 = factory
            .flashblock_at(0)
            .access_list(Some(vec![make_account_changes(ADDR_A, vec![], vec![0x42])]))
            .build();
        let fb1 = factory
            .flashblock_after(&fb0)
            .access_list(Some(vec![make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(slot, vec![(5, U256::from(100))])],
            )]))
            .build();

        let merged = aggregate_for(ADDR_A, vec![fb0, fb1]);

        assert!(
            merged.storage_reads.is_empty(),
            "later write must promote slot out of storage_reads"
        );
        assert_eq!(merged.storage_changes.len(), 1, "exactly one slot in storage_changes");
        let sc = &merged.storage_changes[0];
        assert_eq!(sc.slot, slot);
        assert_eq!(sc.changes.len(), 1, "single write at tx 5 survives");
        assert_eq!(sc.changes[0].block_access_index, 5);
        assert_eq!(sc.changes[0].new_value, U256::from(100));
    }

    #[test]
    fn aggregator_storage_write_then_read_across_flashblocks_stays_as_changes() {
        // FB0 writes `slot` at tx 3; FB1 reads `slot` (no writes).
        // The later read must NOT demote the earlier write.
        let factory = TestFlashBlockFactory::new();
        let slot = U256::from(0x99);
        let fb0 = factory
            .flashblock_at(0)
            .access_list(Some(vec![make_account_changes_full(
                ADDR_A,
                vec![],
                vec![],
                vec![(slot, vec![(3, U256::from(777))])],
            )]))
            .build();
        let fb1 = factory
            .flashblock_after(&fb0)
            .access_list(Some(vec![make_account_changes(ADDR_A, vec![], vec![0x99])]))
            .build();

        let merged = aggregate_for(ADDR_A, vec![fb0, fb1]);

        assert!(
            merged.storage_reads.is_empty(),
            "slot already has writes — must not appear as a bare read",
        );
        assert_eq!(merged.storage_changes.len(), 1);
        let sc = &merged.storage_changes[0];
        assert_eq!(sc.slot, slot);
        assert_eq!(sc.changes.len(), 1, "the FB0 write must survive across the FB1 read");
        assert_eq!(sc.changes[0].block_access_index, 3);
        assert_eq!(sc.changes[0].new_value, U256::from(777));
    }

    #[test]
    fn aggregator_code_changes_merge_across_flashblocks_with_last_extraction() {
        // FB0 deploys code at tx 2; FB1 redeploys at tx 8 (e.g. CREATE2 reuse).
        // After aggregation, code_changes contains both entries, ordered with
        // FB0's write first and FB1's last — matching `.last()` semantics that
        // reth's BAL→hashed-state consumer relies on.
        let factory = TestFlashBlockFactory::new();
        let code_v1 = Bytes::from(vec![0x60, 0x00]);
        let code_v2 = Bytes::from(vec![0x61, 0xff, 0xff]);

        let mk = |bai: u64, code: Bytes| AccountChanges {
            address: ADDR_A,
            code_changes: vec![CodeChange { block_access_index: bai, new_code: code }],
            ..Default::default()
        };

        let fb0 = factory.flashblock_at(0).access_list(Some(vec![mk(2, code_v1.clone())])).build();
        let fb1 =
            factory.flashblock_after(&fb0).access_list(Some(vec![mk(8, code_v2.clone())])).build();

        let merged = aggregate_for(ADDR_A, vec![fb0, fb1]);

        assert_eq!(merged.code_changes.len(), 2, "both flashblocks' code writes survive merge");
        assert_eq!(merged.code_changes[0].block_access_index, 2);
        assert_eq!(merged.code_changes[0].new_code, code_v1);
        let last = merged.code_changes.last().expect("non-empty");
        assert_eq!(last.block_access_index, 8, ".last() must yield the higher index");
        assert_eq!(last.new_code, code_v2);
    }
}
