//! RPC tracer middleware for tracing transaction submissions

use crate::Tracer;
use alloy_primitives::B256;
use futures::future::Either;
use jsonrpsee::{
    core::middleware::{Batch, Notification},
    server::middleware::rpc::RpcServiceT,
    types::Request,
    MethodResponse,
};
use op_alloy_rpc_types_engine::OpExecutionData;
use reth_chainspec::EthereumHardforks;
use reth_node_api::{EngineApiValidator, EngineTypes};
use reth_storage_api::{BlockReader, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use std::{future::Future, sync::Arc};
use tower::Layer;
use tracing::trace;

/// Layer that creates the RPC tracing middleware
#[derive(Clone)]
pub struct RpcTracerLayer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Args: Clone + Send + Sync + 'static,
{
    tracer: Arc<Tracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>>,
}

impl<Provider, EngineT, Pool, Validator, ChainSpec, Args>
    RpcTracerLayer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
    Args: Clone + Send + Sync + 'static,
{
    /// Create a new RPC tracer layer
    pub fn new(tracer: Arc<Tracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>>) -> Self {
        Self { tracer }
    }
}

impl<S, Provider, EngineT, Pool, Validator, ChainSpec, Args> Layer<S>
    for RpcTracerLayer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Args: Clone + Send + Sync + 'static,
{
    type Service = RpcTracerService<S, Provider, EngineT, Pool, Validator, ChainSpec, Args>;

    fn layer(&self, inner: S) -> Self::Service {
        RpcTracerService { inner, tracer: self.tracer.clone() }
    }
}

/// RPC tracer service that intercepts RPC calls
#[derive(Clone)]
pub struct RpcTracerService<S, Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Args: Clone + Send + Sync + 'static,
{
    inner: S,
    tracer: Arc<Tracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>>,
}

impl<S, Provider, EngineT, Pool, Validator, ChainSpec, Args> RpcServiceT
    for RpcTracerService<S, Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
    Args: Clone + Send + Sync + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let method = req.method_name();

        // Check if this is a transaction submission method
        let should_trace = matches!(method, "eth_sendRawTransaction" | "eth_sendTransaction");

        // Early return if not a transaction method - no boxing, direct passthrough
        if !should_trace {
            return Either::Left(self.inner.call(req));
        }

        trace!(
            target: "xlayer::full_trace::rpc",
            "Transaction submission intercepted: method={}",
            method
        );

        let tracer = self.tracer.clone();
        let inner = self.inner.clone();
        let method_owned = method.to_string();

        Either::Right(Box::pin(async move {
            // Call the inner service
            let response = inner.call(req).await;

            // Try to parse the response as a transaction hash
            if let Ok(response_json) = serde_json::from_str::<serde_json::Value>(response.as_ref())
                && let Some(result) = response_json.get("result")
                && let Some(tx_hash_str) = result.as_str()
                && let Ok(tx_hash) = tx_hash_str.parse::<B256>()
            {
                tracer.on_recv_transaction(&method_owned, tx_hash);
            }

            response
        }))
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        // For batches, we pass through to the inner service
        // Could implement per-request tracing if needed
        self.inner.batch(req)
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}
