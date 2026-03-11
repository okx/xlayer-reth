use crate::cache::{FlashblockStateCache, StateCacheProvider};

use alloy_primitives::{BlockNumber, B256};
use core::ops::RangeBounds;
use reth_primitives_traits::{HeaderTy, NodePrimitives, SealedHeader};
use reth_storage_api::{errors::provider::ProviderResult, HeaderProvider};

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> HeaderProvider
    for FlashblockStateCache<N, Provider>
{
    type Header = HeaderTy<N>;

    fn header(&self, block_hash: B256) -> ProviderResult<Option<Self::Header>> {
        if let Some(bar) = self.inner.read().confirm_cache.get_block_by_hash(&block_hash) {
            return Ok(Some(bar.block.header().clone()));
        }
        // Cache miss, delegate to the provider
        self.provider.header(block_hash)
    }

    fn header_by_number(&self, num: u64) -> ProviderResult<Option<Self::Header>> {
        if let Some(bar) = self.inner.read().confirm_cache.get_block_by_number(num) {
            return Ok(Some(bar.block.header().clone()));
        }
        // Cache miss, delegate to the provider
        self.provider.header_by_number(num)
    }

    fn headers_range(
        &self,
        range: impl RangeBounds<BlockNumber>,
    ) -> ProviderResult<Vec<Self::Header>> {
        let (start, end) = self.resolve_range_bounds(range)?;
        if start > end {
            return Ok(Vec::new());
        }
        self.collect_cached_block_range(
            start,
            end,
            |bar| bar.block.header().clone(),
            |r| self.provider.headers_range(r),
        )
    }

    fn sealed_header(
        &self,
        number: BlockNumber,
    ) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
        if let Some(bar) = self.inner.read().confirm_cache.get_block_by_number(number) {
            return Ok(Some(bar.block.sealed_header().clone()));
        }
        // Cache miss, delegate to the provider
        self.provider.sealed_header(number)
    }

    fn sealed_headers_while(
        &self,
        range: impl RangeBounds<BlockNumber>,
        predicate: impl FnMut(&SealedHeader<Self::Header>) -> bool,
    ) -> ProviderResult<Vec<SealedHeader<Self::Header>>> {
        self.provider.sealed_headers_while(range, predicate)
    }
}
