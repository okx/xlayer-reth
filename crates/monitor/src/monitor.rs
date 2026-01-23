use crate::args::FullLinkMonitorArgs;

use std::sync::Arc;
use tracing::debug;

use alloy_eips::BlockNumHash;
use alloy_primitives::B256;

/// XLayerMonitor holds monitoring hook logic for full link monitoring requirements.
#[derive(Clone, Default)]
pub struct XLayerMonitor {
    /// XLayer arguments (reserved for future use)
    #[allow(dead_code)]
    pub args: FullLinkMonitorArgs,
    /// Flashblocks enabled flag
    pub flashblocks_enabled: bool,
}

impl XLayerMonitor {
    pub fn new(args: FullLinkMonitorArgs, flashblocks_enabled: bool) -> Arc<Self> {
        Arc::new(Self { args, flashblocks_enabled })
    }

    /// Handle transaction received via RPC (eth_sendRawTransaction).
    pub fn on_recv_transaction(&self, _method: &str, tx_hash: B256) {
        debug!(target: "xlayer::monitor", tx_hash = %tx_hash, "transaction received via RPC");
        // TODO: add RpcReceiveTxEnd, SeqReceiveTxEnd here, use xlayer_args if you want
    }

    /// Handle block build start event (when payload attributes are received from CL).
    /// This is triggered when the consensus layer sends payload attributes via engine_forkchoiceUpdatedV*.
    pub fn on_block_build_start(&self, block_number: u64) {
        debug!(target: "xlayer::monitor", block_number = block_number, "block building started");
        // TODO: add SeqBlockBuildStart here based on xlayer_args
    }

    /// Handle block send start event (when payload is built and ready to send).
    /// This is triggered when CL calls getPayload and the block is built.
    pub fn on_block_send_start(&self, num_hash: BlockNumHash) {
        debug!(target: "xlayer::monitor", block_number = num_hash.number, block_hash = %num_hash.hash, "block sending started");
        // TODO: add SeqBlockSendStart here based on xlayer_args
    }

    /// Handle block received event (when newPayload is called).
    /// This is triggered by ConsensusEngineEvent::BlockReceived.
    pub fn on_block_received(&self, num_hash: BlockNumHash) {
        debug!(
            target: "xlayer::monitor",
            block_number = num_hash.number,
            block_hash = %num_hash.hash,
            "block received from consensus engine"
        );
        // TODO: add RpcBlockReceiveEnd here based on xlayer_args
    }

    /// Handle transaction commits to the canonical chain.
    pub fn on_tx_commit(&self, _num_hash: BlockNumHash, tx_hash: B256) {
        if !self.flashblocks_enabled {
            debug!(target: "xlayer::monitor", tx_hash = %tx_hash, "transaction committed to canonical chain");
            // TODO: add SeqTxExecutionEnd here if flashblocks is disabled, you can use xlayer_args if you want
        }
    }

    /// Handle block commits to the canonical chain.
    pub fn on_block_commit(&self, num_hash: BlockNumHash) {
        debug!(target: "xlayer::monitor", block_number = num_hash.number, block_hash = %num_hash.hash, "block committed to canonical chain");
        // TODO: add SeqBlockBuildEnd, RpcBlockInsertEnd here
    }
}
