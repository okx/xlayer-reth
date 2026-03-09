use crate::cache::{confirm::ConfirmCache, pending::PendingSequence};
use parking_lot::RwLock;
use std::sync::Arc;

use alloy_consensus::BlockHeader;
use alloy_primitives::B256;
use reth_primitives_traits::NodePrimitives;
use reth_rpc_eth_types::block::BlockAndReceipts;

/// Top-level controller state cache for the flashblocks RPC layer.
///
/// Composed of:
/// - **Pending**: the in-progress flashblock sequence being built from incoming
///   `OpFlashblockPayload` deltas (at most one active sequence at a time).
/// - **Confirmed**: completed flashblock sequences that have been committed but
///   are still ahead of the canonical chain.
///
/// Uses `Arc<RwLock>` for thread safety — a single lock protects all inner
/// state, ensuring atomic operations across pending, confirmed, and height
/// state (e.g. reorg detection + flush + insert in `handle_confirmed_block`).
#[derive(Debug, Clone)]
pub struct StateCache<N: NodePrimitives> {
    inner: Arc<RwLock<StateCacheInner<N>>>,
}

impl<N: NodePrimitives> StateCache<N> {
    /// Creates a new [`StateCache`].
    pub fn new(canon_height: u64) -> Self {
        Self { inner: Arc::new(RwLock::new(StateCacheInner::new(canon_height))) }
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
}

/// Inner state of the flashblocks state cache.
#[derive(Debug)]
struct StateCacheInner<N: NodePrimitives> {
    /// The current in-progress pending flashblock sequence, if any.
    pending: Option<PendingSequence<N>>,
    /// Cache of confirmed flashblock sequences ahead of the canonical chain.
    confirm_cache: ConfirmCache<N>,
    /// The highest confirmed block height.
    confirm_height: Option<u64>,
    /// The highest canonical block height.
    canon_height: u64,
}

impl<N: NodePrimitives> StateCacheInner<N> {
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
        if let Some(confirm_height) = self.confirm_height {
            // Reorg detection: incoming block is at or behind the last confirmed height.
            if block_number <= confirm_height {
                self.confirm_cache.flush_from(block_number);
            }
        }

        self.confirm_cache.insert(block_number, block_hash, block)?;

        // Sanity check: the inserted block must now be the highest in the cache
        self.confirm_height = Some(block_number);
        if self.confirm_height != self.confirm_cache.latest_block_number() {
            return Err(eyre::eyre!(
                "confirmed cache latest height mismatch inserted block height: {block_number}"
            ));
        }
        Ok(())
    }

    fn handle_pending_sequence(&mut self, pending_sequence: PendingSequence<N>) {
        self.pending = Some(pending_sequence);
    }

    fn handle_canonical_block(&mut self, block_number: u64, block_hash: B256) {
        self.canon_height = block_number;
        self.confirm_cache.flush_up_to(block_number);
        if self.pending.as_ref().map(|p| p.block_hash) == Some(block_hash) {
            self.pending = None;
        }
    }
}
