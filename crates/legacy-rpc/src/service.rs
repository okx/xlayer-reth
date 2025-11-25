use std::future::Future;

use jsonrpsee::{
    core::middleware::{Batch, Notification},
    server::middleware::rpc::RpcServiceT,
    types::Request,
    MethodResponse,
};

use crate::LegacyRpcRouterService;

impl<S> RpcServiceT for LegacyRpcRouterService<S>
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let should_route = self.should_route_to_legacy(&req);
        let inner = self.inner.clone();
        let client = self.client.clone();
        let config = self.config.clone();

        async move {
            if should_route {
                tracing::debug!(
                    target: "rpc::legacy",
                    method = req.method_name(),
                    "Routing to legacy endpoint"
                );

                // Create a temporary service just for forwarding
                let service = LegacyRpcRouterService { inner, config, client };
                service.forward_to_legacy(req).await
            } else {
                inner.call(req).await
            }
        }
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        // For batches, could implement per-request routing or route entire batch
        self.inner.batch(req)
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}
