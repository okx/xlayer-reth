mod confirm;
pub mod pending;
pub(crate) mod raw;
pub(crate) mod task;
pub(crate) mod utils;

pub(crate) use confirm::ConfirmCache;
pub use pending::PendingSequence;
pub(crate) use raw::RawFlashblocksCache;
pub(crate) use task::ExecutionTaskQueue;

use crate::PendingSequenceRx;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::*;

use alloy_consensus::BlockHeader;
use alloy_primitives::{TxHash, B256};
use alloy_rpc_types_eth::{BlockId, BlockNumberOrTag};

use reth_chain_state::{CanonicalInMemoryState, ExecutedBlock, MemoryOverlayStateProvider};
use reth_primitives_traits::{NodePrimitives, ReceiptTy, SealedHeaderFor};
use reth_rpc_eth_types::block::BlockAndReceipts;
use reth_storage_api::StateProviderBox;
use reth_trie_db::ChangesetCache;

/// The minimum number of blocks to retain in the changeset cache after eviction.
///
/// This ensures that recent trie changesets are kept in memory for potential reorgs,
/// even when the finalized block is not set (e.g., on L2s like Optimism).
const CHANGESET_CACHE_RETENTION_BLOCKS: u64 = 64;

/// Cached transaction info (block context, receipt and tx data) for O(1) lookups
/// by transaction hash.
#[derive(Debug, Clone)]
pub struct CachedTxInfo<N: NodePrimitives> {
    /// Block number containing the transaction.
    pub block_number: u64,
    /// Block hash containing the transaction.
    pub block_hash: B256,
    /// Index of the transaction within the block.
    pub tx_index: u64,
    /// The signed transaction.
    pub tx: N::SignedTx,
    /// The corresponding receipt.
    pub receipt: ReceiptTy<N>,
}

/// Top-level controller state cache for the flashblocks RPC layer.
///
/// Pure data store composed of:
/// - **Pending**: the in-progress flashblock sequence being built from incoming
///   `OpFlashblockPayload` deltas (at most one active sequence at a time).
/// - **Confirmed**: completed flashblock sequences that have been committed but
///   are still ahead of the canonical chain.
///
/// This cache is a **data source** — it does not wrap a provider or implement
/// any reth provider traits. The RPC override handler decides when to query
/// this cache vs the underlying chainstate provider.
///
/// Uses `Arc<RwLock>` for thread safety — a single lock protects all inner
/// state, ensuring atomic operations across pending, confirmed, and height
/// state (e.g. reorg detection + flush + insert in `handle_confirmed_block`).
#[derive(Debug, Clone)]
pub struct FlashblockStateCache<N: NodePrimitives> {
    inner: Arc<RwLock<FlashblockStateCacheInner<N>>>,
    changeset_cache: ChangesetCache,
    pub(crate) canon_in_memory_state: CanonicalInMemoryState<N>,
    pub(crate) task_queue: ExecutionTaskQueue,
}

// FlashblockStateCache read interfaces
impl<N: NodePrimitives> FlashblockStateCache<N> {
    /// Creates a new [`FlashblockStateCache`].
    pub fn new(canon_in_memory_state: CanonicalInMemoryState<N>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(FlashblockStateCacheInner::new())),
            changeset_cache: ChangesetCache::new(),
            canon_in_memory_state,
            task_queue: ExecutionTaskQueue::new(),
        }
    }
}

// FlashblockStateCache read height interfaces
impl<N: NodePrimitives> FlashblockStateCache<N> {
    /// Returns the changeset cache.
    pub fn get_changeset_cache(&self) -> ChangesetCache {
        self.changeset_cache.clone()
    }

    /// Returns the current confirmed height.
    pub fn get_confirm_height(&self) -> u64 {
        self.inner.read().confirm_height
    }

    /// Return the current canonical height, if any.
    pub fn get_canon_height(&self) -> u64 {
        self.inner.read().canon_info.0
    }

    /// Returns a clone of the current pending sequence, if any.
    pub fn get_pending_sequence(&self) -> Option<PendingSequence<N>> {
        self.inner.read().pending_cache.clone()
    }

    pub fn get_rpc_block_by_id(&self, block_id: Option<BlockId>) -> Option<BlockAndReceipts<N>> {
        match block_id.unwrap_or(BlockId::Number(BlockNumberOrTag::Latest)) {
            BlockId::Number(id) => self.get_rpc_block(id),
            BlockId::Hash(hash) => self.get_block_by_hash(&hash.block_hash),
        }
    }

    /// Returns the current pending block and receipts, if any.
    pub fn get_rpc_block(&self, block_id: BlockNumberOrTag) -> Option<BlockAndReceipts<N>> {
        match block_id {
            BlockNumberOrTag::Pending => self.inner.read().get_pending_block(),
            BlockNumberOrTag::Latest => self.inner.read().get_confirmed_block(),
            BlockNumberOrTag::Number(num) => self.get_block_by_number(num),
            _ => None,
        }
    }

    /// Returns the block for the given block number, if cached.
    pub fn get_block_by_number(&self, num: u64) -> Option<BlockAndReceipts<N>> {
        self.inner.read().get_block_by_number(num)
    }

