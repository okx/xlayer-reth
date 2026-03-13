mod confirm;
pub mod pending;
pub(crate) mod raw;
pub(crate) mod utils;

pub use confirm::ConfirmCache;
pub use pending::PendingSequence;
pub use raw::RawFlashblocksCache;

use crate::PendingSequenceRx;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::*;

use alloy_consensus::BlockHeader;
use alloy_primitives::{TxHash, B256};
use alloy_rpc_types_eth::{BlockId, BlockNumberOrTag};
use reth_chain_state::{ExecutedBlock, MemoryOverlayStateProvider};
use reth_primitives_traits::{NodePrimitives, ReceiptTy, SealedHeaderFor};
use reth_rpc_eth_types::block::BlockAndReceipts;
use reth_storage_api::StateProviderBox;

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
}

// FlashblockStateCache read interfaces
impl<N: NodePrimitives> FlashblockStateCache<N> {
    /// Creates a new [`FlashblockStateCache`].
    pub fn new() -> Self {
        Self { inner: Arc::new(RwLock::new(FlashblockStateCacheInner::new())) }
    }
}

// FlashblockStateCache read height interfaces
impl<N: NodePrimitives> FlashblockStateCache<N> {
    /// Returns the current confirmed height.
    pub fn get_confirm_height(&self) -> u64 {
        self.inner.read().confirm_height
    }

