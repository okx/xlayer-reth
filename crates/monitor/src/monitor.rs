use crate::args::FullLinkMonitorArgs;

use std::sync::Arc;
use tracing::debug;

use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use xlayer_trace_monitor::{from_b256, get_global_tracer, TransactionProcessId};

/// XLayerMonitor holds monitoring hook logic for full link monitoring requirements.
#[derive(Clone, Default)]
pub struct XLayerMonitor {
    /// XLayer arguments (reserved for future use)
    #[allow(dead_code)]
    pub args: FullLinkMonitorArgs,
    /// Flashblocks enabled flag
    pub flashblocks_enabled: bool,
    /// Whether this node is running in sequencer mode (true) or RPC mode (false)
    pub is_sequencer_mode: bool,
}

impl XLayerMonitor {
    pub fn new(
        args: FullLinkMonitorArgs,
        flashblocks_enabled: bool,
        is_sequencer_mode: bool,
    ) -> Arc<Self> {
        Arc::new(Self { args, flashblocks_enabled, is_sequencer_mode })
    }

    /// Check if this node is running in sequencer mode
    pub fn is_sequencer(&self) -> bool {
        self.is_sequencer_mode
    }

    /// Handle transaction received via RPC (eth_sendRawTransaction).
    pub fn on_recv_transaction(&self, _method: &str, tx_hash: B256) {
        if let Some(tracer) = get_global_tracer() {
            if self.is_sequencer() {
                // SeqReceiveTxEnd: eth_sendRawTransaction (seq handler)
                tracer.log_transaction(
                    from_b256(tx_hash),
                    TransactionProcessId::SeqReceiveTxEnd,
                    None,
                );
            } else {
                // RpcReceiveTxEnd: eth_sendRawTransaction (RPC handler)
                tracer.log_transaction(
                    from_b256(tx_hash),
                    TransactionProcessId::RpcReceiveTxEnd,
                    None,
                );
            }
        }
    }

    /// Handle block build start event (when payload attributes are received from CL).
    /// This is triggered when the consensus layer sends payload attributes via engine_forkchoiceUpdatedV*.
    pub fn on_block_build_start(&self, block_number: u64) {
        if let Some(tracer) = get_global_tracer() {
            if self.is_sequencer() {
                // Use block_number as the hash for block-level events
                // Note: We don't have the block hash here, so we use a zero hash
                // The block_number is the key identifier
                let block_hash = B256::ZERO; // Will be updated when block is built
                tracer.log_block(
                    from_b256(block_hash),
                    block_number,
                    TransactionProcessId::SeqBlockBuildStart,
                );
            }
        }
    }

    /// Handle block send start event (when payload is built and ready to send).
    /// This is triggered when CL calls getPayload and the block is built.
    pub fn on_block_send_start(&self, num_hash: BlockNumHash) {
        if let Some(tracer) = get_global_tracer() {
            if self.is_sequencer() {
                tracer.log_block(
                    from_b256(num_hash.hash),
                    num_hash.number,
                    TransactionProcessId::SeqBlockSendStart,
                );
            }
        }
    }

    /// Handle block received event (when newPayload is called).
    /// This is triggered by ConsensusEngineEvent::BlockReceived.
    pub fn on_block_received(&self, num_hash: BlockNumHash) {
        if let Some(tracer) = get_global_tracer() {
            if !self.is_sequencer() {
                tracer.log_block(
                    from_b256(num_hash.hash),
                    num_hash.number,
                    TransactionProcessId::RpcBlockReceiveEnd,
                );
            }

        }
    }

    /// Handle transaction commits to the canonical chain.
    pub fn on_tx_commit(&self, _num_hash: BlockNumHash, tx_hash: B256) {
        if !self.flashblocks_enabled {
            if let Some(tracer) = get_global_tracer() {
                if self.is_sequencer() {
                    tracer.log_transaction(
                        from_b256(tx_hash),
                        TransactionProcessId::SeqTxExecutionEnd,
                        Some(_num_hash.number),
                    );
                }
            }
        }
    }

    /// Handle block commits to the canonical chain.
    pub fn on_block_commit(&self, num_hash: BlockNumHash) {
        if let Some(tracer) = get_global_tracer() {
            if self.is_sequencer() {
                // SeqBlockBuildEnd: canon stream update (seq)
                tracer.log_block(
                    from_b256(num_hash.hash),
                    num_hash.number,
                    TransactionProcessId::SeqBlockBuildEnd,
                );
            } else {
                // RpcBlockInsertEnd: canon stream update (RPC)
                tracer.log_block(
                    from_b256(num_hash.hash),
                    num_hash.number,
                    TransactionProcessId::RpcBlockInsertEnd,
                );
            }
        }
    }
}
