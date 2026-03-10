use super::{utils::StateCacheProvider, StateCache};

use alloy_consensus::BlockHeader;
use alloy_eips::BlockNumHash;
use alloy_primitives::{BlockNumber, B256};
use reth_chainspec::ChainInfo;
use reth_primitives_traits::NodePrimitives;
use reth_storage_api::{
    errors::provider::ProviderResult, BlockHashReader, BlockIdReader, BlockNumReader,
};

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> BlockHashReader
    for StateCache<N, Provider>
{
    fn block_hash(&self, number: BlockNumber) -> ProviderResult<Option<B256>> {
        if let Some(hash) = self.inner.read().confirm_cache.hash_for_number(number) {
            return Ok(Some(hash));
        }
        // Cache miss, delegate to the provider
        self.provider.block_hash(number)
    }

    fn canonical_hashes_range(
        &self,
        start: BlockNumber,
        end: BlockNumber,
    ) -> ProviderResult<Vec<B256>> {
        if start >= end {
            // Aligns with underlying blockchain provider
            return Ok(Vec::new());
        }

        let inner = self.inner.read();
        let mut cache_hashes = Vec::new();
        let mut provider_end = end;
        let mut index = end - 1;
        loop {
            if let Some(hash) = inner.confirm_cache.hash_for_number(index) {
                cache_hashes.push(hash);
                provider_end = index;
            } else {
                break;
            }
            // Guard against underflow when index == 0, and stop once we've
            // covered the full range down to `start`
            if index == start {
                break;
            }
            index -= 1;
        }
        cache_hashes.reverse();
        drop(inner);

        // Delegate the remaining prefix [start, provider_end) to the provider
        let mut result = if provider_end > start {
            self.provider.canonical_hashes_range(start, provider_end)?
        } else {
            Vec::new()
        };
        result.extend(cache_hashes);
        Ok(result)
    }
}

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> BlockNumReader
    for StateCache<N, Provider>
{
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        let mut info = self.provider.chain_info()?;
        let inner = self.inner.read();
        if let Some(h) = inner.confirm_height
            && h > info.best_number
            && let Some(hash) = inner.confirm_cache.hash_for_number(h)
        {
            info.best_number = h;
            info.best_hash = hash;
        }
        Ok(info)
    }

    fn best_block_number(&self) -> ProviderResult<BlockNumber> {
        let provider_height = self.provider.best_block_number()?;
        // If confirm cache is strictly ahead, report that. On tie, prefer provider
        Ok(self.inner.read().confirm_height.map_or(provider_height, |h| h.max(provider_height)))
    }

    fn last_block_number(&self) -> ProviderResult<BlockNumber> {
        self.provider.last_block_number()
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<BlockNumber>> {
        if let Some(num) = self.inner.read().confirm_cache.number_for_hash(&hash) {
            return Ok(Some(num));
        }
        // Cache miss, delegate to the provider
        self.provider.block_number(hash)
    }
}

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> BlockIdReader for StateCache<N, Provider> {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        let inner = self.inner.read();
        if let Some(pending) = &inner.pending {
            let block = pending.pending.block();
            return Ok(Some(BlockNumHash::new(block.number(), block.hash())));
        }
        drop(inner);
        // Cache miss, delegate to the provider
        self.provider.pending_block_num_hash()
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        self.provider.safe_block_num_hash()
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        self.provider.finalized_block_num_hash()
    }
}