    /// Returns the current pending height, if any.
    pub fn get_pending_height(&self) -> Option<u64> {
        self.inner.read().pending_cache.as_ref().map(|p| p.get_height())
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

    /// Returns a cloned watch receiver for pending sequence updates.
    /// Used by `eth_sendRawTransactionSync` to watch for sub-block preconfirmation.
    pub fn subscribe_pending_sequence(&self) -> PendingSequenceRx<N> {
        self.inner.read().subscribe_pending_sequence()
    }

    /// Creates a `StateProviderBox` that overlays the flashblock execution state on top of the
    /// canonical state for the given block ID. Instantiates a `MemoryOverlayStateProvider` by
    /// getting the ordered `ExecutedBlock`s from the cache, and overlaying them on top of the
    /// canonical state provider.
    ///
    /// For a specific block number/hash, returns all confirm cache blocks up to that height.
    /// For `Pending`, it also includes the current pending executed block state.
    /// For `Latest`, resolves to the confirm height.
    /// Returns `None` if the target block is not in the flashblocks cache.
    ///
    /// # Safety of the overlay
    /// The returned blocks are meant to be layered on top of a canonical `StateProviderBox`
    /// via `MemoryOverlayStateProvider`. This is correct **if and only if** the overlay
    /// blocks form a contiguous chain from some height down to `canonical_height + 1`
    /// (or `canonical_height` itself in the redundant-but-safe race case).
    ///
    /// **Safe (redundant overlap)**: Due to a race between canonical commit and confirm
    /// cache flush, the lowest overlay block may equal the canonical height. For example,
    /// canonical is at height `x` and the overlay contains `[x+2, x+1, x]`. This is safe
    /// because `MemoryOverlayStateProvider` checks overlay blocks first (newest-to-oldest)
    /// — the duplicate `BundleState` at height `x` contains changes identical to what
    /// canonical already applied, so the result is correct regardless of which source
    /// resolves the query.
    ///
    /// **State inconsistency (gap in overlay)**: If an intermediate block is missing (e.g.
    /// overlay has `[x+2, x]` but not `x+1`), any account modified only at height `x+1`
    /// would be invisible — the query falls through to canonical, returning stale state.
    ///
    /// **State inconsistency (canonical too far behind)**: If the canonical height is more
    /// than one block below the lowest overlay block (e.g. canonical at `x-2`, lowest overlay
    /// at `x`), changes at height `x-1` are not covered by either source.
    ///
    /// Both failure modes reduce to: every height between `canonical_height + 1` and the
    /// target must be present in the overlay. This invariant is naturally maintained by
    /// `handle_confirmed_block` (rejects non-consecutive heights) and the pending block always
    /// being `confirm_height + 1`.
    ///
    /// On validation failure (non-contiguous overlay or gap to canonical), the cache is
    /// flushed and `None` is returned.
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
            Ok(Some(blocks)) => blocks,
            Ok(None) => return None,
            Err(e) => {
                warn!(target: "flashblocks", "Failed to get flashblocks state provider: {e}. Flushing cache");
                self.inner.write().flush();
                return None;
            }
        };
        Some((
            Box::new(MemoryOverlayStateProvider::new(canonical_state, in_memory)),
            block.clone_sealed_header(),
        ))
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
    ) -> eyre::Result<()> {
        self.inner.write().handle_pending_sequence(pending_sequence)
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
    pub fn handle_canonical_block(&self, block_number: u64, reorg: bool) {
        self.inner.write().handle_canonical_block(block_number, reorg)
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
    canon_height: u64,
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
            canon_height: 0,
            pending_sequence_rx: rx,
            pending_sequence_tx: tx,
        }
    }

    fn flush(&mut self) {
        warn!(target: "flashblocks", "Flushing flashblocks state cache");
        self.pending_cache = None;
        self.confirm_height = 0;
        self.confirm_cache.clear();
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
    ) -> eyre::Result<()> {
        let pending_height = pending_sequence.get_height();
        let expected_height = self.confirm_height + 1;

        if pending_height == expected_height + 1 {
            // Pending tip has advanced — update pending state, and optimistically
            // commit current pending to confirm cache
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
            self.pending_sequence_tx.send(Some(pending_sequence));
        } else if pending_height == expected_height {
            // Replace the existing pending sequence
            self.pending_cache = Some(pending_sequence.clone());
            self.pending_sequence_tx.send(Some(pending_sequence));
        } else {
            return Err(eyre::eyre!(
                "polluted state cache - not next consecutive pending height block"
            ));
        }
        Ok(())
    }

    fn handle_canonical_block(&mut self, canon_height: u64, reorg: bool) {
        let pending_stale =
            self.pending_cache.as_ref().is_some_and(|p| p.get_height() <= canon_height);
        if pending_stale || reorg {
            warn!(
                target: "flashblocks",
                canonical_height = canon_height,
                cache_height = self.confirm_height,
                canonical_reorg = reorg,
                pending_stale = pending_stale,
                "Reorg or pending stale detected on handle canonical block",
            );
            self.flush();
        } else {
            debug!(
                target: "flashblocks",
                canonical_height = canon_height,
                cache_height = self.confirm_height,
                "Flashblocks state cache received canonical block, flushing confirm cache up to canonical height"
            );
            self.confirm_cache.flush_up_to_height(canon_height);
        }
        // Update state heights
        self.canon_height = canon_height;
        self.confirm_height = self.confirm_height.max(canon_height);
    }

    pub fn get_confirmed_block(&self) -> Option<BlockAndReceipts<N>> {
        self.get_block_by_number(self.confirm_height)
    }

    pub fn get_pending_block(&self) -> Option<BlockAndReceipts<N>> {
        self.pending_cache.as_ref().map(|p| p.get_block_and_receipts())
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

    /// Returns all `ExecutedBlock`s up to `target_height`.
    fn get_executed_blocks_up_to_height(
        &self,
        target_height: u64,
    ) -> eyre::Result<Option<Vec<ExecutedBlock<N>>>> {
        if self.confirm_height == 0
            || self.canon_height == 0
            || target_height > self.confirm_height + 1
            || target_height <= self.canon_height
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
                .get_executed_blocks_up_to_height(target_height, self.canon_height)?,
        );
        Ok(Some(blocks))
    }

    pub fn subscribe_pending_sequence(&self) -> PendingSequenceRx<N> {
        self.pending_sequence_rx.clone()
    }
}