    /// Returns the confirmed block for the given block hash, if cached.
    pub fn get_block_by_hash(&self, hash: &B256) -> Option<BlockAndReceipts<N>> {
        self.inner.read().get_block_by_hash(hash)
    }

    /// Looks up cached transaction info by hash: pending sequence first, then
    /// confirm cache. Returns `None` if the tx is not in either cache layer.
    pub fn get_tx_info(&self, tx_hash: &TxHash) -> Option<(CachedTxInfo<N>, BlockAndReceipts<N>)> {
        self.inner.read().get_tx_info(tx_hash)
    }

    /// Returns a cloned watch receiver for pending sequence state updates.
    pub fn subscribe_pending_sequence(&self) -> PendingSequenceRx<N> {
        self.inner.read().subscribe_pending_sequence()
    }

    /// Instantiates a `MemoryOverlayStateProvider` that overlays the flashblock
    /// execution state on top of the canonical state for the given block ID.
    ///
    /// 1. Block number/hash - all block overlays in the cache up to that block.
    /// 2. `Pending` - all block overlays in the flashblocks state cache, which
    ///    includes the current pending executed block state.
    /// 3. `Latest` - all block overlays in the confirm cache up to the confirm
    ///    height.
    ///
    /// Returns `None` if the target block is not in the flashblocks cache.
    pub fn get_state_provider_by_id(
        &self,
        block_id: Option<BlockId>,
        canonical_state: StateProviderBox,
    ) -> Option<(StateProviderBox, SealedHeaderFor<N>)> {
        let guard = self.inner.read();
        let block = match block_id.unwrap_or(BlockId::Number(BlockNumberOrTag::Latest)) {
            BlockId::Number(id) => match id {
                BlockNumberOrTag::Pending => guard.get_pending_block(),
                BlockNumberOrTag::Latest => guard.get_confirmed_block(),
                BlockNumberOrTag::Number(num) => guard.get_block_by_number(num),
                _ => None,
            },
            BlockId::Hash(hash) => guard.get_block_by_hash(&hash.block_hash),
        }?
        .block;
        let block_num = block.number();
        let in_memory = guard.get_executed_blocks_up_to_height(block_num);
        drop(guard);

        let in_memory = match in_memory {
            Ok(blocks) => blocks,
            Err(e) => {
                // Flush as the overlay is non-contiguous, indicating potential poluuted state.
                warn!(target: "flashblocks", "Failed to get flashblocks state provider: {e}. Flushing cache");
                self.inner.write().flush();
                self.task_queue.flush();
                None
            }
        }?;
        Some((
            Box::new(MemoryOverlayStateProvider::new(canonical_state, in_memory)),
            block.clone_sealed_header(),
        ))
    }

    /// Instantiates a `MemoryOverlayStateProvider` with all block overlays in
    /// the flashblocks state cache, including the current pending executed
    /// block state.
    pub fn get_pending_state_provider(
        &self,
        canonical_state: StateProviderBox,
    ) -> Option<(StateProviderBox, SealedHeaderFor<N>)> {
        let guard = self.inner.read();
        let block = guard.get_pending_block()?.block;
        let block_num = block.number();
        let in_memory = guard.get_executed_blocks_up_to_height(block_num);
        drop(guard);

        let in_memory = match in_memory {
            Ok(blocks) => blocks,
            Err(e) => {
                // Flush as the overlay is non-contiguous, indicating potential poluuted state.
                warn!(target: "flashblocks", "Failed to get flashblocks state provider: {e}. Flushing cache");
                self.inner.write().flush();
                self.task_queue.flush();
                None
            }
        }?;
        Some((
            Box::new(MemoryOverlayStateProvider::new(canonical_state, in_memory)),
            block.clone_sealed_header(),
        ))
    }

    /// Returns all overlay blocks for the given hash, spanning from the
    /// persisted on-disk anchor up through the flashblocks state cache.
    ///
    /// Overlay blocks are collected newest-to-oldest from two layers:
    /// 1. **Flashblocks state cache** — pending + confirmed blocks
    /// 2. **Engine canonical in-memory state** — blocks committed to the
    ///    canonical chain but not yet persisted to disk
    ///
    /// Returns the overlay blocks, sealed header of the requested block,
    /// and the on-disk anchor hash. Returns `Ok(None)` if the block is
    /// not found in either layer (i.e. it is fully persisted on disk).
    #[expect(clippy::type_complexity)]
    pub fn get_overlay_data(
        &self,
        block_hash: &B256,
    ) -> eyre::Result<Option<(Vec<ExecutedBlock<N>>, SealedHeaderFor<N>, B256)>> {
        // 1. Retrieve flashblocks state cache overlay
        let guard = self.inner.read();
        let mut header =
            guard.get_block_by_hash(block_hash).map(|bar| bar.block.clone_sealed_header());
        let executed = header.as_ref().map(|h| guard.get_executed_blocks_up_to_height(h.number()));
        let canon_hash = guard.get_canon_info().1;
        drop(guard);

        let mut overlay = match executed {
            Some(Ok(Some(blocks))) => blocks,
            Some(Ok(None)) | None => Vec::new(),
            Some(Err(e)) => {
                warn!(target: "flashblocks", "Failed to get flashblocks state provider: {e}. Flushing cache");
                self.inner.write().flush();
                self.task_queue.flush();
                return Err(e);
            }
        };

        // 2. Retrieve engine canonical in-memory blocks
        let anchor_hash =
            if let Some(block_state) = self.canon_in_memory_state.state_by_hash(canon_hash) {
                let anchor = block_state.anchor();
                if header.is_none() {
                    header = block_state
                        .chain()
                        .find(|s| s.hash() == *block_hash)
                        .map(|s| s.block_ref().recovered_block().sealed_header().clone())
                }
                overlay.extend(block_state.chain().map(|s| s.block()));
                anchor.hash
            } else {
                canon_hash
            };

        if overlay.is_empty() || header.is_none() {
            // Block hash not found, already persisted to disk
            return Ok(None);
        }
        Ok(Some((overlay, header.expect("valid cached header"), anchor_hash)))
    }

