mod confirm;
pub mod pending;
pub(crate) mod raw;
pub(crate) mod utils;

pub(crate) use confirm::ConfirmCache;
pub(crate) use raw::RawFlashblocksCache;

pub use pending::PendingSequence;

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
    canon_in_memory_state: CanonicalInMemoryState<N>,
}

// FlashblockStateCache read interfaces
impl<N: NodePrimitives> FlashblockStateCache<N> {
    /// Creates a new [`FlashblockStateCache`].
    pub fn new(canon_in_memory_state: CanonicalInMemoryState<N>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(FlashblockStateCacheInner::new())),
            changeset_cache: ChangesetCache::new(),
            canon_in_memory_state,
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
        let mut overlay = if let Some(ref h) = header {
            let block_num = h.number();
            guard.get_executed_blocks_up_to_height(block_num)?.unwrap_or_default()
        } else {
            Vec::new()
        };
        let canon_hash = guard.get_canon_info().1;
        drop(guard);

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
}

// FlashblockStateCache state mutation interfaces.
impl<N: NodePrimitives> FlashblockStateCache<N> {
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
        target_index: u64,
    ) -> eyre::Result<()> {
        self.inner.write().handle_pending_sequence(pending_sequence, target_index)
    }

    /// Handles a canonical block committed to the canonical chainstate.
    ///
    /// This method will flush the confirm cache up to the canonical block height and
    /// the pending state if it matches the committed block to ensure flashblocks state
    /// cache memory does not grow unbounded.
    ///
    /// It also detects chainstate re-orgs (set with re-org arg flag) and flashblocks
    /// state cache pollution. By default once error is detected, we will automatically
    /// flush the flashblocks state cache.
    pub fn handle_canonical_block(&self, canon_info: (u64, B256), reorg: bool) -> bool {
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
        self.inner.write().handle_canonical_block(canon_info, reorg)
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
        self.confirm_height = block_number;
        self.confirm_cache.insert(block_number, executed_block, receipts)?;
        Ok(())
    }

    fn handle_pending_sequence(
        &mut self,
        pending_sequence: PendingSequence<N>,
        target_index: u64,
    ) -> eyre::Result<()> {
        let pending_height = pending_sequence.get_height();
        let expected_height = self.confirm_height + 1;

        if pending_height == expected_height {
            let incoming_seq = pending_sequence.clone();
            if target_index > 0 && pending_sequence.get_last_flashblock_index() >= target_index {
                // Target flashblock. Promote to confirm, and clear pending state
                self.handle_confirmed_block(
                    expected_height,
                    incoming_seq.pending.executed_block,
                    incoming_seq.pending.receipts,
                )?;
                self.pending_cache = None;
            } else {
                // In-progress — replace pending with newer flashblock
                self.pending_cache = Some(incoming_seq);
            }
        } else if pending_height == expected_height + 1 {
            // The next block's flashblock arrived. Somehow target flashblocks was
            // missed. Promote current pending to confirm, and set incoming as new
            // pending sequence.
            let sequence = self.pending_cache.take().ok_or_else(|| {
                eyre::eyre!(
                    "polluted state cache - trying to advance pending tip but no current pending"
                )
            })?;
            self.handle_confirmed_block(
                expected_height,
                sequence.pending.executed_block,
                sequence.pending.receipts,
            )?;
            self.pending_cache = Some(pending_sequence.clone());
        } else {
            return Err(eyre::eyre!(
                "polluted state cache - not next consecutive pending height block"
            ));
        }
        let _ = self.pending_sequence_tx.send(Some(pending_sequence));
        Ok(())
    }

    fn handle_canonical_block(&mut self, canon_info: (u64, B256), reorg: bool) -> bool {
        let pending_stale =
            self.pending_cache.as_ref().is_some_and(|p| p.get_height() <= canon_info.0);
        let flush = pending_stale || reorg;
        if flush {
            warn!(
                target: "flashblocks",
                canonical_height = canon_info.0,
                cache_height = self.confirm_height,
                canonical_reorg = reorg,
                pending_stale,
                "Reorg or pending stale detected on handle canonical block",
            );
            self.flush();
        } else {
            debug!(
                target: "flashblocks",
                canonical_height = canon_info.0,
                cache_height = self.confirm_height,
                "Evicting flashblocks state inner cache"
            );

            self.confirm_cache.flush_up_to_height(canon_info.0);
        }
        // Update state heights
        self.canon_info = canon_info;
        self.confirm_height = self.confirm_height.max(canon_info.0);
        flush
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
    /// sequence is promoted directly to the confirm cache via `target_index`.
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
        make_executed_block, make_pending_sequence, make_pending_sequence_with_txs,
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

    #[test]
    fn test_new_defaults() {
        let inner = TestInner::new();
        assert_eq!(inner.confirm_height, 0);
        assert_eq!(inner.canon_info, (0, B256::ZERO));
        assert!(inner.pending_cache.is_none());
    }

    #[test]
    fn test_handle_pending_first_at_expected_height() {
        let mut inner = TestInner::new();
        // confirm_height=0, expected_height=1
        let seq = make_pending_sequence(1, B256::ZERO);
        inner.handle_pending_sequence(seq, 0).unwrap();

        assert!(inner.pending_cache.is_some());
        assert_eq!(inner.pending_cache.as_ref().unwrap().get_height(), 1);
        assert_eq!(inner.confirm_height, 0);
    }

    #[test]
    fn test_handle_pending_replace_same_height() {
        let mut inner = TestInner::new();
        let seq1 = make_pending_sequence(1, B256::ZERO);
        let seq1_hash = seq1.block_hash;
        inner.handle_pending_sequence(seq1, 0).unwrap();

        // Replace at same height with different parent_hash to produce a different block
        let seq2 = make_pending_sequence(1, B256::repeat_byte(0xAA));
        let seq2_hash = seq2.block_hash;
        inner.handle_pending_sequence(seq2, 0).unwrap();

        assert!(inner.pending_cache.is_some());
        // Block hash should have changed due to different parent hash
        assert_ne!(seq1_hash, seq2_hash);
        assert_eq!(inner.pending_cache.as_ref().unwrap().block_hash, seq2_hash);
        assert_eq!(inner.confirm_height, 0);
    }

    #[test]
    fn test_handle_pending_advance_commits_to_confirm() {
        let mut inner = TestInner::new();
        let seq1 = make_pending_sequence(1, B256::ZERO);
        inner.handle_pending_sequence(seq1, 0).unwrap();

        // Advance to height 2 — seq at height 1 should be committed to confirm
        let seq2 = make_pending_sequence(2, B256::repeat_byte(0xBB));
        inner.handle_pending_sequence(seq2, 0).unwrap();

        assert_eq!(inner.confirm_height, 1);
        assert_eq!(inner.pending_cache.as_ref().unwrap().get_height(), 2);
        assert!(inner.confirm_cache.get_block_by_number(1).is_some());
    }

    #[test]
    fn test_handle_pending_advance_without_existing_errors() {
        let mut inner = TestInner::new();
        let seq = make_pending_sequence(2, B256::ZERO);
        let result = inner.handle_pending_sequence(seq, 0);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("trying to advance pending tip but no current pending"));
    }

    #[test]
    fn test_handle_pending_wrong_height_errors() {
        let mut inner = TestInner::new();
        let seq = make_pending_sequence(5, B256::ZERO);
        let result = inner.handle_pending_sequence(seq, 0);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not next consecutive pending height block"));
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

    #[test]
    fn test_handle_canonical_evicts_confirm() {
        let mut inner = TestInner::new();
        for i in 1..=5 {
            let seq = make_pending_sequence(i, B256::repeat_byte(i as u8));
            inner.handle_pending_sequence(seq, 0).unwrap();
        }
        assert_eq!(inner.confirm_height, 4);
        assert_eq!(inner.pending_cache.as_ref().unwrap().get_height(), 5);

        let flushed = inner.handle_canonical_block((2, B256::repeat_byte(0xFF)), false);
        assert!(!flushed); // No full flush (pending at 5 > canon 2)
        assert_eq!(inner.canon_info.0, 2);
        assert!(inner.confirm_cache.get_block_by_number(1).is_none());
        assert!(inner.confirm_cache.get_block_by_number(2).is_none());
        assert!(inner.confirm_cache.get_block_by_number(3).is_some());
        assert!(inner.confirm_cache.get_block_by_number(4).is_some());
    }

    #[test]
    fn test_handle_canonical_flush_on_pending_stale() {
        let mut inner = TestInner::new();
        // Insert pending at height 1
        let seq = make_pending_sequence(1, B256::ZERO);
        inner.handle_pending_sequence(seq, 0).unwrap();

        // Canonical catches up to height 1 — pending is stale
        let flushed = inner.handle_canonical_block((1, B256::repeat_byte(0xCC)), false);
        assert!(flushed);
        assert!(inner.pending_cache.is_none());
        assert_eq!(inner.confirm_height, 1); // max(0, 1)
    }

    #[test]
    fn test_handle_canonical_flush_on_reorg() {
        let mut inner = TestInner::new();
        let seq = make_pending_sequence(1, B256::ZERO);
        inner.handle_pending_sequence(seq, 0).unwrap();

        // Even if pending is ahead, reorg flag forces full flush
        let flushed = inner.handle_canonical_block((0, B256::repeat_byte(0xDD)), true);
        assert!(flushed);
        assert!(inner.pending_cache.is_none());
    }

    #[test]
    fn test_get_block_by_number_pending_priority() {
        let cache = make_test_cache();
        cache.handle_pending_sequence(make_pending_sequence(1, B256::ZERO), 0).unwrap();
        cache
            .handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01)), 0)
            .unwrap();

        let new_pending = make_pending_sequence(2, B256::repeat_byte(0x02));
        let new_pending_hash = new_pending.block_hash;
        cache.handle_pending_sequence(new_pending, 0).unwrap();

        let result = cache.get_block_by_number(2).unwrap();
        assert_eq!(result.block.hash(), new_pending_hash);
    }

    #[test]
    fn test_get_block_by_number_falls_to_confirm() {
        let cache = make_test_cache();
        let seq1 = make_pending_sequence(1, B256::ZERO);
        let seq1_hash = seq1.block_hash;
        cache.handle_pending_sequence(seq1, 0).unwrap();
        cache
            .handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01)), 0)
            .unwrap();

        let result = cache.get_block_by_number(1).unwrap();
        assert_eq!(result.block.hash(), seq1_hash);
    }

    #[test]
    fn test_get_block_by_hash_pending_priority() {
        let cache = make_test_cache();
        let seq = make_pending_sequence(1, B256::ZERO);
        let pending_hash = seq.block_hash;
        cache.handle_pending_sequence(seq, 0).unwrap();

        let result = cache.get_block_by_hash(&pending_hash).unwrap();
        assert_eq!(result.block.number(), 1);
    }

    #[test]
    fn test_get_rpc_block_latest_returns_confirmed() {
        let cache = make_test_cache();
        let seq1 = make_pending_sequence(1, B256::ZERO);
        let seq1_hash = seq1.block_hash;
        cache.handle_pending_sequence(seq1, 0).unwrap();
        cache
            .handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01)), 0)
            .unwrap();

        let result = cache.get_rpc_block(BlockNumberOrTag::Latest).unwrap();
        assert_eq!(result.block.hash(), seq1_hash);
        assert_eq!(result.block.number(), 1);
    }

    #[test]
    fn test_get_rpc_block_pending_returns_pending() {
        let cache = make_test_cache();
        let seq = make_pending_sequence(1, B256::ZERO);
        let pending_hash = seq.block_hash;
        cache.handle_pending_sequence(seq, 0).unwrap();

        let result = cache.get_rpc_block(BlockNumberOrTag::Pending).unwrap();
        assert_eq!(result.block.hash(), pending_hash);
    }

    #[test]
    fn test_get_tx_info_checks_pending_then_confirm() {
        let cache = make_test_cache();
        let seq1 = make_pending_sequence_with_txs(1, B256::ZERO, 0, 2);
        let confirm_tx_hash = *seq1.tx_index.keys().next().unwrap();
        cache.handle_pending_sequence(seq1, 0).unwrap();

        let seq2 = make_pending_sequence_with_txs(2, B256::repeat_byte(0x01), 100, 1);
        let pending_tx_hash = *seq2.tx_index.keys().next().unwrap();
        cache.handle_pending_sequence(seq2, 0).unwrap();

        let (info, _) = cache.get_tx_info(&pending_tx_hash).unwrap();
        assert_eq!(info.block_number, 2);

        let (info, _) = cache.get_tx_info(&confirm_tx_hash).unwrap();
        assert_eq!(info.block_number, 1);

        assert!(cache.get_tx_info(&B256::repeat_byte(0xFF)).is_none());
    }

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

        for i in 2..=4 {
            let seq = make_pending_sequence(i, B256::repeat_byte(i as u8));
            inner.handle_pending_sequence(seq, 0).unwrap();
        }

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
    fn test_subscribe_receives_update_on_pending_insert() {
        let cache = make_test_cache();
        let mut rx = cache.subscribe_pending_sequence();

        let seq = make_pending_sequence(1, B256::ZERO);
        cache.handle_pending_sequence(seq, 0).unwrap();

        assert!(rx.has_changed().unwrap());
        let val = rx.borrow_and_update();
        assert!(val.is_some());
        assert_eq!(val.as_ref().unwrap().get_height(), 1);
    }

    #[test]
    fn test_subscribe_sees_replacement() {
        let cache = make_test_cache();
        let mut rx = cache.subscribe_pending_sequence();

        cache.handle_pending_sequence(make_pending_sequence(1, B256::ZERO), 0).unwrap();
        rx.borrow_and_update(); // consume first update

        let replacement = make_pending_sequence(1, B256::repeat_byte(0xAA));
        let replacement_hash = replacement.block_hash;
        cache.handle_pending_sequence(replacement, 0).unwrap();

        assert!(rx.has_changed().unwrap());
        let val = rx.borrow_and_update();
        assert_eq!(val.as_ref().unwrap().block_hash, replacement_hash);
    }

    #[test]
    fn test_subscribe_receives_on_advance() {
        let cache = make_test_cache();
        let mut rx = cache.subscribe_pending_sequence();

        cache.handle_pending_sequence(make_pending_sequence(1, B256::ZERO), 0).unwrap();
        rx.borrow_and_update(); // consume

        let seq2 = make_pending_sequence(2, B256::repeat_byte(0x01));
        let seq2_hash = seq2.block_hash;
        cache.handle_pending_sequence(seq2, 0).unwrap();

        assert!(rx.has_changed().unwrap());
        let val = rx.borrow_and_update();
        assert_eq!(val.as_ref().unwrap().get_height(), 2);
        assert_eq!(val.as_ref().unwrap().block_hash, seq2_hash);
    }

    #[test]
    fn test_outer_handle_canonical_block_updates_canon_info() {
        let cache = make_test_cache();
        let seq = make_pending_sequence(1, B256::ZERO);
        cache.handle_pending_sequence(seq, 0).unwrap();
        cache
            .handle_pending_sequence(make_pending_sequence(2, B256::repeat_byte(0x01)), 0)
            .unwrap();

        let canon_hash = B256::repeat_byte(0xCC);
        cache.handle_canonical_block((1, canon_hash), false);

        assert_eq!(cache.get_canon_height(), 1);
    }

    #[test]
    fn test_flush_resets_confirm_height_to_canon() {
        let mut inner = TestInner::new();
        inner.handle_pending_sequence(make_pending_sequence(1, B256::ZERO), 0).unwrap();

        inner.handle_canonical_block((1, B256::repeat_byte(0xAA)), false);

        assert_eq!(inner.confirm_height, 1);
        assert!(inner.pending_cache.is_none());
    }
}
