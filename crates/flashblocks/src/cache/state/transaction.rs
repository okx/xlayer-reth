use super::{utils::StateCacheProvider, StateCache};

use alloy_eips::BlockHashOrNumber;
use alloy_primitives::{Address, BlockNumber, TxHash, TxNumber};
use core::ops::RangeBounds;
use reth_primitives_traits::{BlockBody, NodePrimitives, TransactionMeta};
use reth_storage_api::{errors::provider::ProviderResult, TransactionsProvider};

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> TransactionsProvider
    for StateCache<N, Provider>
{
    type Transaction = N::SignedTx;

    fn transaction_id(&self, tx_hash: TxHash) -> ProviderResult<Option<TxNumber>> {
        self.provider.transaction_id(tx_hash)
    }

    fn transaction_by_id(&self, id: TxNumber) -> ProviderResult<Option<Self::Transaction>> {
        self.provider.transaction_by_id(id)
    }

    fn transaction_by_id_unhashed(
        &self,
        id: TxNumber,
    ) -> ProviderResult<Option<Self::Transaction>> {
        self.provider.transaction_by_id_unhashed(id)
    }

    fn transaction_by_hash(&self, hash: TxHash) -> ProviderResult<Option<Self::Transaction>> {
        self.provider.transaction_by_hash(hash)
    }

    fn transaction_by_hash_with_meta(
        &self,
        hash: TxHash,
    ) -> ProviderResult<Option<(Self::Transaction, TransactionMeta)>> {
        self.provider.transaction_by_hash_with_meta(hash)
    }

    fn transactions_by_block(
        &self,
        block: BlockHashOrNumber,
    ) -> ProviderResult<Option<Vec<Self::Transaction>>> {
        let cached = match block {
            BlockHashOrNumber::Hash(hash) => {
                self.inner.read().confirm_cache.get_block_by_hash(&hash)
            }
            BlockHashOrNumber::Number(num) => {
                self.inner.read().confirm_cache.get_block_by_number(num)
            }
        };
        if let Some(bar) = cached {
            return Ok(Some(bar.block.body().transactions().to_vec()));
        }
        self.provider.transactions_by_block(block)
    }

    fn transactions_by_block_range(
        &self,
        range: impl RangeBounds<BlockNumber>,
    ) -> ProviderResult<Vec<Vec<Self::Transaction>>> {
        self.provider.transactions_by_block_range(range)
    }

    fn transactions_by_tx_range(
        &self,
        range: impl RangeBounds<TxNumber>,
    ) -> ProviderResult<Vec<Self::Transaction>> {
        self.provider.transactions_by_tx_range(range)
    }

    fn senders_by_tx_range(
        &self,
        range: impl RangeBounds<TxNumber>,
    ) -> ProviderResult<Vec<Address>> {
        self.provider.senders_by_tx_range(range)
    }

    fn transaction_sender(&self, id: TxNumber) -> ProviderResult<Option<Address>> {
        self.provider.transaction_sender(id)
    }
}
