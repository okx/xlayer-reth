//! Tracer for full trace support

use alloy_primitives::B256;
use alloy_rpc_types_engine::ForkchoiceState;
use op_alloy_rpc_types_engine::OpExecutionData;
use reth_node_api::EngineTypes;
use std::sync::Arc;

/// Block information for tracing.
#[derive(Debug, Clone)]
pub struct BlockInfo {
    /// The block number
    pub block_number: u64,
    /// The block hash
    pub block_hash: B256,
}

/// Tracer that holds tracing state and configuration.
///
/// This is the main entry point for the full trace functionality.
/// It manages the tracing configuration and implements event handlers.
///
/// This struct is intentionally simple with only the `Args` generic parameter,
/// making it easy to share across different tracer components without
/// carrying unnecessary type complexity.
pub struct Tracer<Args>
where
    Args: Clone + Send + Sync + 'static,
{
    /// XLayer arguments (reserved for future use)
    #[allow(dead_code)]
    xlayer_args: Args,
}

impl<Args> Tracer<Args>
where
    Args: Clone + Send + Sync + 'static,
{
    /// Create a new Tracer instance wrapped in Arc for sharing.
    pub fn new(xlayer_args: Args) -> Arc<Self> {
        Arc::new(Self { xlayer_args })
    }

    /// Handle fork choice updated events.
    ///
    /// This method is called when a fork choice update API is invoked (before execution).
    /// Implement your custom tracing logic here.
    ///
    /// # Parameters
    /// - `version`: The fork choice updated version (e.g., "v1", "v2", "v3")
    /// - `state`: The fork choice state containing head, safe, and finalized block hashes
    /// - `attrs`: Optional payload attributes for block building
    pub fn on_fork_choice_updated<EngineT: EngineTypes<ExecutionData = OpExecutionData>>(
        &self,
        version: &str,
        state: &ForkchoiceState,
        _attrs: &Option<EngineT::PayloadAttributes>,
    ) {
        tracing::info!(
            target: "xlayer::full_trace::fork_choice_updated",
            "Fork choice updated {} called: head={:?}, safe={:?}, finalized={:?}, has_attrs={}",
            version,
            state.head_block_hash,
            state.safe_block_hash,
            state.finalized_block_hash,
            _attrs.is_some()
        );

        // TODO: add SeqBlockBuildStart here
    }

    /// Handle new payload events.
    ///
    /// This method is called when a new payload API is invoked (before execution).
    /// Implement your custom tracing logic here.
    ///
    /// # Parameters
    /// - `version`: The payload version (e.g., "v2", "v3", "v4")
    /// - `block_info`: Block information containing block number and hash
    pub fn on_new_payload(&self, version: &str, block_info: &BlockInfo) {
        tracing::info!(
            target: "xlayer::full_trace::new_payload",
            "New payload {} called: block_number={}, block_hash={:?}",
            version,
            block_info.block_number,
            block_info.block_hash
        );

        // TODO: add SeqBlockSendStart, RpcBlockReceiveEnd here, use xlayer_args if you want
    }

    /// Handle transaction received events.
    ///
    /// This method is called when a transaction is received via RPC (e.g., eth_sendRawTransaction).
    /// Implement your custom tracing logic here.
    ///
    /// # Parameters
    /// - `method`: The RPC method name (e.g., "eth_sendRawTransaction")
    /// - `tx_hash`: The transaction hash
    pub fn on_recv_transaction(&self, method: &str, tx_hash: B256) {
        tracing::info!(
            target: "xlayer::full_trace::recv_transaction",
            "Transaction received via {}: tx_hash={:?}",
            method,
            tx_hash
        );

        // TODO: add RpcReceiveTxEnd, SeqReceiveTxEnd here, use xlayer_args if you want
    }

    /// Handle block commit events.
    ///
    /// This method is called when a block is committed to the canonical chain.
    /// Implement your custom tracing logic here.
    ///
    /// # Parameters
    /// - `block_info`: Block information containing block number and hash
    pub fn on_block_commit(&self, block_info: &BlockInfo) {
        tracing::info!(
            target: "xlayer::full_trace::block_commit",
            "Block committed: block_number={}, block_hash={:?}",
            block_info.block_number,
            block_info.block_hash
        );

        // TODO: add SeqBlockBuildEnd, RpcBlockInsertEnd here
    }

    /// Handle transaction commit events.
    ///
    /// This method is called when a transaction is committed to the canonical chain.
    /// Implement your custom tracing logic here.
    ///
    /// # Parameters
    /// - `block_info`: Block information containing block number and hash
    /// - `tx_hash`: The transaction hash
    pub fn on_tx_commit(&self, block_info: &BlockInfo, tx_hash: B256) {
        tracing::info!(
            target: "xlayer::full_trace::tx_commit",
            "Transaction committed: block_number={}, block_hash={:?}, tx_hash={:?}",
            block_info.block_number,
            block_info.block_hash,
            tx_hash
        );

        // TODO: add SeqTxExecutionEnd here if flashblocks is disabled, you can use xlayer_args if you want
    }

    /// Initialize blockchain tracer to monitor canonical state changes.
    ///
    /// This spawns a background task that subscribes to canonical state notifications
    /// and calls the tracer's event handlers for each block and transaction.
    ///
    /// # Example
    /// ```rust,ignore
    /// let tracer = Tracer::new(xlayer_args);
    /// tracer.initialize_blockchain_tracer(ctx.node());
    /// ```
    pub fn initialize_blockchain_tracer<Node>(self: &Arc<Self>, node: &Node)
    where
        Node: reth_node_api::FullNodeComponents + Clone + 'static,
        <Node as reth_node_api::FullNodeTypes>::Provider: reth_chain_state::CanonStateSubscriptions,
    {
        use reth_chain_state::CanonStateSubscriptions as _;

        let provider = node.provider().clone();
        let task_executor = node.task_executor().clone();

        // Subscribe to canonical state updates
        let canonical_stream = provider.canonical_state_stream();

        tracing::info!(target: "xlayer::full_trace::blockchain", "Initializing blockchain tracer for canonical state stream");

        let tracer = self.clone();
        task_executor.spawn_critical(
            "xlayer-blockchain-tracer",
            Box::pin(async move {
                crate::handle_canonical_state_stream(canonical_stream, tracer).await;
            }),
        );
    }
}

impl<Args> Default for Tracer<Args>
where
    Args: Clone + Send + Sync + Default + 'static,
{
    fn default() -> Self {
        Self { xlayer_args: Args::default() }
    }
}