    /// Returns the `ExecutedBlock` for the given block number from confirm cache.
    /// Used for diagnostic comparison with the engine's execution.
    pub fn debug_get_executed_block_by_number(
        &self,
        block_number: u64,
    ) -> Option<reth_chain_state::ExecutedBlock<N>> {
        self.inner.read().confirm_cache.get_executed_block_by_number(block_number)
    }

    /// Handles updating the latest pending state by the flashblocks rpc handle.
    ///
    /// This method detects when the flashblocks sequencer has advanced to the next
    /// pending sequence height, and optimistically commits the current pending
    /// sequence to the confirm cache before advancing the pending tip.
    ///
    /// If the pending sequence to be updated is the same as the current pending
    /// sequence, it will replace the existing with the incoming pending sequence.
    ///
    /// Note that this state update is fallible if something goes really wrong here
    /// as it detects potential reorgs and flashblocks state cache pollution. An entry
    /// is invalidated if the incoming pending sequence height is not the next pending
    /// height or current pending height.
    pub fn handle_pending_sequence(
        &self,
        pending_sequence: PendingSequence<N>,
    ) -> eyre::Result<()> {
        self.inner.write().handle_pending_sequence(pending_sequence)
    }

    /// Handles a canonical block committed to the canonical chainstate.
    ///
    /// Evicts confirmed blocks at or below the canonical height and clears any stale
    /// pending state. On reorg or hash mismatch, flushes the entire state cache and
    /// drains the execution task queue.
    pub fn handle_canonical_block(&self, canon_info: (u64, B256), reorg: bool) {
        debug!(
            target: "flashblocks",
            canonical_height = canon_info.0,
            "Flashblocks state cache received canonical block"
        );

        // Evict trie changesets for blocks below the eviction threshold.
        // Keep at least CHANGESET_CACHE_RETENTION_BLOCKS from the persisted tip, and also respect
        // the finalized block if set.
        let eviction_threshold = canon_info.0.saturating_sub(CHANGESET_CACHE_RETENTION_BLOCKS);
        debug!(
            target: "flashblocks",
            canonical_height = canon_info.0,
            eviction_threshold = eviction_threshold,
            "Evicting changesets below threshold"
        );
        self.changeset_cache.evict(eviction_threshold);
        if self.inner.write().handle_canonical_block(canon_info, reorg) {
            self.task_queue.flush();
        }
    }
}

/// Inner state of the flashblocks state cache.
#[derive(Debug)]
struct FlashblockStateCacheInner<N: NodePrimitives> {
    /// The current in-progress pending flashblock sequence, if any.
    pending_cache: Option<PendingSequence<N>>,
    /// Cache of confirmed flashblock sequences ahead of the canonical chain.
    confirm_cache: ConfirmCache<N>,
    /// Highest confirmed block height in the confirm cache. If flashblocks state cache
    /// is uninitialized, the confirm height is set to 0.
    confirm_height: u64,
    /// Highest confirmed block height in the canonical chainstate.
    canon_info: (u64, B256),
    /// Receiver of the most recent executed [`PendingSequence`] built from the latest
    /// flashblocks sequence.
    pending_sequence_rx: PendingSequenceRx<N>,
    /// Sender of the most recent executed [`PendingSequence`] built from the latest
    /// flashblocks sequence.
    pending_sequence_tx: watch::Sender<Option<PendingSequence<N>>>,
}

impl<N: NodePrimitives> FlashblockStateCacheInner<N> {
    fn new() -> Self {
        let (tx, rx) = watch::channel(None);

        Self {
            pending_cache: None,
            confirm_cache: ConfirmCache::new(),
            confirm_height: 0,
            canon_info: (0, B256::ZERO),
            pending_sequence_rx: rx,
            pending_sequence_tx: tx,
        }
    }

    fn flush(&mut self) {
        warn!(target: "flashblocks", "Flushing flashblocks state cache");
        self.pending_cache = None;
        self.confirm_cache.clear();
        self.confirm_height = self.canon_info.0;
    }

