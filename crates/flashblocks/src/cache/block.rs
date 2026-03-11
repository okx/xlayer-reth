use crate::cache::{block_from_bar, FlashblockStateCache, StateCacheProvider};

use alloy_eips::{BlockHashOrNumber, BlockId};
use alloy_primitives::{BlockNumber, TxNumber, B256};
use core::ops::RangeInclusive;
use reth_db_models::StoredBlockBodyIndices;
use reth_primitives_traits::{BlockTy, NodePrimitives, RecoveredBlock, SealedHeader};
use reth_storage_api::{
    errors::provider::ProviderResult, BlockBodyIndicesProvider, BlockReader, BlockReaderIdExt,
    BlockSource, HeaderProvider, TransactionVariant,
};

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> BlockReader
    for FlashblockStateCache<N, Provider>
{
    type Block = BlockTy<N>;

    fn find_block_by_hash(
        &self,
        hash: B256,
        source: BlockSource,
    ) -> ProviderResult<Option<Self::Block>> {
        if let Some(bar) = self.inner.read().confirm_cache.get_block_by_hash(&hash) {
            return Ok(Some(block_from_bar(&bar)));
        }
        self.provider.find_block_by_hash(hash, source)
    }

    fn block(&self, id: BlockHashOrNumber) -> ProviderResult<Option<Self::Block>> {
        let cached = match id {
            BlockHashOrNumber::Hash(hash) => {
                self.inner.read().confirm_cache.get_block_by_hash(&hash)
            }
            BlockHashOrNumber::Number(num) => {
                self.inner.read().confirm_cache.get_block_by_number(num)
            }
        };
        if let Some(bar) = cached {
            return Ok(Some(block_from_bar(&bar)));
        }
        self.provider.block(id)
    }

    fn pending_block(&self) -> ProviderResult<Option<RecoveredBlock<Self::Block>>> {
        {
            let inner = self.inner.read();
            if let Some(pending) = &inner.pending {
                return Ok(Some(pending.pending.block().as_ref().clone()));
            }
        }
        self.provider.pending_block()
    }

    fn pending_block_and_receipts(
        &self,
    ) -> ProviderResult<Option<(RecoveredBlock<Self::Block>, Vec<Self::Receipt>)>> {
        {
            let inner = self.inner.read();
            if let Some(pending) = &inner.pending {
                let block = pending.pending.block().as_ref().clone();
                let receipts = pending.pending.receipts.as_ref().clone();
                return Ok(Some((block, receipts)));
            }
        }
        self.provider.pending_block_and_receipts()
    }

    fn recovered_block(
        &self,
        id: BlockHashOrNumber,
        transaction_kind: TransactionVariant,
    ) -> ProviderResult<Option<RecoveredBlock<Self::Block>>> {
        let cached = match id {
            BlockHashOrNumber::Hash(hash) => {
                self.inner.read().confirm_cache.get_block_by_hash(&hash)
            }
            BlockHashOrNumber::Number(num) => {
                self.inner.read().confirm_cache.get_block_by_number(num)
            }
        };
        if let Some(bar) = cached {
            return Ok(Some((*bar.block).clone()));
        }
        self.provider.recovered_block(id, transaction_kind)
    }

    fn sealed_block_with_senders(
        &self,
        id: BlockHashOrNumber,
        transaction_kind: TransactionVariant,
    ) -> ProviderResult<Option<RecoveredBlock<Self::Block>>> {
        self.recovered_block(id, transaction_kind)
    }

    fn block_range(&self, range: RangeInclusive<BlockNumber>) -> ProviderResult<Vec<Self::Block>> {
        self.collect_cached_block_range(
            *range.start(),
            *range.end(),
            |bar| block_from_bar(bar),
            |r, _| self.provider.block_range(r),
            None,
        )
    }

    fn block_with_senders_range(
        &self,
        range: RangeInclusive<BlockNumber>,
    ) -> ProviderResult<Vec<RecoveredBlock<Self::Block>>> {
        self.collect_cached_block_range(
            *range.start(),
            *range.end(),
            |bar| (*bar.block).clone(),
            |r, _| self.provider.block_with_senders_range(r),
            None,
        )
    }

    fn recovered_block_range(
        &self,
        range: RangeInclusive<BlockNumber>,
    ) -> ProviderResult<Vec<RecoveredBlock<Self::Block>>> {
        self.collect_cached_block_range(
            *range.start(),
            *range.end(),
            |bar| (*bar.block).clone(),
            |r, _| self.provider.recovered_block_range(r),
            None,
        )
    }

    fn block_by_transaction_id(&self, id: TxNumber) -> ProviderResult<Option<BlockNumber>> {
        self.provider.block_by_transaction_id(id)
    }
}

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> BlockReaderIdExt
    for FlashblockStateCache<N, Provider>
{
    fn block_by_id(&self, id: BlockId) -> ProviderResult<Option<Self::Block>> {
        match id {
            BlockId::Hash(hash) => self.block_by_hash(hash.into()),
            BlockId::Number(num) => self.block_by_number_or_tag(num),
        }
    }

    fn sealed_header_by_id(
        &self,
        id: BlockId,
    ) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
        match id {
            BlockId::Hash(hash) => self.sealed_header_by_hash(hash.into()),
            BlockId::Number(tag) => self.sealed_header_by_number_or_tag(tag),
        }
    }

    fn header_by_id(&self, id: BlockId) -> ProviderResult<Option<Self::Header>> {
        match id {
            BlockId::Hash(hash) => self.header(hash.into()),
            BlockId::Number(num) => self.header_by_number_or_tag(num),
        }
    }
}

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> BlockBodyIndicesProvider
    for FlashblockStateCache<N, Provider>
{
    fn block_body_indices(&self, num: u64) -> ProviderResult<Option<StoredBlockBodyIndices>> {
        self.provider.block_body_indices(num)
    }

    fn block_body_indices_range(
        &self,
        range: RangeInclusive<BlockNumber>,
    ) -> ProviderResult<Vec<StoredBlockBodyIndices>> {
        self.provider.block_body_indices_range(range)
    }
}
