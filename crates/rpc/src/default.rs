use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
};

use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::SequencerClient;

use xlayer_flashblocks::FlashblockStateCache;

/// Trait for accessing sequencer client from backend
pub trait SequencerClientProvider {
    /// Returns the sequencer client if available
    fn sequencer_client(&self) -> Option<&SequencerClient>;
}

/// X Layer default Eth JSON-RPC API extension trait.
#[rpc(server, namespace = "eth")]
pub trait DefaultRpcExtApi {
    /// Returns boolean indicating if the node's flashblocks RPC functionality is enabled,
    /// and if the flashblocks state cache is initialized.
    ///
    /// Returns `true` if the flashblocks state cache is not `None`, and when the flashblocks
    /// state cache has been initialized (i.e. confirm height > 0), meaning the node is actively
    /// receiving and caching flashblock data.
    #[method(name = "flashblocksEnabled")]
    async fn flashblocks_enabled(&self) -> RpcResult<bool>;
}

/// X Layer default Eth JSON-RPC API extension implementation.
#[derive(Debug, Clone)]
pub struct DefaultRpcExt {
    flashblocks_state: Option<FlashblockStateCache<OpPrimitives>>,
}

impl DefaultRpcExt {
    /// Creates a new [`DefaultRpcExt`].
    pub fn new(flashblocks_state: Option<FlashblockStateCache<OpPrimitives>>) -> Self {
        Self { flashblocks_state }
    }
}

#[async_trait]
impl DefaultRpcExtApiServer for DefaultRpcExt {
    /// Handler for: `eth_flashblocksEnabled`
    async fn flashblocks_enabled(&self) -> RpcResult<bool> {
        Ok(self.flashblocks_state.as_ref().is_some_and(|cache| cache.get_confirm_height() > 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_trie_db::ChangesetCache;

    #[test]
    fn test_flashblocks_disabled_when_no_cache() {
        let ext = DefaultRpcExt::new(None);
        assert!(ext.flashblocks_state.is_none());
    }

    #[test]
    fn test_flashblocks_disabled_at_zero_height() {
        let cache = FlashblockStateCache::<OpPrimitives>::new(
            reth_chain_state::CanonicalInMemoryState::new(
                Default::default(),
                Default::default(),
                None,
                None,
                None,
            ),
            ChangesetCache::new(),
        );
        let ext = DefaultRpcExt::new(Some(cache));
        assert!(ext.flashblocks_state.as_ref().unwrap().get_confirm_height() == 0);
    }
}
