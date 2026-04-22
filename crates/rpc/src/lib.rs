#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod aa;
pub mod default;
pub mod eth;
pub mod filter;
pub mod helper;

pub use aa::{
    read_2d_nonce,
    receipt::{build_eip8130_fields, build_fields_for_aa},
    TransactionCountOverrideImpl, TransactionCountOverrideServer,
};
pub use default::{DefaultRpcExt, DefaultRpcExtApiServer, SequencerClientProvider};
pub use eth::{FlashblocksEthApiExt, FlashblocksEthApiOverrideServer};
pub use filter::{FlashblocksEthFilterExt, FlashblocksFilterOverrideServer};

use reth_optimism_rpc::{OpEthApi, SequencerClient};
use reth_rpc_eth_api::{RpcConvert, RpcNodeCore};

// Implement `SequencerClientProvider` for `OpEthApi`
impl<N, Rpc> SequencerClientProvider for OpEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
{
    fn sequencer_client(&self) -> Option<&SequencerClient> {
        self.sequencer_client()
    }
}
