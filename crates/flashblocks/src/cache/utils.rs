use reth_primitives_traits::{Block, BlockTy, NodePrimitives};
use reth_rpc_eth_types::block::BlockAndReceipts;

pub(crate) fn block_from_bar<N: NodePrimitives>(bar: &BlockAndReceipts<N>) -> BlockTy<N> {
    BlockTy::<N>::new(bar.block.header().clone(), bar.block.body().clone())
}
