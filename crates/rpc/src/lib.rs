#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod default;
pub mod flashblocks;
pub mod helper;

pub use default::{DefaultRpcExt, DefaultRpcExtApiServer, SequencerClientProvider};
pub use flashblocks::{FlashblocksEthApiExt, FlashblocksEthApiOverrideServer};

// Implement `SequencerClientProvider` for `OpEthApi`
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
