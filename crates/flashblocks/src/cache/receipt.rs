use crate::cache::{FlashblockStateCache, StateCacheProvider};

use alloy_eips::BlockHashOrNumber;
use alloy_primitives::{BlockNumber, TxHash, TxNumber};
use core::ops::{RangeBounds, RangeInclusive};
use reth_primitives_traits::{NodePrimitives, ReceiptTy};
use reth_storage_api::{errors::provider::ProviderResult, ReceiptProvider, ReceiptProviderIdExt};

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> ReceiptProvider
    for FlashblockStateCache<N, Provider>
{
    type Receipt = ReceiptTy<N>;

    fn receipt(&self, id: TxNumber) -> ProviderResult<Option<Self::Receipt>> {
        self.provider.receipt(id)
    }

    fn receipt_by_hash(&self, hash: TxHash) -> ProviderResult<Option<Self::Receipt>> {
        if let Some(info) = self.inner.read().confirm_cache.get_tx_info(&hash) {
            return Ok(Some(info.receipt.clone()));
        }
        self.provider.receipt_by_hash(hash)
    }

    fn receipts_by_block(
        &self,
        block: BlockHashOrNumber,
    ) -> ProviderResult<Option<Vec<Self::Receipt>>> {
        let cached = match block {
            BlockHashOrNumber::Hash(hash) => {
                self.inner.read().confirm_cache.get_block_by_hash(&hash)
            }
            BlockHashOrNumber::Number(num) => {
                self.inner.read().confirm_cache.get_block_by_number(num)
            }
        };
        if let Some(bar) = cached {
            return Ok(Some((*bar.receipts).clone()));
        }
        self.provider.receipts_by_block(block)
    }

    fn receipts_by_tx_range(
        &self,
        range: impl RangeBounds<TxNumber>,
    ) -> ProviderResult<Vec<Self::Receipt>> {
        self.provider.receipts_by_tx_range(range)
    }

    fn receipts_by_block_range(
        &self,
        block_range: RangeInclusive<BlockNumber>,
    ) -> ProviderResult<Vec<Vec<Self::Receipt>>> {
        self.collect_cached_block_range(
            *block_range.start(),
            *block_range.end(),
            |bar| (*bar.receipts).clone(),
            |r| self.provider.receipts_by_block_range(r),
        )
    }
}

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> ReceiptProviderIdExt
    for FlashblockStateCache<N, Provider>
{
}
