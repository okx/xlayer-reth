use crate::{cache::CachedTxInfo, execution::PrefixExecutionMeta};
use derive_more::Deref;
use std::collections::HashMap;

use alloy_consensus::BlockHeader;
use alloy_primitives::{TxHash, B256};
use reth_primitives_traits::NodePrimitives;
use reth_primitives_traits::SealedHeader;
use reth_rpc_eth_types::{block::BlockAndReceipts, PendingBlock};

/// The pending flashblocks sequence built with all received `OpFlashblockPayload`
/// alongside the metadata for the last added flashblock.
#[derive(Debug, Clone, Deref)]
pub struct PendingSequence<N: NodePrimitives> {
    /// Locally built full pending block of the latest flashblocks sequence.
    #[deref]
    pub pending: PendingBlock<N>,
    /// Transaction index: tx hash → cached tx info for O(1) tx/receipt lookups.
    pub tx_index: HashMap<TxHash, CachedTxInfo<N>>,
    /// The current block hash of the latest flashblocks sequence.
    pub block_hash: B256,
    /// Parent hash of the built block (may be non-canonical or canonical).
    pub parent_header: SealedHeader<N::BlockHeader>,
    /// Prefix execution metadata for incremental builds.
    pub prefix_execution_meta: PrefixExecutionMeta,
    /// Target index of the latest flashblock in the sequence.
    pub target_index: u64,
}

impl<N: NodePrimitives> PendingSequence<N> {
    pub fn get_hash(&self) -> B256 {
        self.block_hash
    }

    pub fn get_height(&self) -> u64 {
        self.pending.block().number()
    }

    pub fn get_block_and_receipts(&self) -> BlockAndReceipts<N> {
        self.pending.to_block_and_receipts()
    }

    pub fn get_tx_info(&self, tx_hash: &TxHash) -> Option<(CachedTxInfo<N>, BlockAndReceipts<N>)> {
        self.tx_index
            .get(tx_hash)
            .cloned()
            .map(|tx_info| (tx_info, self.pending.to_block_and_receipts()))
    }

    pub fn get_last_flashblock_index(&self) -> u64 {
        self.prefix_execution_meta.last_flashblock_index
    }

    pub fn is_target_flashblock(&self) -> bool {
        self.target_index > 0 && self.get_last_flashblock_index() >= self.target_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_executed_block, mock_tx};
    use std::{collections::HashMap, time::Instant};

    use alloy_consensus::Receipt;
    use alloy_primitives::B256;
    use op_alloy_consensus::OpReceipt;

    use reth_optimism_primitives::OpPrimitives;
    use reth_rpc_eth_types::PendingBlock;

    fn make_pending_sequence(block_number: u64) -> PendingSequence<OpPrimitives> {
        let executed = make_executed_block(block_number, B256::ZERO);
        let block_hash = executed.recovered_block.hash();
        let pending_block = PendingBlock::with_executed_block(Instant::now(), executed);
        PendingSequence {
            pending: pending_block,
            tx_index: HashMap::new(),
            block_hash,
            parent_header: Default::default(),
            prefix_execution_meta: Default::default(),
            target_index: 0,
        }
    }

    fn make_pending_sequence_with_txs(
        block_number: u64,
        tx_count: usize,
    ) -> PendingSequence<OpPrimitives> {
        let executed = make_executed_block(block_number, B256::ZERO);
        let block_hash = executed.recovered_block.hash();
        let mut tx_index = HashMap::new();
        for i in 0..tx_count {
            let tx = mock_tx(i as u64);
            let tx_hash = tx.tx_hash();
            let receipt = OpReceipt::Eip7702(Receipt {
                status: true.into(),
                cumulative_gas_used: 21_000 * (i as u64 + 1),
                logs: vec![],
            });
            tx_index.insert(
                tx_hash,
                CachedTxInfo { block_number, block_hash, tx_index: i as u64, tx, receipt },
            );
        }

        let pending_block = PendingBlock::with_executed_block(Instant::now(), executed);
        PendingSequence {
            pending: pending_block,
            tx_index,
            block_hash,
            parent_header: Default::default(),
            prefix_execution_meta: Default::default(),
            target_index: 0,
        }
    }

    #[test]
    fn test_pending_sequence_get_hash_returns_stored_block_hash() {
        let cache = make_pending_sequence(42);
        assert_eq!(cache.get_hash(), cache.block_hash);
    }

    #[test]
    fn test_pending_sequence_get_height_returns_block_number() {
        let cache = make_pending_sequence(99);
        assert_eq!(cache.get_height(), 99);
    }

    #[test]
    fn test_pending_sequence_get_block_and_receipts_empty_receipts_on_no_tx_block() {
        let cache = make_pending_sequence(3);
        let bar = cache.get_block_and_receipts();
        assert!(bar.receipts.is_empty());
    }

    #[test]
    fn test_pending_sequence_get_tx_info_returns_none_for_unknown_hash() {
        let cache = make_pending_sequence_with_txs(10, 2);
        assert!(cache.get_tx_info(&B256::repeat_byte(0xFF)).is_none());
    }

    #[test]
    fn test_pending_sequence_get_tx_info_returns_correct_info_for_known_tx() {
        let cache = make_pending_sequence_with_txs(42, 3);
        let (tx_hash, expected_info) = cache.tx_index.iter().next().unwrap();
        let (info, bar) = cache.get_tx_info(tx_hash).expect("known tx hash should return Some");
        assert_eq!(info.block_number, 42);
        assert_eq!(info.block_hash, cache.block_hash);
        assert_eq!(info.tx_index, expected_info.tx_index);
        assert_eq!(*info.tx.tx_hash(), *tx_hash);
        assert_eq!(bar.block.number(), 42);
    }
}
