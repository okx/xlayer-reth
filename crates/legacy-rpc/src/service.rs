use std::future::Future;

use jsonrpsee::{
    core::middleware::{Batch, Notification},
    server::middleware::rpc::RpcServiceT,
    types::Request,
    MethodResponse,
};
use tracing::debug;

use crate::LegacyRpcRouterService;

/// Only these methods should be considered for legacy routing.
#[inline]
pub fn is_legacy_routable(method: &str) -> bool {
    matches!(
        method,
        "eth_getBlockByNumber"
            | "eth_getBlockByHash"
            | "eth_getBlockTransactionCountByNumber"
            | "eth_getBlockTransactionCountByHash"
            | "eth_getBlockReceipts"
            | "eth_getHeaderByNumber"
            | "eth_getHeaderByHash"
            | "eth_getTransactionByHash"
            | "eth_getTransactionReceipt"
            | "eth_getTransactionByBlockHashAndIndex"
            | "eth_getTransactionByBlockNumberAndIndex"
            | "eth_getRawTransactionByHash"
            | "eth_getRawTransactionByBlockHashAndIndex"
            | "eth_getRawTransactionByBlockNumberAndIndex"
            | "eth_getBalance"
            | "eth_getCode"
            | "eth_getStorageAt"
            | "eth_getTransactionCount"
            | "eth_call"
            | "eth_estimateGas"
            | "eth_createAccessList"
            | "eth_getLogs"
            | "eth_getInternalTransactions"
            | "eth_getBlockInternalTransactions"
            | "eth_transactionPreExec"
    )
}

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
            | "eth_getBlockInternalTransactions"
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

#[inline]
fn should_try_local_then_legacy(method: &str) -> bool {
    matches!(
        method,
        "eth_getTransactionByHash"
            | "eth_getTransactionReceipt"
            | "eth_getRawTransactionByHash"
            | "eth_getBlockByHash"
            | "eth_getHeaderByHash"
            | "eth_getBlockTransactionCountByHash"
            | "eth_getTransactionByBlockHashAndIndex"
            | "eth_getRawTransactionByBlockHashAndIndex"
    )
}

/// Check if the response has a non-empty result.
/// Returns true if the result is null, an empty object {}, or an empty array [].
fn is_result_empty(response: &MethodResponse) -> bool {
    // Parse the JSON response
    let json_str = response.as_ref();
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str)
        && let Some(result) = json.get("result")
    {
        match result {
            serde_json::Value::Null => return true,
            serde_json::Value::Object(obj) => return obj.is_empty(),
            serde_json::Value::Array(arr) => return arr.is_empty(),
            _ => return false,
        }
    }
    // If we can't parse or no result field, consider it non-empty
    false
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
            if !is_legacy_routable(&method) {
                return inner.call(req).await;
            }

            if method == "eth_getInternalTransactions" {
                let tx_hash = crate::parse_tx_hash_param(params, 0);
                if let Some(tx_hash) = tx_hash {
                    let service = LegacyRpcRouterService {
                        inner: inner.clone(),
                        config: config.clone(),
                        client: client.clone(),
                    };
                    let res = service.get_transaction_by_hash(&tx_hash).await;
                    // Route to legacy only if tx hash cannot be found.
                    if res.is_ok_and(|hash| hash.is_none()) {
                        return service.forward_to_legacy(req).await;
                    }
                }
            }

            if should_try_local_then_legacy(&method) {
                // Try local first
                let res = inner.call(req.clone()).await;

                // If error, forward to legacy
                // If success but result is empty (null, {}, or []), forward to legacy
                if res.is_error() || (res.is_success() && is_result_empty(&res)) {
                    debug!(
                        "Route to legacy for method = {}. is_error = {}, is_empty_result = {}",
                        method,
                        res.is_error(),
                        res.is_success()
                    );
                    let service = LegacyRpcRouterService {
                        inner: inner.clone(),
                        config: config.clone(),
                        client: client.clone(),
                    };
                    return service.forward_to_legacy(req).await;
                }

                // Success with non-empty result, return local response
                debug!("No legacy routing for method (local success with data) = {}", method);
                return res;
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
                        let res = service.call_eth_get_block_by_hash(&block_param, false).await;

                        match res {
                            Ok(n) => {
                                if n.is_none() {
                                    debug!(
                                        "Route to legacy for method (block by hash not found) = {}",
                                        method
                                    );
                                    return service.forward_to_legacy(req).await;
                                } else {
                                    // TODO: if block_num parsed from blk hash is smaller than
                                    // cutoff, route to legacy as well?
                                    debug!("No route to legacy since got block num from block hash. block = {}", n.unwrap());
                                }
                            }
                            Err(err) => debug!("Error getting block by hash = {err:?}"),
                        }
                    } else {
                        match block_param.parse::<u64>() {
                            Ok(block_num) => {
                                debug!("block_num = {}", block_num);
                                if block_num < service.config.cutoff_block {
                                    debug!(
                                        "Route to legacy for method (below cuttoff) = {}",
                                        method
                                    );
                                    return service.forward_to_legacy(req).await;
                                }
                            }
                            Err(err) => debug!("Failed to parse block num, err = {err:?}"),
                        }
                    }
                } else {
                    debug!("Failed to parse block param, got None");
                }
            }

            debug!("No legacy routing for method = {}", method);
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
