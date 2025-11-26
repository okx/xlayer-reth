use std::future::Future;

use jsonrpsee::{
    core::middleware::{Batch, Notification},
    server::middleware::rpc::RpcServiceT,
    types::Request,
    MethodResponse,
};

use crate::LegacyRpcRouterService;

/// Takes block number/hash as param
#[inline]
fn need_parse_block(method: &str) -> bool {
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
            | "eth_transactionPreExec"
    )
}

/// Need to fetch block num from DB/API
#[inline]
fn need_get_block(method: &str) -> bool {
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
            | "eth_transactionPreExec"
    )
}

/// Returns the block param index.
///
/// In eth requests, there is params list: [...].
/// Looks at each method and decides block num/hash
/// param position in that argument list.
#[inline]
fn block_param_pos(method: &str) -> usize {
    // 2nd position (index 1)
    if matches!(
        method,
        "eth_getBalance"
            | "eth_getCode"
            | "eth_getTransactionCount"
            | "eth_call"
            | "eth_estimateGas"
            | "eth_createAccessList"
            | "eth_transactionPreExec"
    ) {
        return 1;
    }

    // 3rd position (index 2)
    if matches!(method, "eth_getStorageAt") {
        return 2;
    }

    0
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

            // Not under legacy routing
            if !crate::is_legacy_routable(&method) {
                return inner.call(req).await;
            }

            if need_parse_block(&method) {
                let block_param =
                    crate::parse_block_param(params, block_param_pos(&method), config.cutoff_block);
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
