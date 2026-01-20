//! Core trait definitions for Engine API middleware

use op_alloy_rpc_types_engine::OpExecutionData;
use reth_chainspec::EthereumHardforks;
use reth_node_api::{EngineApiValidator, EngineTypes};
use reth_optimism_rpc::{OpEngineApi, OpEngineApiServer};
use reth_rpc_api::IntoEngineApiRpcModule;
use reth_storage_api::{BlockReader, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;

/// XLayer Engine API middleware trait.
///
/// This trait defines the requirements for an Engine API middleware implementation.
///
pub trait XLayerEngineApiMiddleware<Provider, EngineT, Pool, Validator, ChainSpec>:
    OpEngineApiServer<EngineT> + IntoEngineApiRpcModule + Send + Sync
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
{
    /// Set the inner OpEngineApi on the middleware.
    ///
    /// This method is called by the XLayerEngineApiBuilder to inject the inner OpEngineApi
    /// after it has been built.
    fn set_inner(&mut self, inner: OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>);
}
