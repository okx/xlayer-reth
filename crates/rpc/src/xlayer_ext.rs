use std::sync::Arc;

use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
};

use reth_chainspec::{ChainSpecProvider, EthChainSpec};
use reth_optimism_rpc::SequencerClient;
use reth_rpc::RpcTypes;
use reth_rpc_eth_api::{
    helpers::{EthFees, LoadBlock, LoadFee},
    EthApiTypes,
};
use reth_storage_api::{BlockReaderIdExt, HeaderProvider, ProviderHeader};

/// Trait for accessing sequencer client from backend
pub trait SequencerClientProvider {
    /// Returns the sequencer client if available
    fn sequencer_client(&self) -> Option<&SequencerClient>;
}

/// Trait for checking if pending block (flashblocks) is enabled
pub trait PendingFlashBlockProvider {
    /// Returns true if pending block receiver is available and has actual pending block data (flashblocks enabled)
    fn has_pending_flashblock(&self) -> bool;
}

/// XLayer-specific RPC API trait
#[rpc(server, namespace = "eth", server_bounds(
    Net: 'static + RpcTypes,
    <Net as RpcTypes>::TransactionRequest:
        serde::de::DeserializeOwned + serde::Serialize
))]
pub trait XlayerRpcExtApi<Net: RpcTypes> {
    /// Returns boolean indicating if the node's flashblocks functionality is enabled and working.
    #[method(name = "flashblocksEnabled")]
    async fn flashblocks_enabled(&self) -> RpcResult<bool>;
}

/// XLayer RPC extension implementation
#[derive(Debug)]
pub struct XlayerRpcExt<T> {
    pub backend: Arc<T>,
}

#[async_trait]
impl<T, Net> XlayerRpcExtApiServer<Net> for XlayerRpcExt<T>
where
    T: EthFees
        + LoadFee
        + LoadBlock
        + EthApiTypes<NetworkTypes = Net>
        + SequencerClientProvider
        + PendingFlashBlockProvider
        + Clone
        + Send
        + Sync
        + 'static,
    T::Provider: ChainSpecProvider<ChainSpec: EthChainSpec<Header = ProviderHeader<T::Provider>>>
        + BlockReaderIdExt
        + HeaderProvider,
    Net: RpcTypes + Send + Sync + 'static,
{
    async fn flashblocks_enabled(&self) -> RpcResult<bool> {
        Ok(self.backend.has_pending_flashblock())
    }
}

#[cfg(test)]
mod tests {
    use super::PendingFlashBlockProvider;
    use std::time::{Duration, Instant};
    use tokio::sync::watch;

    struct MockPendingFlashBlock {
        expires_at: Instant,
    }

    struct MockPendingFlashBlockProvider {
        rx: Option<watch::Receiver<Option<MockPendingFlashBlock>>>,
    }

    impl PendingFlashBlockProvider for MockPendingFlashBlockProvider {
        fn has_pending_flashblock(&self) -> bool {
            self.rx.as_ref().is_some_and(|rx| {
                rx.borrow().as_ref().is_some_and(|pending_flashblock| {
                    Instant::now() < pending_flashblock.expires_at
                })
            })
        }
    }

    #[test]
    fn test_no_receiver_returns_false() {
        let provider = MockPendingFlashBlockProvider { rx: None };
        assert!(!provider.has_pending_flashblock());
    }

    #[test]
    fn test_empty_receiver_returns_false() {
        let (_tx, rx) = watch::channel(None);
        let provider = MockPendingFlashBlockProvider { rx: Some(rx) };
        assert!(!provider.has_pending_flashblock());
    }

    #[test]
    fn test_expired_flashblock_returns_false() {
        let expired =
            MockPendingFlashBlock { expires_at: Instant::now() - Duration::from_secs(60) };
        let (_tx, rx) = watch::channel(Some(expired));
        let provider = MockPendingFlashBlockProvider { rx: Some(rx) };
        assert!(!provider.has_pending_flashblock());
    }

    #[test]
    fn test_valid_flashblock_returns_true() {
        let valid = MockPendingFlashBlock { expires_at: Instant::now() + Duration::from_secs(60) };
        let (_tx, rx) = watch::channel(Some(valid));
        let provider = MockPendingFlashBlockProvider { rx: Some(rx) };
        assert!(provider.has_pending_flashblock());
    }
}