    /// Handles flushing a newly confirmed block to the confirm cache. Note that
    /// this state update is fallible as it detects potential reorgs, and triggers
    /// cache flush on invalidate entries.
    ///
    /// An entry is invalidated if:
    /// 1. Block height to be is lower than the cache's confirmed height
    /// 2. Block height to be is not the next confirm block height
    fn handle_confirmed_block(
        &mut self,
        block_number: u64,
        executed_block: ExecutedBlock<N>,
        receipts: Arc<Vec<ReceiptTy<N>>>,
    ) -> eyre::Result<()> {
        if block_number != self.confirm_height + 1 {
            return Err(eyre::eyre!(
                "polluted state cache - not next consecutive target confirm height block"
            ));
        }
        self.confirm_cache.insert(block_number, executed_block, receipts)?;
        self.confirm_height = block_number;
        info!(
            target: "flashblocks",
            confirm_height = self.confirm_height,
            canonical_height = self.canon_info.0,
            "Committed pending block to confirm flashblocks state cache",
        );
        Ok(())
    }

    fn handle_pending_sequence(
        &mut self,
        pending_sequence: PendingSequence<N>,
    ) -> eyre::Result<()> {
        let pending_height = pending_sequence.get_height();
        let expected_height = self.confirm_height + 1;

        if pending_height == expected_height {
            let broadcast = pending_sequence.clone();
            if pending_sequence.is_sequence_end() {
                // Sequence end. Promote to confirm, and clear pending state.
                self.handle_confirmed_block(
                    expected_height,
                    pending_sequence.pending.executed_block,
                    pending_sequence.pending.receipts,
                )?;
                self.pending_cache = None;
            } else {
                // In-progress — replace pending with newer flashblock
                self.pending_cache = Some(pending_sequence);
            }
            let _ = self.pending_sequence_tx.send(Some(broadcast));
        } else if pending_height <= self.confirm_height {
            // State cache may have advanced from canonical block stream. Skip
        } else {
            return Err(eyre::eyre!(
                "polluted state cache - not next consecutive pending height block"
            ));
        }
        Ok(())
    }

    fn handle_canonical_block(&mut self, canon_info: (u64, B256), reorg: bool) -> bool {
        let hash_mismatch = self.confirm_cache.number_for_hash(&canon_info.1).is_none()
            && self.confirm_cache.get_block_by_number(canon_info.0).is_some();
        if reorg || hash_mismatch {
            warn!(
                target: "flashblocks",
                canonical_height = canon_info.0,
                confirm_height = self.confirm_height,
                canonical_reorg = reorg,
                hash_mismatch,
                "Reorg or hash mismatch detected, flushing entire flashblocks state cache",
            );
            self.canon_info = canon_info;
            self.flush();
            return true;
        }

        // Clear pending if at or below canonical height — the canonical chainstate
        // already serves this block. The validator's in-flight commit (if any) will
        // get a benign "polluted" error and move on.
        if self.pending_cache.as_ref().is_some_and(|p| p.get_height() <= canon_info.0) {
            debug!(
                target: "flashblocks",
                canonical_height = canon_info.0,
                confirm_height = self.confirm_height,
                "Clearing stale pending on canonical block",
            );
            self.pending_cache = None;
        }

        // Evict confirmed blocks at or below canonical height. The executed block state
        // will now be served by the in-memory canonical chainstate.
        self.confirm_cache.flush_up_to_height(canon_info.0);

        // Update state heights
        self.canon_info = canon_info;
        self.confirm_height = self.confirm_height.max(canon_info.0);
        false
    }

    pub fn get_confirmed_block(&self) -> Option<BlockAndReceipts<N>> {
        self.get_block_by_number(self.confirm_height)
    }

    pub fn get_pending_block(&self) -> Option<BlockAndReceipts<N>> {
        self.pending_cache.as_ref().map(|p| p.get_block_and_receipts())
    }

    pub fn get_canon_info(&self) -> (u64, B256) {
        self.canon_info
    }

    pub fn get_block_by_number(&self, num: u64) -> Option<BlockAndReceipts<N>> {
        if let Some(pending_sequence) = self.pending_cache.as_ref()
            && pending_sequence.get_height() == num
        {
            return Some(pending_sequence.get_block_and_receipts());
        }
        self.confirm_cache.get_block_by_number(num)
    }

    pub fn get_block_by_hash(&self, hash: &B256) -> Option<BlockAndReceipts<N>> {
        if let Some(pending_sequence) = self.pending_cache.as_ref()
            && pending_sequence.get_hash() == *hash
        {
            return Some(pending_sequence.get_block_and_receipts());
        }
        self.confirm_cache.get_block_by_hash(hash)
    }

    fn get_tx_info(&self, tx_hash: &TxHash) -> Option<(CachedTxInfo<N>, BlockAndReceipts<N>)> {
        self.pending_cache
            .as_ref()
            .and_then(|p| p.get_tx_info(tx_hash))
            .or_else(|| self.confirm_cache.get_tx_info(tx_hash))
    }

