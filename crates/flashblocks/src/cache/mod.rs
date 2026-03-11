mod block;
mod confirm;
mod factory;
mod header;
mod id;
mod pending;
pub(crate) mod raw;
mod receipt;
mod transaction;
mod utils;

pub use raw::RawFlashblocksCache;

use confirm::ConfirmCache;
use pending::PendingSequence;
use utils::{block_from_bar, StateCacheProvider};

use core::ops::RangeBounds;
use parking_lot::RwLock;
use std::sync::Arc;

use alloy_consensus::BlockHeader;
use alloy_primitives::{BlockNumber, TxHash, B256};
use reth_primitives_traits::{NodePrimitives, ReceiptTy};
use reth_rpc_eth_types::block::BlockAndReceipts;
use reth_storage_api::{errors::provider::ProviderResult, BlockNumReader};

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
/// Composed of:
/// - **Pending**: the in-progress flashblock sequence being built from incoming
///   `OpFlashblockPayload` deltas (at most one active sequence at a time).
/// - **Confirmed**: completed flashblock sequences that have been committed but
///   are still ahead of the canonical chain.
///
/// Implements all reth provider traits using the flashblocks state cache layer
/// as an overlay on top of the underlying chainstate `Provider`.
/// (`BlockReaderIdExt`, `StateProviderFactory`, etc.)
///
/// **Lookup strategy:**
/// - **Confirmed state** (by hash/number): Check the flashblocks state cache
///   layer first, then fall back to the chainstate provider.
/// - **Latest**: Compare the flashblocks state cache's highest height vs the
///   chainstate provider's best height. Return whichever is higher, on tie we
///   prefer the chainstate provider.
/// - **Pending**: Returns the pending state from the flashblocks state cache.
/// - **All other IDs** (safe, finalized, historical, index-based): delegate
///   directly to the chainstate provider.
///
/// Uses `Arc<RwLock>` for thread safety — a single lock protects all inner
/// state, ensuring atomic operations across pending, confirmed, and height
/// state (e.g. reorg detection + flush + insert in `handle_confirmed_block`).
#[derive(Debug, Clone)]
pub struct FlashblockStateCache<N: NodePrimitives, Provider> {
    pub(super) inner: Arc<RwLock<FlashblockStateCacheInner<N>>>,
    pub(super) provider: Provider,
}

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> FlashblockStateCache<N, Provider> {
    /// Creates a new [`FlashblockStateCache`].
    pub fn new(provider: Provider) -> eyre::Result<Self> {
        let canon_height = provider.best_block_number()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(FlashblockStateCacheInner::new(canon_height))),
            provider,
        })
    }

    /// Returns a reference to the underlying chainstate provider.
    pub const fn provider(&self) -> &Provider {
        &self.provider
    }

    /// Handles a newly confirmed block by detecting reorgs, flushing invalidated
    /// entries, and inserting into the confirm blocks cache.
    pub fn handle_confirmed_block(
        &self,
        block_number: u64,
        block_hash: B256,
        block: BlockAndReceipts<N>,
    ) -> eyre::Result<()> {
        self.inner.write().handle_confirmed_block(block_number, block_hash, block)
    }

    /// Handles updating the pending state with a newly executed pending flashblocks
    /// sequence. Note that it will replace any existing pending sequence.
    pub fn handle_pending_sequence(&self, pending_sequence: PendingSequence<N>) {
        self.inner.write().handle_pending_sequence(pending_sequence)
    }

    pub fn handle_canonical_block(&self, block_number: u64, block_hash: B256) {
        self.inner.write().handle_canonical_block(block_number, block_hash)
    }

    /// Returns the current confirmed cache height, if any blocks have been confirmed.
    pub fn get_confirm_height(&self) -> Option<u64> {
        self.inner.read().confirm_height
    }

    /// Returns the current pending height, if any flashblocks have been executed.
    pub fn get_pending_height(&self) -> Option<u64> {
        self.inner.read().pending.as_ref().map(|p| p.pending.block().number())
    }

    /// Collects items from an inclusive block number range `[start..=end]`, using
    /// the confirm cache as an overlay on top of the provider.
    ///
    /// Walks backward from `end`, collecting consecutive cache hits via `from_cache`.
    /// Delegates the remaining prefix `[start..=provider_end]` to `from_provider`.
    ///
    /// When `predicate` is `Some`, items are filtered: the provider receives the
    /// predicate to stop early, and cached items are checked before appending.
    /// When `None`, all items in the range are collected unconditionally.
    pub(super) fn collect_cached_block_range<T>(
        &self,
        start: BlockNumber,
        end: BlockNumber,
        from_cache: impl Fn(&BlockAndReceipts<N>) -> T,
        from_provider: impl FnOnce(
            core::ops::RangeInclusive<BlockNumber>,
            &mut dyn FnMut(&T) -> bool,
        ) -> ProviderResult<Vec<T>>,
        predicate: Option<&mut dyn FnMut(&T) -> bool>,
    ) -> ProviderResult<Vec<T>> {
        let inner = self.inner.read();
        let mut cached_bars = Vec::new();
        let mut provider_end = end;
        let mut index = end;
        loop {
            if let Some(bar) = inner.confirm_cache.get_block_by_number(index) {
                cached_bars.push(bar);
                provider_end = index.saturating_sub(1);
            } else {
                break;
            }
            if index == start {
                break;
            }
            index -= 1;
        }
        cached_bars.reverse();
        drop(inner);

        let has_provider_range =
            provider_end >= start && cached_bars.len() < (end - start + 1) as usize;

        let mut always_true = |_: &T| true;
        let predicate = predicate.unwrap_or(&mut always_true);

        let mut predicate_stopped = false;
        let mut result = if has_provider_range {
            let items = from_provider(start..=provider_end, predicate)?;
            let expected = (provider_end - start + 1) as usize;
            if items.len() < expected {
                predicate_stopped = true;
            }
            items
        } else {
            Vec::new()
        };

        if !predicate_stopped {
            for bar in &cached_bars {
                let item = from_cache(bar);
                if !predicate(&item) {
                    break;
                }
                result.push(item);
            }
        }

        Ok(result)
    }

    /// Resolves an `impl RangeBounds<BlockNumber>` into an inclusive `(start, end)` pair.
    /// Matches reth's blockchain provider's convert_range_bounds semantics, and unbounded
    /// ends are resolved to `best_block_number`.
    pub(super) fn resolve_range_bounds(
        &self,
        range: impl RangeBounds<BlockNumber>,
    ) -> ProviderResult<(BlockNumber, BlockNumber)> {
        let start = match range.start_bound() {
            core::ops::Bound::Included(&n) => n,
            core::ops::Bound::Excluded(&n) => n + 1,
            core::ops::Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            core::ops::Bound::Included(&n) => n,
            core::ops::Bound::Excluded(&n) => n - 1,
            core::ops::Bound::Unbounded => self.best_block_number()?,
        };
        Ok((start, end))
    }
}

