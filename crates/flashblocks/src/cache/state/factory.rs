use super::{utils::StateCacheProvider, StateCache};

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{BlockNumber, B256};
use reth_primitives_traits::NodePrimitives;
use reth_storage_api::{errors::provider::ProviderResult, StateProviderBox, StateProviderFactory};

impl<N: NodePrimitives, Provider: StateCacheProvider<N>> StateProviderFactory
    for StateCache<N, Provider>
{
    fn latest(&self) -> ProviderResult<StateProviderBox> {
        // Determine effective latest: if confirm cache is strictly ahead of the
        // provider's best block, use the confirmed block's hash to resolve state
        // from the engine tree. Otherwise, use the provider's latest.
        let provider_best = self.provider.best_block_number()?;
        let inner = self.inner.read();
        if let Some(confirm_height) = inner.confirm_height {
            if confirm_height > provider_best {
                if let Some(hash) = inner.confirm_cache.hash_for_number(confirm_height) {
                    drop(inner);
                    return self.provider.state_by_block_hash(hash);
                }
            }
        }
        drop(inner);
        self.provider.latest()
    }

    fn state_by_block_number_or_tag(
        &self,
        number_or_tag: BlockNumberOrTag,
    ) -> ProviderResult<StateProviderBox> {
        match number_or_tag {
            BlockNumberOrTag::Latest => self.latest(),
            BlockNumberOrTag::Pending => self.pending(),
            other => self.provider.state_by_block_number_or_tag(other),
        }
    }

    fn history_by_block_number(&self, block: BlockNumber) -> ProviderResult<StateProviderBox> {
        // If the requested block is in the confirm cache (ahead of canonical),
        // resolve via hash so the engine tree can serve it.
        if let Some(hash) = self.inner.read().confirm_cache.hash_for_number(block) {
            return self.provider.state_by_block_hash(hash);
        }
        self.provider.history_by_block_number(block)
    }

    fn history_by_block_hash(&self, block: B256) -> ProviderResult<StateProviderBox> {
        // If the hash is in our confirm cache, route through `state_by_block_hash`
        // which also covers the engine tree's in-memory state.
        if self.inner.read().confirm_cache.contains_hash(&block) {
            return self.provider.state_by_block_hash(block);
        }
        self.provider.history_by_block_hash(block)
    }

    fn state_by_block_hash(&self, block: B256) -> ProviderResult<StateProviderBox> {
        self.provider.state_by_block_hash(block)
    }

    fn pending(&self) -> ProviderResult<StateProviderBox> {
        // Delegate to the underlying provider. The engine tree should have the
        // pending block's world state if it has been submitted via engine API.
        // Building a custom state overlay from the `PendingSequence`'s
        // `ExecutedBlock` is a future enhancement.
        self.provider.pending()
    }

    fn pending_state_by_hash(&self, block_hash: B256) -> ProviderResult<Option<StateProviderBox>> {
        self.provider.pending_state_by_hash(block_hash)
    }

    fn maybe_pending(&self) -> ProviderResult<Option<StateProviderBox>> {
        self.provider.maybe_pending()
    }
}
