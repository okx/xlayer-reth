use std::future::Future;

use jsonrpsee::{
    core::middleware::{Batch, Notification},
    server::middleware::rpc::RpcServiceT,
    types::Request,
    MethodResponse,
};
use tracing::debug;

use crate::LegacyRpcRouterService;
use jsonrpsee_types::ErrorObject;

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
            let method = req.method_name();

            // If legacy not enabled, do not route.
            // Not under legacy routing, do not route.
            if !config.enabled || !is_legacy_routable(method) {
                return inner.call(req).await;
            }

            if method == "eth_getLogs" {
                return crate::get_logs::handle_eth_get_logs(req, client, config, inner).await;
            } else if method == "eth_getInternalTransactions" {
                return handle_eth_get_internal_transactions(req, client, config, inner).await;
            } else if should_try_local_then_legacy(method) {
                return handle_try_local_then_legacy(req, client, config, inner).await;
            } else if need_parse_block(method) {
                return handle_block_param_methods(req, client, config, inner).await;
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

async fn handle_eth_get_internal_transactions<S>(
    req: Request<'_>,
    client: reqwest::Client,
    config: std::sync::Arc<crate::LegacyRpcRouterConfig>,
    inner: S,
) -> MethodResponse
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    let service = LegacyRpcRouterService { inner: inner.clone(), config, client };
    let _p = req.params(); // keeps compiler quiet
    let params = _p.as_str().unwrap();
    let tx_hash = crate::parse_tx_hash_param(params, 0);
    if tx_hash.is_none() {
        return MethodResponse::error(
            req.id(),
            ErrorObject::owned(-32603, "Need proper txn hash", None::<()>),
        );
    }

    let res = service.get_transaction_by_hash(&tx_hash.unwrap()).await;

    // Route to legacy only if tx hash cannot be found
    if res.is_ok_and(|hash| hash.is_none()) {
        service.forward_to_legacy(req).await
    } else {
        inner.call(req).await
    }
}

async fn handle_try_local_then_legacy<S>(
    req: Request<'_>,
    client: reqwest::Client,
    config: std::sync::Arc<crate::LegacyRpcRouterConfig>,
    inner: S,
) -> MethodResponse
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    let method = req.method_name();
    let res = inner.call(req.clone()).await;
    if res.is_error() || (res.is_success() && is_result_empty(&res)) {
        let service = LegacyRpcRouterService { inner: inner.clone(), config, client };
        debug!(
            "Route to legacy for method = {method}. is_error = {}, is_empty_result = {}",
            res.is_error(),
            res.is_success()
        );
        service.forward_to_legacy(req).await
    } else {
        debug!("No legacy routing(local success with data) for method = {method}");
        res
    }
}

async fn handle_block_param_methods<S>(
    req: Request<'_>,
    client: reqwest::Client,
    config: std::sync::Arc<crate::LegacyRpcRouterConfig>,
    inner: S,
) -> MethodResponse
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    let _p = req.params(); // keeps compiler quiet
    let params = _p.as_str().unwrap();
    let method = req.method_name();
    let block_param =
        crate::parse_block_param(params, block_param_pos(method), config.cutoff_block);

    let cutoff_block = config.cutoff_block;
    if let Some(block_param) = block_param {
        let service = LegacyRpcRouterService { inner: inner.clone(), config, client };
        if need_get_block(method) && crate::is_block_hash(&block_param) {
            let res = service.call_eth_get_block_by_hash(&block_param, false).await;
            match res {
                Ok(n) => {
                    if n.is_none() {
                        debug!("Route to legacy for method (block by hash not found) = {}", method);
                        return service.forward_to_legacy(req).await;
                    } else {
                        // TODO: if block_num parsed from blk hash is smaller than
                        // cutoff, route to legacy as well?
                        debug!(
                            "No route to legacy since got block num from block hash. block = {:?}",
                            n
                        );
                    }
                }
                Err(err) => debug!("Error getting block by hash = {err:?}"),
            }
        } else {
            match block_param.parse::<u64>() {
                Ok(block_num) => {
                    debug!("block_num = {}", block_num);
                    if block_num < cutoff_block {
                        debug!("Route to legacy for method (below cuttoff) = {}", method);
                        return service.forward_to_legacy(req).await;
                    }
                }
                Err(err) => debug!("Failed to parse block num, err = {err:?}"),
            }
        }
    } else {
        debug!("Failed to parse block param, got None");
    }

    inner.call(req).await
}
