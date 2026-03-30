pub(crate) mod assemble;
pub(crate) mod validator;

pub(crate) use validator::FlashblockSequenceValidator;

use alloy_eips::eip4895::Withdrawal;
use alloy_rpc_types_engine::PayloadId;
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;

use reth_optimism_primitives::OpReceipt;
use reth_provider::{
    BlockNumReader, ChangeSetReader, DatabaseProviderFactory, PruneCheckpointReader,
    StageCheckpointReader, StorageChangeSetReader, StorageSettingsCache,
};
use reth_revm::cached::CachedReads;
use reth_trie::updates::TrieUpdates;

pub(crate) struct BuildArgs<I> {
    pub(crate) base: OpFlashblockPayloadBase,
    pub(crate) payload_id: PayloadId,
    pub(crate) transactions: I,
    pub(crate) withdrawals: Vec<Withdrawal>,
    pub(crate) last_flashblock_index: u64,
    pub(crate) target_index: u64,
}

/// Cached prefix execution data used to resume canonical builds.
#[derive(Debug, Clone, Default)]
pub struct PrefixExecutionMeta {
    /// The payload ID of the latest flashblocks sequence.
    pub(crate) payload_id: PayloadId,
    /// Cached reads from execution for reuse.
    pub cached_reads: CachedReads,
    /// Number of leading transactions covered by cached execution.
    pub(crate) cached_tx_count: usize,
    /// Total gas used by the cached prefix.
    pub(crate) gas_used: u64,
    /// Total blob/DA gas used by the cached prefix.
    pub(crate) blob_gas_used: u64,
    /// The last flashblock index of the latest flashblocks sequence.
    pub(crate) last_flashblock_index: u64,
    /// Accumulated trie updates across sequence incremental executions.
    pub(crate) accumulated_trie_updates: TrieUpdates,
}

/// Strategy describing how to compute the state root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateRootStrategy {
    /// Use the state root task (background sparse trie computation).
    StateRootTask,
    /// Run the parallel state root computation on the calling thread.
    Parallel,
    /// Fall back to synchronous computation via the state provider.
    Synchronous,
}

/// Receipt requirements for cache-resume flow.
pub trait FlashblockReceipt: Clone {
    /// Adds `gas_offset` to each receipt's `cumulative_gas_used`.
    fn add_cumulative_gas_offset(&mut self, gas_offset: u64);
}

impl FlashblockReceipt for OpReceipt {
    fn add_cumulative_gas_offset(&mut self, gas_offset: u64) {
        if gas_offset == 0 {
            return;
        }
        let inner = self.as_receipt_mut();
        inner.cumulative_gas_used = inner.cumulative_gas_used.saturating_add(gas_offset);
    }
}

/// Trait alias for the bounds required on a provider factory to create an
/// [`OverlayStateProviderFactory`] that supports parallel and serial state
/// root computation.
pub trait OverlayProviderFactory:
    DatabaseProviderFactory<
        Provider: StageCheckpointReader
                      + PruneCheckpointReader
                      + BlockNumReader
                      + ChangeSetReader
                      + StorageChangeSetReader
                      + StorageSettingsCache,
    > + Clone
    + Send
    + Sync
    + 'static
{
}

impl<T> OverlayProviderFactory for T where
    T: DatabaseProviderFactory<
            Provider: StageCheckpointReader
                          + PruneCheckpointReader
                          + BlockNumReader
                          + ChangeSetReader
                          + StorageChangeSetReader
                          + StorageSettingsCache,
        > + Clone
        + Send
        + Sync
        + 'static
{
}
