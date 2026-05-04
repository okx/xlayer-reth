/// RPC service name
const RPC_SERVICE_NAME: &str = "okx-defi-xlayer-rpcpay-pro";

/// Sequencer service name
const SEQ_SERVICE_NAME: &str = "okx-defi-xlayer-egseqz-pro";

/// Transaction process ID for tracking different stages in the transaction lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransactionProcessId {
    /// RPC node: Transaction received and ready to forward
    RpcReceiveTxEnd = 15010,

    /// Sequencer node: Transaction received and added to pool
    SeqReceiveTxEnd = 15030,

    /// Sequencer node: Block building started
    SeqBlockBuildStart = 15032,

    /// Sequencer node: Transaction execution completed
    SeqTxExecutionEnd = 15034,

    /// Sequencer node: Block building completed
    SeqBlockBuildEnd = 15036,

    /// Sequencer node: Block sending started
    SeqBlockSendStart = 15042,

    /// RPC node: Block received from sequencer
    RpcBlockReceiveEnd = 15060,

    /// RPC node: Block insertion completed
    RpcBlockInsertEnd = 15062,
}

impl TransactionProcessId {
    /// Returns the string representation of the process ID.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::RpcReceiveTxEnd => "xlayer_rpc_receive_tx",
            Self::SeqReceiveTxEnd => "xlayer_seq_receive_tx",
            Self::SeqBlockBuildStart => "xlayer_seq_begin_block",
            Self::SeqTxExecutionEnd => "xlayer_seq_package_tx",
            Self::SeqBlockBuildEnd => "xlayer_seq_end_block",
            Self::SeqBlockSendStart => "xlayer_seq_ds_sent",
            Self::RpcBlockReceiveEnd => "xlayer_rpc_receive_block",
            Self::RpcBlockInsertEnd => "xlayer_rpc_finish_block",
        }
    }

    /// Returns the numeric ID of the process.
    pub const fn as_u64(&self) -> u64 {
        *self as u64
    }

    /// Returns the service name based on the process ID.
    pub const fn service_name(&self) -> &'static str {
        match self {
            // RPC-related process IDs
            Self::RpcReceiveTxEnd | Self::RpcBlockReceiveEnd | Self::RpcBlockInsertEnd => {
                RPC_SERVICE_NAME
            }

            // Sequencer-related process IDs
            Self::SeqReceiveTxEnd
            | Self::SeqBlockBuildStart
            | Self::SeqTxExecutionEnd
            | Self::SeqBlockBuildEnd
            | Self::SeqBlockSendStart => SEQ_SERVICE_NAME,
        }
    }
}
