mod confirm;
pub mod pending;
pub(crate) mod raw;
pub(crate) mod utils;

pub use confirm::ConfirmCache;
pub use pending::PendingSequence;
pub use raw::RawFlashblocksCache;

use parking_lot::RwLock;
use std::sync::Arc;
use tracing::*;

use alloy_primitives::{TxHash, B256};
use reth_primitives_traits::{NodePrimitives, ReceiptTy};
use reth_rpc_eth_types::block::BlockAndReceipts;

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

    /// Returns the current pending height.
    pub fn get_pending_height(&self) -> u64 {
        let inner = self.inner.read();
        inner.pending_cache.as_ref().map_or(inner.confirm_height, |p| p.get_height())
    }

    /// Returns the block for the given block number, if cached.
    pub fn get_block_by_number(&self, num: u64) -> Option<BlockAndReceipts<N>> {
        self.inner.read().confirm_cache.get_block_by_number(num)
    }

    /// Returns the confirmed block for the given block hash, if cached.
    pub fn get_block_by_hash(&self, hash: &B256) -> Option<BlockAndReceipts<N>> {
        self.inner.read().confirm_cache.get_block_by_hash(hash)
    }

    // --- Pending block ---

    /// Returns the current pending block and receipts, if any.
    pub fn get_pending_block(&self) -> Option<BlockAndReceipts<N>> {
        self.inner.read().pending_cache.as_ref().map(|p| p.get_block())
    }

    // --- Transaction/receipt lookup (pending + confirm) ---

    /// Looks up cached transaction info by hash: pending sequence first, then
    /// confirm cache. Returns `None` if the tx is not in either cache layer.
    pub fn get_tx_info(&self, tx_hash: &TxHash) -> Option<CachedTxInfo<N>> {
        self.inner.read().get_tx_info(tx_hash)
    }

    // --- Hash/number mapping ---

    /// Returns the block hash for the given block number, if cached in the
    /// confirm cache.
    pub fn get_block_hash(&self, num: u64) -> Option<B256> {
        self.inner.read().confirm_cache.hash_for_number(num)
    }

    /// Returns the block number for the given block hash, if cached in the
    /// confirm cache.
    pub fn get_block_number(&self, hash: &B256) -> Option<u64> {
        self.inner.read().confirm_cache.number_for_hash(hash)
    }

    // --- Range queries (for `eth_getLogs`) ---

    /// Returns all cached confirmed blocks in the inclusive range `[start..=end]`.
    /// Blocks not present in the cache are skipped (the caller must fill gaps
    /// from the provider).
    pub fn get_blocks_in_range(&self, start: u64, end: u64) -> Vec<BlockAndReceipts<N>> {
        let inner = self.inner.read();
        let mut result = Vec::new();
        for num in start..=end {
            if let Some(bar) = inner.confirm_cache.get_block_by_number(num) {
                result.push(bar);
            }
        }
        result
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
    /// Note that this state update is fallible as it detects potential reorgs, and
    /// triggers cache flush on invalidate entries. An entry is invalidated if the
    /// incoming pending sequence height is not the next pending height or current
    /// pending height.
    pub fn handle_pending_sequence(
        &self,
        pending_sequence: PendingSequence<N>,
    ) -> eyre::Result<()> {
        self.inner.write().handle_pending_sequence(pending_sequence)
    }

    /// Handles a canonical block commit by flushing stale confirmed entries and
    /// the pending state if it matches the committed block.
    ///
    /// If reorg flag is set, the flashblocks state cache will be default be flushed.
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
    /// The highest confirmed block height of from both the confirm cache or
    /// the pending cache.
    confirm_height: u64,
}

impl<N: NodePrimitives> FlashblockStateCacheInner<N> {
    fn new() -> Self {
        Self { pending_cache: None, confirm_cache: ConfirmCache::new(), confirm_height: 0 }
    }

    /// Looks up cached transaction info by hash: pending sequence first, then
    /// confirm cache. Returns `None` if the tx is not in either cache layer.
    fn get_tx_info(&self, tx_hash: &TxHash) -> Option<CachedTxInfo<N>> {
        self.pending_cache
            .as_ref()
            .and_then(|p| p.get_tx_info(tx_hash))
            .or_else(|| self.confirm_cache.get_tx_info(tx_hash))
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
        block: BlockAndReceipts<N>,
    ) -> eyre::Result<()> {
        if block_number <= self.confirm_height {
            return Err(eyre::eyre!(
                "polluted state cache - trying to commit lower confirm height block"
            ));
        }
        if block_number != self.confirm_height + 1 {
            return Err(eyre::eyre!(
                "polluted state cache - not next consecutive confirm height block"
            ));
        }

        self.confirm_height = block_number;
        self.confirm_cache.insert(block_number, block)?;
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
            self.handle_confirmed_block(expected_height, sequence.get_block())?;
            self.pending_cache = Some(pending_sequence);
        } else if pending_height == expected_height {
            // Replace the existing pending sequence
            self.pending_cache = Some(pending_sequence);
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
                reorg,
                "Flushing flashblocks state cache",
            );
            self.flush();
        } else {
            debug!(
                target: "flashblocks",
                canonical_height = canon_height,
                cache_height = self.confirm_height,
                "Flashblocks state cache received canonical block, flushing confirm cache up to canonical height"
            );
            self.confirm_cache.flush_up_to(canon_height);
        }
        self.confirm_height = self.confirm_height.max(canon_height);
    }

    fn flush(&mut self) {
        self.confirm_height = 0;
        self.pending_cache = None;
        self.confirm_cache.clear();
    }
}
