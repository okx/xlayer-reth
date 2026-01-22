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
use std::{future::Future, sync::Arc};
use tower::Layer;
use tracing::trace;

/// Layer that creates the RPC tracing middleware.
#[derive(Clone)]
pub struct RpcTracerLayer {
    tracer: Arc<Tracer>,
}

impl RpcTracerLayer {
    /// Create a new RPC tracer layer with a shared tracer.
    pub fn new(tracer: Arc<Tracer>) -> Self {
        Self { tracer }
    }
}

impl<S> Layer<S> for RpcTracerLayer {
    type Service = RpcTracerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RpcTracerService { inner, tracer: self.tracer.clone() }
    }
}

/// RPC tracer service that intercepts RPC calls.
///
/// This service is generic only over the inner service `S`,
/// making it simple to use while still providing access to the shared `Tracer`.
#[derive(Clone)]
pub struct RpcTracerService<S> {
    inner: S,
    tracer: Arc<Tracer>,
}

impl<S> RpcServiceT for RpcTracerService<S>
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
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

        Either::Right(async move {
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
        })
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