    /// Returns the ordered vector of `ExecutedBlock`s from the cache.
    ///
    /// # Safety of the overlay
    /// The returned blocks used for state overlay is correct **if and only if** the
    /// blocks form a contiguous chain from some height down to `canonical_height + 1`
    /// (or `canonical_height` itself in the redundant-but-safe race case).
    ///
    /// **Safe (redundant overlap)**: Due to a race between canonical commit and confirm
    /// cache flush, the lowest overlay block may be equal to or lower than the canonical
    /// height.
    ///
    /// For example, canonical is at height `x` and the overlay contains `[x+2, x+1, x]`.
    /// This is safe the overlay blocks are checked first (newest-to-oldest). The state
    /// at height `x` contains changes identical to what canonical already applied, so
    /// the result is correct regardless of which source resolves the query.
    ///
    /// **State inconsistency (gap in overlay)**: If an intermediate block is missing,
    /// for example overlay has `[x+2, x]` but not `x+1`, then any account modified only
    /// at height `x+1` would be invisible — the query falls through to canonical which
    /// returns stale incorrect state.
    ///
    /// **State inconsistency (canonical too far behind)**: If the canonical height is
    /// more than one block below the lowest overlay block. For example, canonical at
    /// `x-2`, lowest overlay at `x`, then changes at height `x-1` are not covered by
    /// either source.
    ///
    /// Both failure modes reduce to: every height between `canonical_height + 1` and
    /// the target must be present in the overlay. This invariant is naturally maintained
    /// by `handle_confirmed_block` (rejects non-consecutive heights). The pending block,
    /// if present, sits at `confirm_height + 1`; it may be absent after a complete
    /// sequence is promoted directly to the confirm cache via `sequence_end` signal.
    ///
    /// On validation failure (non-contiguous overlay or gap to canonical), the cache is
    /// flushed and `None` is returned.
    fn get_executed_blocks_up_to_height(
        &self,
        target_height: u64,
    ) -> eyre::Result<Option<Vec<ExecutedBlock<N>>>> {
        if self.confirm_height == 0
            || self.canon_info.0 == 0
            || target_height > self.confirm_height + 1
            || target_height <= self.canon_info.0
        {
            // Cache not initialized or target height is outside the cache range
            return Ok(None);
        }
        let mut blocks = Vec::new();
        if let Some(p) = self.pending_cache.as_ref()
            && p.get_height() == target_height
        {
            blocks.push(p.pending.executed_block.clone());
        }
        blocks.extend(
            self.confirm_cache
                .get_executed_blocks_up_to_height(target_height, self.canon_info.0)?,
        );
        Ok(Some(blocks))
    }

