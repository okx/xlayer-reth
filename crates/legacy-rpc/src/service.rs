use std::future::Future;

use jsonrpsee::{
    core::middleware::{Batch, Notification},
    server::middleware::rpc::RpcServiceT,
    types::Request,
    MethodResponse,
};

use crate::LegacyRpcRouterService;

#[inline]
fn need_parse_block(method: &str) -> bool {
    // route_by_number + route_by_block_id + route_by_block_id_opt
    matches!(
        method,
        "eth_getBlockByNumber"
            | "eth_getBlockTransactionCountByNumber"
            | "eth_getHeaderByNumber"
            | "eth_getTransactionByBlockNumberAndIndex"
            | "eth_getRawTransactionByBlockNumberAndIndex"
            | "eth_getBlockReceipts"
            | "eth_getBalance"
            | "eth_getCode"
            | "eth_getStorageAt"
            | "eth_getTransactionCount"
            | "eth_call"
            | "eth_estimateGas"
            | "eth_createAccessList"
    )
}

#[inline]
fn need_get_block(method: &str) -> bool {
    // route_by_block_id + route_by_block_id_opt
    matches!(
        method,
        "eth_getBlockReceipts"
            | "eth_getBalance"
            | "eth_getCode"
            | "eth_getStorageAt"
            | "eth_getTransactionCount"
            | "eth_call"
            | "eth_estimateGas"
            | "eth_createAccessList"
    )
}

impl<S> RpcServiceT for LegacyRpcRouterService<S>
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let client = self.client.clone();
        let config = self.config.clone();
        let inner = self.inner.clone();

        Box::pin(async move {
            let _p = req.params(); // keeps compiler quiet
            let params = _p.as_str().unwrap();
            let method = req.method_name().to_string();

            // If legacy not enabled, do not route.
            if !config.enabled {
                return inner.call(req).await;
            }

            if need_parse_block(&method) {
                let block_param = crate::parse_block_param(params, 0, config.cutoff_block);
                if let Some(block_param) = block_param {
                    // Clone to prevent lifetime error
                    let service = LegacyRpcRouterService {
                        inner: inner.clone(),
                        config: config.clone(),
                        client: client.clone(),
                    };
                    // Only some methods that need to get block from DB do this.
                    if need_get_block(&method) && crate::is_block_hash(&block_param) {
                        // If failed to get block number internally, route to legacy then.
                        if service
                            .call_eth_get_block_by_hash(&block_param, false)
                            .await
                            .ok()
                            .is_none()
                        {
                            let service = LegacyRpcRouterService { inner, config, client };
                            return service.forward_to_legacy(req).await;
                        }
                    } else {
                        let block_num = block_param.parse::<u64>().unwrap();
                        if block_num < service.config.cutoff_block {
                            return service.forward_to_legacy(req).await;
                        }
                    }
                }
            }

            // Default resorts to normal rpc calls.
            inner.call(req).await
        })
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
