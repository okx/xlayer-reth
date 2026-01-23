use crate::monitor::XLayerMonitor;

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

/// Layer that creates the RPC full link monitor middleware.
#[derive(Clone)]
pub struct RpcMonitorLayer {
    monitor: Arc<XLayerMonitor>,
}

impl RpcMonitorLayer {
    pub fn new(monitor: Arc<XLayerMonitor>) -> Self {
        Self { monitor }
    }
}

impl<S> Layer<S> for RpcMonitorLayer {
    type Service = RpcMonitorService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RpcMonitorService { inner, monitor: self.monitor.clone() }
    }
}

/// RPC monitor service that intercepts RPC calls.
#[derive(Clone)]
pub struct RpcMonitorService<S> {
    inner: S,
    monitor: Arc<XLayerMonitor>,
}

impl<S> RpcServiceT for RpcMonitorService<S>
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let method = req.method_name();
        if !matches!(method, "eth_sendRawTransaction" | "eth_sendTransaction") {
            return Either::Left(self.inner.call(req));
        }

        let monitor = self.monitor.clone();
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
                monitor.on_recv_transaction(&method_owned, tx_hash);
                trace!(
                    target: "xlayer::monitor::rpc",
                    "Transaction submission intercepted: method={}",
                    method_owned
                );
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
