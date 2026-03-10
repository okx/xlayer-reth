use reth_primitives_traits::{Block, BlockTy, HeaderTy, NodePrimitives, ReceiptTy};
use reth_rpc_eth_types::block::BlockAndReceipts;
use reth_storage_api::{BlockReaderIdExt, StateProviderFactory};

/// Provider trait bound alias used throughout the `StateCache` implementation.
///
/// The provider must implement the full reth block reader + state provider stack.
pub(crate) trait StateCacheProvider<N: NodePrimitives>:
    StateProviderFactory
    + BlockReaderIdExt<
        Header = HeaderTy<N>,
        Block = BlockTy<N>,
        Transaction = N::SignedTx,
        Receipt = ReceiptTy<N>,
    > + Unpin
{
}

impl<N, P> StateCacheProvider<N> for P
where
    N: NodePrimitives,
    P: StateProviderFactory
        + BlockReaderIdExt<
            Header = HeaderTy<N>,
            Block = BlockTy<N>,
            Transaction = N::SignedTx,
            Receipt = ReceiptTy<N>,
        > + Unpin,
{
}

pub(crate) fn block_from_bar<N: NodePrimitives>(bar: &BlockAndReceipts<N>) -> BlockTy<N> {
    BlockTy::<N>::new(bar.block.header().clone(), bar.block.body().clone())
}
