//! Tracer implementation for full trace support

use crate::EngineApiTracer;
use alloy_primitives::B256;
use alloy_rpc_types_engine::ForkchoiceState;
use op_alloy_rpc_types_engine::OpExecutionData;
use reth_chainspec::EthereumHardforks;
use reth_node_api::{EngineApiValidator, EngineTypes};
use reth_storage_api::{BlockReader, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use std::sync::Arc;

/// Block information for tracing.
#[derive(Debug, Clone)]
pub struct BlockInfo {
    /// The block number
    pub block_number: u64,
    /// The block hash
    pub block_hash: B256,
}

/// Tracer struct that holds tracing state and configuration.
///
/// This is the main entry point for the full trace functionality.
/// It manages the tracing configuration and implements event handlers.
pub struct Tracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Args: Clone + Send + Sync + 'static,
{
    /// XLayer arguments (reserved for future use)
    #[allow(dead_code)]
    xlayer_args: Args,
    _phantom: std::marker::PhantomData<(Provider, EngineT, Pool, Validator, ChainSpec)>,
}

impl<Provider, EngineT, Pool, Validator, ChainSpec, Args>
    Tracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
    Args: Clone + Send + Sync + 'static,
{
    /// Create a new Tracer instance.
    pub fn new(xlayer_args: Args) -> Self {
        Self { xlayer_args, _phantom: std::marker::PhantomData }
    }

    /// Build an EngineApiTracer with this tracer instance.
    ///
    /// This creates a new EngineApiTracer instance that holds a reference to this tracer.
    pub fn build_engine_api_tracer(
        self: Arc<Self>,
    ) -> EngineApiTracer<Provider, EngineT, Pool, Validator, ChainSpec, Args> {
        EngineApiTracer::new(self)
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
    pub(crate) fn on_fork_choice_updated(
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
    pub(crate) fn on_new_payload(&self, version: &str, block_info: &BlockInfo) {
        // TODO: Implement your custom new payload tracing logic here
        tracing::info!(
            target: "xlayer::full_trace::new_payload",
            "New payload {} called: block_number={}, block_hash={:?}",
            version,
            block_info.block_number,
            block_info.block_hash
        );

        // TODO: add SeqBlockSendStart, RpcBlockReceiveEnd here, use xlayer_args if you want
    }
}

impl<Provider, EngineT, Pool, Validator, ChainSpec, Args> Default
    for Tracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
    Args: Clone + Send + Sync + Default + 'static,
{
    fn default() -> Self {
        Self::new(Args::default())
    }
}