    pub fn subscribe_pending_sequence(&self) -> PendingSequenceRx<N> {
        self.pending_sequence_rx.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{
        make_executed_block, make_pending_sequence, make_pending_sequence_end,
        make_pending_sequence_with_txs,
    };
    use alloy_consensus::BlockHeader;
    use reth_chain_state::CanonicalInMemoryState;
    use reth_optimism_primitives::OpPrimitives;

    type TestCache = FlashblockStateCache<OpPrimitives>;
    type TestInner = FlashblockStateCacheInner<OpPrimitives>;

    fn make_test_cache() -> TestCache {
        TestCache::new(CanonicalInMemoryState::new(
            Default::default(),
            Default::default(),
            None,
            None,
            None,
        ))
    }

    /// Helper: promote heights 1..=n to confirm cache via `sequence_end: true`,
    /// then insert height n+1 as pending with `sequence_end: false`.
    fn populate_inner_with_confirmed_and_pending(inner: &mut TestInner, confirmed_count: u64) {
        for i in 1..=confirmed_count {
            let seq = make_pending_sequence_end(i, B256::repeat_byte(i as u8));
            inner.handle_pending_sequence(seq).unwrap();
        }
        let pending_height = confirmed_count + 1;
        let seq = make_pending_sequence(pending_height, B256::repeat_byte(pending_height as u8));
        inner.handle_pending_sequence(seq).unwrap();
    }

    // ========================================================================
    // Defaults
    // ========================================================================

    #[test]
    fn test_new_defaults() {
        let inner = TestInner::new();
        assert_eq!(inner.confirm_height, 0);
        assert_eq!(inner.canon_info, (0, B256::ZERO));
        assert!(inner.pending_cache.is_none());
    }

    // ========================================================================
    // handle_pending_sequence — insertion, replacement, promotion
    // ========================================================================

    #[test]
    fn test_handle_pending_first_at_expected_height() {
        let mut inner = TestInner::new();
        let seq = make_pending_sequence(1, B256::ZERO);
        inner.handle_pending_sequence(seq).unwrap();

        assert!(inner.pending_cache.is_some());
        assert_eq!(inner.pending_cache.as_ref().unwrap().get_height(), 1);
        assert_eq!(inner.confirm_height, 0);
    }

    #[test]
    fn test_handle_pending_replace_same_height() {
        let mut inner = TestInner::new();
        let seq1 = make_pending_sequence(1, B256::ZERO);
        inner.handle_pending_sequence(seq1).unwrap();

        let seq2 = make_pending_sequence(1, B256::repeat_byte(0xAA));
        let seq2_hash = seq2.block_hash;
        inner.handle_pending_sequence(seq2).unwrap();

        assert!(inner.pending_cache.is_some());
        assert_eq!(inner.pending_cache.as_ref().unwrap().block_hash, seq2_hash);
        assert_eq!(inner.confirm_height, 0);
    }

    #[test]
    fn test_sequence_end_promotes_to_confirm() {
        let mut inner = TestInner::new();
        // Insert with sequence_end: true → promotes to confirm, clears pending
        let seq = make_pending_sequence_end(1, B256::ZERO);
        inner.handle_pending_sequence(seq).unwrap();

        assert_eq!(inner.confirm_height, 1);
        assert!(inner.pending_cache.is_none());
        assert!(inner.confirm_cache.get_block_by_number(1).is_some());

        // Now height 2 is accepted as next pending
        let seq2 = make_pending_sequence(2, B256::repeat_byte(0xBB));
        inner.handle_pending_sequence(seq2).unwrap();
        assert_eq!(inner.pending_cache.as_ref().unwrap().get_height(), 2);
    }

    #[test]
    fn test_sequence_end_false_does_not_promote() {
        let mut inner = TestInner::new();
        // Multiple inserts at same height with sequence_end: false
        inner.handle_pending_sequence(make_pending_sequence(1, B256::ZERO)).unwrap();
        inner.handle_pending_sequence(make_pending_sequence(1, B256::repeat_byte(0x01))).unwrap();

        assert_eq!(inner.confirm_height, 0);
        assert!(inner.pending_cache.is_some());
        assert!(inner.confirm_cache.is_empty());
    }

    #[test]
    fn test_handle_pending_gap_height_errors() {
        let mut inner = TestInner::new();
        // Height 2 when confirm_height=0 → expected_height=1, gap error
        let seq = make_pending_sequence(2, B256::ZERO);
        let result = inner.handle_pending_sequence(seq);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not next consecutive pending height block"));
    }

    #[test]
    fn test_handle_pending_wrong_height_errors() {
        let mut inner = TestInner::new();
        let seq = make_pending_sequence(5, B256::ZERO);
        let result = inner.handle_pending_sequence(seq);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not next consecutive pending height block"));
    }

    #[test]
    fn test_handle_pending_skip_already_confirmed_height() {
        let mut inner = TestInner::new();
        // Promote height 1 to confirm
        inner.handle_pending_sequence(make_pending_sequence_end(1, B256::ZERO)).unwrap();
        assert_eq!(inner.confirm_height, 1);

        // Re-sending height 1 is silently skipped (height <= confirm_height)
        let result = inner.handle_pending_sequence(make_pending_sequence(1, B256::ZERO));
        assert!(result.is_ok());
    }

    #[test]
    fn test_handle_confirmed_block_non_consecutive_errors() {
        let mut inner = TestInner::new();
        let executed = make_executed_block(5, B256::ZERO);
        let result = inner.handle_confirmed_block(5, executed, Arc::new(vec![]));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not next consecutive target confirm height block"));
    }

    // ========================================================================
    // handle_canonical_block — eviction, stale clearing, reorg flush
    // ========================================================================

    #[test]
    fn test_handle_canonical_evicts_confirm() {
        let mut inner = TestInner::new();
        // Promote heights 1-4 to confirm, height 5 as pending
        populate_inner_with_confirmed_and_pending(&mut inner, 4);
        assert_eq!(inner.confirm_height, 4);
        assert_eq!(inner.pending_cache.as_ref().unwrap().get_height(), 5);

        // Use the actual hash of the confirmed block at height 2 to avoid hash_mismatch
        let block2_hash = inner.confirm_cache.get_block_by_number(2).unwrap().block.hash();
        let flushed = inner.handle_canonical_block((2, block2_hash), false);
        assert!(!flushed);
        assert_eq!(inner.canon_info.0, 2);
        assert_eq!(inner.confirm_height, 4); // confirm_height unchanged
        assert!(inner.confirm_cache.get_block_by_number(1).is_none());
        assert!(inner.confirm_cache.get_block_by_number(2).is_none());
        assert!(inner.confirm_cache.get_block_by_number(3).is_some());
        assert!(inner.confirm_cache.get_block_by_number(4).is_some());
    }

    #[test]
    fn test_handle_canonical_clears_stale_pending() {
        let mut inner = TestInner::new();
        let seq = make_pending_sequence(1, B256::ZERO);
        inner.handle_pending_sequence(seq).unwrap();

        // Canonical at height 1 — pending is stale, cleared (not a full flush)
        let flushed = inner.handle_canonical_block((1, B256::repeat_byte(0xCC)), false);
        assert!(!flushed);
        assert!(inner.pending_cache.is_none());
        assert_eq!(inner.confirm_height, 1); // max(0, 1)
    }

    #[test]
    fn test_handle_canonical_flush_on_reorg() {
        let mut inner = TestInner::new();
        let seq = make_pending_sequence(1, B256::ZERO);
        inner.handle_pending_sequence(seq).unwrap();

        let flushed = inner.handle_canonical_block((0, B256::repeat_byte(0xDD)), true);
        assert!(flushed);
        assert!(inner.pending_cache.is_none());
    }

    // ========================================================================
    // get_block_by_number — pending priority, confirm fallback
    // ========================================================================

    #[test]
    fn test_get_block_by_number_pending_priority() {
        let cache = make_test_cache();
        // Promote height 1 to confirm, height 2 pending
        cache.handle_pending_sequence(make_pending_sequence_end(1, B256::ZERO)).unwrap();
        cache.handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01))).unwrap();

        // Replace pending at height 2 with new block
        let new_pending = make_pending_sequence(2, B256::repeat_byte(0x02));
        let new_pending_hash = new_pending.block_hash;
        cache.handle_pending_sequence(new_pending).unwrap();

        let result = cache.get_block_by_number(2).unwrap();
        assert_eq!(result.block.hash(), new_pending_hash);
    }

    #[test]
    fn test_get_block_by_number_falls_to_confirm() {
        let cache = make_test_cache();
        let seq1 = make_pending_sequence_end(1, B256::ZERO);
        let seq1_hash = seq1.block_hash;
        cache.handle_pending_sequence(seq1).unwrap();
        cache.handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01))).unwrap();

        // Height 1 is in confirm, not pending
        let result = cache.get_block_by_number(1).unwrap();
        assert_eq!(result.block.hash(), seq1_hash);
    }

    // ========================================================================
    // get_block_by_hash — pending priority, confirm fallback
    // ========================================================================

    #[test]
    fn test_get_block_by_hash_returns_pending() {
        let cache = make_test_cache();
        let seq = make_pending_sequence(1, B256::ZERO);
        let pending_hash = seq.block_hash;
        cache.handle_pending_sequence(seq).unwrap();

        let result = cache.get_block_by_hash(&pending_hash).unwrap();
        assert_eq!(result.block.number(), 1);
    }

    #[test]
    fn test_get_block_by_hash_returns_from_confirm_cache() {
        let cache = make_test_cache();
        let seq = make_pending_sequence_end(1, B256::ZERO);
        let confirmed_hash = seq.block_hash;
        cache.handle_pending_sequence(seq).unwrap();
        // Height 1 promoted to confirm, pending is empty
        cache.handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01))).unwrap();

        let result = cache.get_block_by_hash(&confirmed_hash).unwrap();
        assert_eq!(result.block.number(), 1);
    }

    // ========================================================================
    // get_rpc_block — Pending, Latest, Number(n), empty cache
    // ========================================================================

    #[test]
    fn test_get_rpc_block_pending_returns_pending() {
        let cache = make_test_cache();
        let seq = make_pending_sequence(1, B256::ZERO);
        let pending_hash = seq.block_hash;
        cache.handle_pending_sequence(seq).unwrap();

        let result = cache.get_rpc_block(BlockNumberOrTag::Pending).unwrap();
        assert_eq!(result.block.hash(), pending_hash);
    }

    #[test]
    fn test_get_rpc_block_latest_returns_confirmed() {
        let cache = make_test_cache();
        let seq1 = make_pending_sequence_end(1, B256::ZERO);
        let seq1_hash = seq1.block_hash;
        cache.handle_pending_sequence(seq1).unwrap();
        cache.handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01))).unwrap();

        let result = cache.get_rpc_block(BlockNumberOrTag::Latest).unwrap();
        assert_eq!(result.block.hash(), seq1_hash);
        assert_eq!(result.block.number(), 1);
    }

    #[test]
    fn test_get_rpc_block_number_tag_returns_block() {
        let cache = make_test_cache();
        let seq = make_pending_sequence_end(1, B256::ZERO);
        let hash = seq.block_hash;
        cache.handle_pending_sequence(seq).unwrap();

        let result = cache.get_rpc_block(BlockNumberOrTag::Number(1)).unwrap();
        assert_eq!(result.block.hash(), hash);
    }

    #[test]
    fn test_get_rpc_block_returns_none_on_empty_cache() {
        let cache = make_test_cache();
        assert!(cache.get_rpc_block(BlockNumberOrTag::Pending).is_none());
        assert!(cache.get_rpc_block(BlockNumberOrTag::Latest).is_none());
    }

    // ========================================================================
    // get_tx_info — pending, confirm, missing (split into 3 tests)
    // ========================================================================

    #[test]
    fn test_get_tx_info_returns_from_pending() {
        let cache = make_test_cache();
        let seq = make_pending_sequence_with_txs(1, B256::ZERO, 0, 2);
        let tx_hash = *seq.tx_index.keys().next().unwrap();
        cache.handle_pending_sequence(seq).unwrap();

        let (info, _) = cache.get_tx_info(&tx_hash).unwrap();
        assert_eq!(info.block_number, 1);
    }

    #[test]
    fn test_get_tx_info_falls_back_to_confirm() {
        let cache = make_test_cache();
        // Promote height 1 with txs to confirm
        let mut seq1 = make_pending_sequence_with_txs(1, B256::ZERO, 0, 2);
        let confirm_tx_hash = *seq1.tx_index.keys().next().unwrap();
        seq1.sequence_end = true;
        cache.handle_pending_sequence(seq1).unwrap();

        // Height 2 as pending (no overlap with height 1's txs)
        cache.handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01))).unwrap();

        let (info, _) = cache.get_tx_info(&confirm_tx_hash).unwrap();
        assert_eq!(info.block_number, 1);
    }

    #[test]
    fn test_get_tx_info_returns_none_for_unknown_hash() {
        let cache = make_test_cache();
        cache.handle_pending_sequence(make_pending_sequence(1, B256::ZERO)).unwrap();
        assert!(cache.get_tx_info(&B256::repeat_byte(0xFF)).is_none());
    }

    // ========================================================================
    // get_executed_blocks_up_to_height
    // ========================================================================

    #[test]
    fn test_get_executed_blocks_returns_none_when_uninitialized() {
        let inner = TestInner::new();
        let result = inner.get_executed_blocks_up_to_height(1).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_executed_blocks_contiguous() {
        let mut inner = TestInner::new();
        inner.canon_info = (1, B256::repeat_byte(0x01));
        inner.confirm_height = 1;

        // Heights 2-3 promoted to confirm
        for i in 2..=3 {
            let seq = make_pending_sequence_end(i, B256::repeat_byte(i as u8));
            inner.handle_pending_sequence(seq).unwrap();
        }
        // Height 4 as pending
        let seq = make_pending_sequence(4, B256::repeat_byte(0x04));
        inner.handle_pending_sequence(seq).unwrap();

        let blocks = inner.get_executed_blocks_up_to_height(4).unwrap().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].recovered_block.number(), 4);
    }

    #[test]
    fn test_get_executed_blocks_returns_none_for_target_below_canon() {
        let mut inner = TestInner::new();
        inner.canon_info = (5, B256::repeat_byte(0x01));
        inner.confirm_height = 5;

        let result = inner.get_executed_blocks_up_to_height(5).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_executed_blocks_returns_none_above_confirm_plus_one() {
        let mut inner = TestInner::new();
        inner.canon_info = (1, B256::repeat_byte(0x01));
        inner.confirm_height = 2;

        // Target far above confirm_height + 1
        let result = inner.get_executed_blocks_up_to_height(5).unwrap();
        assert!(result.is_none());
    }

    // ========================================================================
    // subscribe_pending_sequence
    // ========================================================================

    #[test]
    fn test_subscribe_receives_update_on_pending_insert() {
        let cache = make_test_cache();
        let mut rx = cache.subscribe_pending_sequence();

        let seq = make_pending_sequence(1, B256::ZERO);
        cache.handle_pending_sequence(seq).unwrap();

        assert!(rx.has_changed().unwrap());
        let val = rx.borrow_and_update();
        assert!(val.is_some());
        assert_eq!(val.as_ref().unwrap().get_height(), 1);
    }

    #[test]
    fn test_subscribe_sees_replacement() {
        let cache = make_test_cache();
        let mut rx = cache.subscribe_pending_sequence();

        cache.handle_pending_sequence(make_pending_sequence(1, B256::ZERO)).unwrap();
        rx.borrow_and_update(); // consume first update

        let replacement = make_pending_sequence(1, B256::repeat_byte(0xAA));
        let replacement_hash = replacement.block_hash;
        cache.handle_pending_sequence(replacement).unwrap();

        assert!(rx.has_changed().unwrap());
        let val = rx.borrow_and_update();
        assert_eq!(val.as_ref().unwrap().block_hash, replacement_hash);
    }

    #[test]
    fn test_subscribe_receives_on_advance() {
        let cache = make_test_cache();
        let mut rx = cache.subscribe_pending_sequence();

        // Promote height 1 to confirm
        cache.handle_pending_sequence(make_pending_sequence_end(1, B256::ZERO)).unwrap();
        rx.borrow_and_update(); // consume

        // Insert height 2 as pending
        let seq2 = make_pending_sequence(2, B256::repeat_byte(0x01));
        let seq2_hash = seq2.block_hash;
        cache.handle_pending_sequence(seq2).unwrap();

        assert!(rx.has_changed().unwrap());
        let val = rx.borrow_and_update();
        assert_eq!(val.as_ref().unwrap().get_height(), 2);
        assert_eq!(val.as_ref().unwrap().block_hash, seq2_hash);
    }

    // ========================================================================
    // Outer FlashblockStateCache methods
    // ========================================================================

    #[test]
    fn test_outer_handle_canonical_block_updates_canon_info() {
        let cache = make_test_cache();
        // Promote height 1, then height 2 as pending
        cache.handle_pending_sequence(make_pending_sequence_end(1, B256::ZERO)).unwrap();
        cache.handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01))).unwrap();

        let canon_hash = B256::repeat_byte(0xCC);
        cache.handle_canonical_block((1, canon_hash), false);

        assert_eq!(cache.get_canon_height(), 1);
    }

    #[test]
    fn test_flush_resets_confirm_height_to_canon() {
        let mut inner = TestInner::new();
        inner.handle_pending_sequence(make_pending_sequence(1, B256::ZERO)).unwrap();

        // Canonical at height 1 clears stale pending, confirm_height = max(0, 1) = 1
        inner.handle_canonical_block((1, B256::repeat_byte(0xAA)), false);

        assert_eq!(inner.confirm_height, 1);
        assert!(inner.pending_cache.is_none());
    }
}
