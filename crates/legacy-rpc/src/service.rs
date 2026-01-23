use std::future::Future;

use futures::future::Either;
use jsonrpsee::{
    core::middleware::{Batch, BatchEntry, Notification},
    server::middleware::rpc::RpcServiceT,
    types::Request,
    BatchResponseBuilder, MethodResponse,
};
use tracing::debug;

use crate::LegacyRpcRouterService;

/// Indicates where a request should be routed
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum RouteDestination {
    /// Route to local node
    Local,
    /// Route to legacy endpoint
    Legacy,
    /// Special handling for eth_getLogs
    EthGetLogs,
    /// Try local first, then legacy if empty/error
    TryLocalThenLegacy,
    /// Parse block parameter to determine routing
    BlockParamMethod,
}

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
            | "eth_transactionPreExec"
            | "debug_traceTransaction"
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
    )
}

/// Need to fetch block num from DB/API
#[inline]
fn can_use_block_hash_as_param(method: &str) -> bool {
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
fn need_try_local_then_legacy(method: &str) -> bool {
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
            | "debug_traceTransaction"
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
    S: RpcServiceT<MethodResponse = MethodResponse, BatchResponse = MethodResponse>
        + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = MethodResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let method = req.method_name();

        // Early return - no boxing, direct passthrough
        if !self.config.enabled || !is_legacy_routable(method) {
            return Either::Left(self.inner.call(req));
        }

        let client = self.client.clone();
        let config = self.config.clone();
        let inner = self.inner.clone();

        Either::Right(Box::pin(async move {
            let method = req.method_name();

            if method == "eth_getLogs" {
                return crate::get_logs::handle_eth_get_logs(req, client, config, inner).await;
            } else if need_try_local_then_legacy(method) {
                return handle_try_local_then_legacy(req, client, config, inner).await;
            } else if need_parse_block(method) {
                return handle_block_param_methods(req, client, config, inner).await;
            }

            debug!(target:"xlayer_legacy_rpc", "No legacy routing for method = {}", method);
            // Default resorts to normal rpc calls.
            inner.call(req).await
        }))
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        // Early return if legacy routing is disabled
        if !self.config.enabled {
            return Either::Left(self.inner.batch(req));
        }

        let client = self.client.clone();
        let config = self.config.clone();
        let inner = self.inner.clone();

        Either::Right(Box::pin(async move {
            let cutoff_block = config.cutoff_block;

            // Categorize requests and track their positions
            let mut legacy_batch_requests = Vec::new(); // Requests that go directly to legacy as a batch
            let mut individual_requests = Vec::new(); // Requests needing individual processing
            let mut notifications = Vec::new();
            let mut response_index = 0;

            for entry in req {
                match entry {
                    Ok(BatchEntry::Call(request)) => {
                        let method = request.method_name();

                        if !is_legacy_routable(method) {
                            // Not routable - process locally
                            individual_requests.push((
                                request,
                                response_index,
                                RouteDestination::Local,
                            ));
                            response_index += 1;
                        } else if method == "eth_getLogs" {
                            // Special hybrid handling needed
                            individual_requests.push((
                                request,
                                response_index,
                                RouteDestination::EthGetLogs,
                            ));
                            response_index += 1;
                        } else if need_try_local_then_legacy(method) {
                            // Try local first, then legacy
                            individual_requests.push((
                                request,
                                response_index,
                                RouteDestination::TryLocalThenLegacy,
                            ));
                            response_index += 1;
                        } else if need_parse_block(method) {
                            // Parse block parameter to determine if it goes to legacy
                            let _p = request.params();
                            let params = _p.as_str().unwrap_or("[]");
                            let block_param =
                                crate::parse_block_param(params, block_param_pos(method));

                            if block_below_cutoff(method, block_param, cutoff_block) {
                                // Can batch this to legacy
                                legacy_batch_requests.push((request, response_index));
                                response_index += 1;
                            } else {
                                // Needs individual processing (block hash resolution or local)
                                individual_requests.push((
                                    request,
                                    response_index,
                                    RouteDestination::BlockParamMethod,
                                ));
                                response_index += 1;
                            }
                        } else {
                            // Unexpected - process locally
                            individual_requests.push((
                                request,
                                response_index,
                                RouteDestination::Local,
                            ));
                            response_index += 1;
                        }
                    }
                    Ok(BatchEntry::Notification(notif)) => {
                        notifications.push(notif);
                    }
                    Err(_) => {
                        // Skip malformed entries
                        continue;
                    }
                }
            }

            // Process notifications (no responses)
            for notif in notifications {
                let _ = inner.notification(notif).await;
            }

            let mut responses: Vec<Option<MethodResponse>> =
                (0..response_index).map(|_| None).collect();

            let service = LegacyRpcRouterService {
                inner: inner.clone(),
                config: config.clone(),
                client: client.clone(),
            };

            // Process legacy batch requests as a single batch
            if !legacy_batch_requests.is_empty() {
                debug!(
                    target: "xlayer_legacy_rpc",
                    "Forwarding {} requests to legacy as batch",
                    legacy_batch_requests.len()
                );
                let legacy_responses = service.forward_batch_to_legacy(legacy_batch_requests).await;
                for (response, pos) in legacy_responses {
                    responses[pos] = Some(response);
                }
            }

            // Process individual requests with their specific routing logic
            for (request, pos, route_dest) in individual_requests {
                let response = match route_dest {
                    RouteDestination::EthGetLogs => {
                        crate::get_logs::handle_eth_get_logs(
                            request,
                            client.clone(),
                            config.clone(),
                            inner.clone(),
                        )
                        .await
                    }
                    RouteDestination::TryLocalThenLegacy => {
                        handle_try_local_then_legacy(
                            request,
                            client.clone(),
                            config.clone(),
                            inner.clone(),
                        )
                        .await
                    }
                    RouteDestination::BlockParamMethod => {
                        handle_block_param_methods(
                            request,
                            client.clone(),
                            config.clone(),
                            inner.clone(),
                        )
                        .await
                    }
                    RouteDestination::Local => inner.call(request).await,
                    RouteDestination::Legacy => {
                        // Shouldn't reach here, but handle it
                        service.forward_to_legacy(request).await
                    }
                };
                responses[pos] = Some(response);
            }

            let mut batch_response = BatchResponseBuilder::new_with_limit(usize::MAX);
            for response in responses.into_iter().flatten() {
                if let Err(err) = batch_response.append(response) {
                    return err;
                }
            }

            MethodResponse::from_batch(batch_response.finish())
        }))
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}

#[inline]
pub(crate) fn block_below_cutoff(
    method: &str,
    block_param: Option<String>,
    cutoff_block: u64,
) -> bool {
    if let Some(block_param) = block_param {
        // Check if it's a block hash (always needs individual processing)
        if can_use_block_hash_as_param(method) && crate::is_block_hash(&block_param) {
            false // Needs individual processing to resolve hash
        } else {
            // Check if block number is below cutoff
            match block_param.parse::<u64>() {
                Ok(block_num) => block_num < cutoff_block,
                Err(_) => false,
            }
        }
    } else {
        false
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
            target:"xlayer_legacy_rpc",
            "Route to legacy for method = {method}. is_error = {}, is_empty_result = {}",
            res.is_error(),
            res.is_success()
        );
        service.forward_to_legacy(req).await
    } else {
        debug!(target:"xlayer_legacy_rpc", "No legacy routing(local success with data) for method = {method}");
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
    let block_param = crate::parse_block_param(params, block_param_pos(method));

    let cutoff_block = config.cutoff_block;
    if let Some(block_param) = block_param {
        let service = LegacyRpcRouterService { inner: inner.clone(), config, client };
        if can_use_block_hash_as_param(method) && crate::is_block_hash(&block_param) {
            let res = service.call_eth_get_block_by_hash(&block_param, false).await;
            match res {
                Ok(n) => {
                    if n.is_none() {
                        debug!(target:"xlayer_legacy_rpc", "Route to legacy for method (block by hash not found) = {}", method);
                        return service.forward_to_legacy(req).await;
                    } else {
                        // TODO: if block_num parsed from blk hash is smaller than
                        // cutoff, route to legacy as well?
                        debug!(
                            target:"xlayer_legacy_rpc",
                            "No route to legacy since got block num from block hash. block = {:?}",
                            n
                        );
                    }
                }
                Err(err) => {
                    debug!(target:"xlayer_legacy_rpc", "Error getting block by hash = {err:?}")
                }
            }
        } else if block_below_cutoff(method, Some(block_param), cutoff_block) {
            debug!(target:"xlayer_legacy_rpc", "Route to legacy for method (below cuttoff) = {}", method);
            return service.forward_to_legacy(req).await;
        }
    } else {
        debug!(target:"xlayer_legacy_rpc", "Failed to parse block param, got None");
    }

    debug!(target:"xlayer_legacy_rpc", "No legacy routing for method = {}", method);
    inner.call(req).await
}
