#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod xlayer_ext;

use std::time::Instant;
// Re-export for convenience
pub use xlayer_ext::{
    PendingFlashBlockProvider, SequencerClientProvider, XlayerRpcExt, XlayerRpcExtApiServer,
};

// Implement SequencerClientProvider for OpEthApi
use reth_optimism_rpc::{OpEthApi, SequencerClient};
use reth_rpc_eth_api::{RpcConvert, RpcNodeCore};

impl<N, Rpc> SequencerClientProvider for OpEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
{
    fn sequencer_client(&self) -> Option<&SequencerClient> {
        self.sequencer_client()
    }
}

impl<N, Rpc> PendingFlashBlockProvider for OpEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
{
    fn has_pending_flashblock(&self) -> bool {
        self.pending_block_rx().is_some_and(|rx| {
            rx.borrow()
                .as_ref()
                .is_some_and(|pending_flashblock| Instant::now() < pending_flashblock.expires_at)
        })
    }
}
