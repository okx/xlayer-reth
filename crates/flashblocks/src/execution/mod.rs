pub(crate) mod processor;
pub(crate) mod validator;

use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;
use reth_optimism_primitives::OpReceipt;

pub(crate) struct BuildArgs<I> {
    pub(crate) base: OpFlashblockPayloadBase,
    pub(crate) transactions: I,
    pub(crate) last_flashblock_index: u64,
}

/// State root strategies during flashblocks sequence validation.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum StateRootStrategy {
    /// Synchronous state root computation
    #[default]
    Synchronous,
    /// Parallel state root computation
    Parallel,
    /// Sparse trie task
    SparseTrieTask,
}

/// Receipt requirements for cache-resume flow.
pub trait FlashblockCachedReceipt: Clone {
    /// Adds `gas_offset` to each receipt's `cumulative_gas_used`.
    fn add_cumulative_gas_offset(receipts: &mut [Self], gas_offset: u64);
}

impl FlashblockCachedReceipt for OpReceipt {
    fn add_cumulative_gas_offset(receipts: &mut [Self], gas_offset: u64) {
        if gas_offset == 0 {
            return;
        }
        for receipt in receipts {
            let inner = receipt.as_receipt_mut();
            inner.cumulative_gas_used = inner.cumulative_gas_used.saturating_add(gas_offset);
        }
    }
}
