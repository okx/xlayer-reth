use crate::cache::CachedTxInfo;
use derive_more::Deref;
use std::collections::HashMap;

use alloy_consensus::BlockHeader;
use alloy_primitives::{TxHash, B256};
use reth_primitives_traits::NodePrimitives;
use reth_revm::cached::CachedReads;
use reth_rpc_eth_types::{block::BlockAndReceipts, PendingBlock};

/// The pending flashblocks sequence built with all received OpFlashblockPayload
/// alongside the metadata for the last added flashblock.
#[derive(Debug, Clone, Deref)]
pub struct PendingSequence<N: NodePrimitives> {
    /// Locally built full pending block of the latest flashblocks sequence.
    #[deref]
    pub pending: PendingBlock<N>,
    /// Transaction index: tx hash → cached tx info for O(1) tx/receipt lookups.
    pub tx_index: HashMap<TxHash, CachedTxInfo<N>>,
    /// Cached reads from execution for reuse.
    pub cached_reads: CachedReads,
    /// The current block hash of the latest flashblocks sequence.
    pub block_hash: B256,
    /// Parent hash of the built block (may be non-canonical or canonical).
    pub parent_hash: B256,
    /// The last flashblock index of the latest flashblocks sequence.
    pub last_flashblock_index: u64,
}

impl<N: NodePrimitives> PendingSequence<N> {
    /// Create new pending flashblock.
    pub const fn new(
        pending: PendingBlock<N>,
        tx_index: HashMap<TxHash, CachedTxInfo<N>>,
        cached_reads: CachedReads,
        block_hash: B256,
        parent_hash: B256,
        last_flashblock_index: u64,
    ) -> Self {
        Self { pending, tx_index, cached_reads, block_hash, parent_hash, last_flashblock_index }
    }

    pub fn get_hash(&self) -> B256 {
        self.block_hash
    }

    pub fn get_height(&self) -> u64 {
        self.pending.block().number()
    }

    pub fn get_block_and_receipts(&self) -> BlockAndReceipts<N> {
        self.pending.to_block_and_receipts()
    }

    /// Returns the cached transaction info for the given tx hash, if present
    /// in the pending sequence.
    pub fn get_tx_info(&self, tx_hash: &TxHash) -> Option<(CachedTxInfo<N>, BlockAndReceipts<N>)> {
        self.tx_index
            .get(tx_hash)
            .cloned()
            .map(|tx_info| (tx_info, self.pending.to_block_and_receipts()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{BlockHeader, Header, Receipt, TxEip7702};
    use alloy_primitives::{Address, Bytes, PrimitiveSignature, B256, U256};
    use op_alloy_consensus::{OpReceipt, OpTypedTransaction};
    use reth_chain_state::{ComputedTrieData, ExecutedBlock};
    use reth_execution_types::{BlockExecutionOutput, BlockExecutionResult};
    use reth_optimism_primitives::{OpBlock, OpPrimitives, OpTransactionSigned};
    use reth_primitives_traits::{RecoveredBlock, SealedBlock, SealedHeader};
    use reth_rpc_eth_types::PendingBlock;
    use std::{collections::HashMap, sync::Arc, time::Instant};

    fn make_executed_block(block_number: u64, parent_hash: B256) -> ExecutedBlock<OpPrimitives> {
        let header = Header { number: block_number, parent_hash, ..Default::default() };
        let sealed_header = SealedHeader::seal_slow(header);
        let block = OpBlock::new(sealed_header.unseal(), Default::default());
        let sealed_block = SealedBlock::seal_slow(block);
        let recovered_block = RecoveredBlock::new_sealed(sealed_block, vec![]);
        let execution_output = Arc::new(BlockExecutionOutput {
            result: BlockExecutionResult {
                receipts: vec![],
                requests: Default::default(),
                gas_used: 0,
                blob_gas_used: 0,
            },
            state: Default::default(),
        });
        ExecutedBlock::new(Arc::new(recovered_block), execution_output, ComputedTrieData::default())
    }

    fn make_pending_sequence(block_number: u64) -> PendingSequence<OpPrimitives> {
        let executed = make_executed_block(block_number, B256::ZERO);
        let block_hash = executed.recovered_block.hash();
        let parent_hash = executed.recovered_block.parent_hash();
        let pending_block = PendingBlock::with_executed_block(Instant::now(), executed);
        PendingSequence::new(
            pending_block,
            HashMap::new(),
            Default::default(),
            block_hash,
            parent_hash,
            0,
        )
    }

    fn mock_tx(nonce: u64) -> OpTransactionSigned {
        let tx = TxEip7702 {
            chain_id: 1u64,
            nonce,
            max_fee_per_gas: 0x28f000fff,
            max_priority_fee_per_gas: 0x28f000fff,
            gas_limit: 21_000,
            to: Address::default(),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
            authorization_list: Default::default(),
        };
        let signature = PrimitiveSignature::new(U256::default(), U256::default(), true);
        OpTransactionSigned::new_unhashed(OpTypedTransaction::Eip7702(tx), signature)
    }

    fn make_pending_sequence_with_txs(
        block_number: u64,
        tx_count: usize,
    ) -> PendingSequence<OpPrimitives> {
        use alloy_consensus::transaction::TxHashRef;

        let executed = make_executed_block(block_number, B256::ZERO);
        let block_hash = executed.recovered_block.hash();
        let parent_hash = executed.recovered_block.parent_hash();

        let mut tx_index = HashMap::new();
        for i in 0..tx_count {
            let tx = mock_tx(i as u64);
            let tx_hash = *tx.tx_hash();
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
        PendingSequence::new(
            pending_block,
            tx_index,
            Default::default(),
            block_hash,
            parent_hash,
            0,
        )
    }

    #[test]
    fn test_pending_sequence_get_hash_returns_stored_block_hash() {
        let seq = make_pending_sequence(42);
        assert_eq!(seq.get_hash(), seq.block_hash);
    }

    #[test]
    fn test_pending_sequence_get_height_returns_block_number() {
        let seq = make_pending_sequence(99);
        assert_eq!(seq.get_height(), 99);
    }

    #[test]
    fn test_pending_sequence_get_block_and_receipts_empty_receipts_on_no_tx_block() {
        let seq = make_pending_sequence(3);
        let bar = seq.get_block_and_receipts();
        assert!(bar.receipts.is_empty());
    }

    #[test]
    fn test_pending_sequence_get_tx_info_returns_none_for_unknown_hash() {
        let seq = make_pending_sequence_with_txs(10, 2);
        assert!(seq.get_tx_info(&B256::repeat_byte(0xFF)).is_none());
    }

    #[test]
    fn test_pending_sequence_get_tx_info_returns_correct_info_for_known_tx() {
        use alloy_consensus::transaction::TxHashRef;

        let seq = make_pending_sequence_with_txs(42, 3);
        let (tx_hash, expected_info) = seq.tx_index.iter().next().unwrap();
        let (info, bar) = seq.get_tx_info(tx_hash).expect("known tx hash should return Some");
        assert_eq!(info.block_number, 42);
        assert_eq!(info.block_hash, seq.block_hash);
        assert_eq!(info.tx_index, expected_info.tx_index);
        assert_eq!(*info.tx.tx_hash(), *tx_hash);
        assert_eq!(bar.block.number(), 42);
    }
}