/// Inner state of the flashblocks state cache.
#[derive(Debug)]
pub(super) struct FlashblockStateCacheInner<N: NodePrimitives> {
    /// The current in-progress pending flashblock sequence, if any.
    pub(super) pending: Option<PendingSequence<N>>,
    /// Cache of confirmed flashblock sequences ahead of the canonical chain.
    pub(super) confirm_cache: ConfirmCache<N>,
    /// The highest confirmed block height.
    pub(super) confirm_height: Option<u64>,
    /// The highest canonical block height.
    pub(super) canon_height: u64,
}

impl<N: NodePrimitives> FlashblockStateCacheInner<N> {
    fn new(canon_height: u64) -> Self {
        Self {
            pending: None,
            confirm_cache: ConfirmCache::new(),
            confirm_height: None,
            canon_height,
        }
    }

    /// Handles a newly confirmed block with reorg detection.
    fn handle_confirmed_block(
        &mut self,
        block_number: u64,
        block_hash: B256,
        block: BlockAndReceipts<N>,
    ) -> eyre::Result<()> {
        // Validation checks
        if let Some(confirm_height) = self.confirm_height {
            if block_number <= confirm_height {
                // Reorg detected - confirm cache is polluted
                return Err(eyre::eyre!(
                    "polluted state cache - trying to commit lower confirm height block"
                ));
            }
            if block_number != confirm_height + 1 {
                return Err(eyre::eyre!(
                    "polluted state cache - not next consecutive confirm height block"
                ));
            }
        }

        // Commit new confirmed block to state cache
        self.confirm_height = Some(block_number);
        self.confirm_cache.insert(block_number, block_hash, block)?;
        Ok(())
    }

    /// Looks up cached transaction info by hash: pending sequence first, then
    /// confirm cache. Returns `None` if the tx is not in either cache layer.
    pub(super) fn get_tx_info(&self, tx_hash: &TxHash) -> Option<CachedTxInfo<N>> {
        self.pending
            .as_ref()
            .and_then(|p| p.get_tx_info(tx_hash))
            .or_else(|| self.confirm_cache.get_tx_info(tx_hash))
    }

    fn handle_pending_sequence(&mut self, pending_sequence: PendingSequence<N>) {
        self.pending = Some(pending_sequence);
    }

    fn handle_canonical_block(&mut self, block_number: u64, block_hash: B256) {
        self.canon_height = block_number;
        self.confirm_cache.flush_up_to(block_number);
        if self.pending.as_ref().and_then(|p| p.block_hash) == Some(block_hash) {
            self.pending = None;
        }
    }
}
