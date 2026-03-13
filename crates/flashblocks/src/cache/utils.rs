use reth_primitives_traits::{Block, BlockTy, NodePrimitives};
use reth_rpc_eth_types::block::BlockAndReceipts;

pub(crate) fn block_from_bar<N: NodePrimitives>(bar: &BlockAndReceipts<N>) -> BlockTy<N> {
    BlockTy::<N>::new(bar.block.header().clone(), bar.block.body().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{BlockHeader, Header};
    use alloy_primitives::B256;
    use reth_optimism_primitives::{OpBlock, OpPrimitives};
    use reth_primitives_traits::{RecoveredBlock, SealedBlock, SealedHeader};
    use reth_rpc_eth_types::block::BlockAndReceipts;
    use std::sync::Arc;

    /// Builds a minimal `BlockAndReceipts<OpPrimitives>` for testing.
    fn make_block_and_receipts(
        block_number: u64,
        parent_hash: B256,
    ) -> BlockAndReceipts<OpPrimitives> {
        let header = Header { number: block_number, parent_hash, ..Default::default() };
        let sealed_header = SealedHeader::seal_slow(header);
        let block = OpBlock::new(sealed_header.unseal(), Default::default());
        let sealed_block = SealedBlock::seal_slow(block);
        let recovered_block = RecoveredBlock::new_sealed(sealed_block, vec![]);
        BlockAndReceipts { block: Arc::new(recovered_block), receipts: Arc::new(vec![]) }
    }

    #[test]
    fn test_block_from_bar_returns_block_with_correct_number() {
        let bar = make_block_and_receipts(42, B256::ZERO);
        let block = block_from_bar::<OpPrimitives>(&bar);
        assert_eq!(block.header().number(), 42, "block_from_bar should preserve the block number");
    }

    #[test]
    fn test_block_from_bar_returns_block_with_correct_parent_hash() {
        let parent = B256::repeat_byte(0xBE);
        let bar = make_block_and_receipts(10, parent);
        let block = block_from_bar::<OpPrimitives>(&bar);
        assert_eq!(
            block.header().parent_hash(),
            parent,
            "block_from_bar should preserve the parent hash"
        );
    }
}
