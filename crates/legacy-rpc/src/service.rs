use std::future::Future;

use futures::{future::Either, stream::FuturesOrdered, StreamExt};
use jsonrpsee::{
    core::middleware::{Batch, BatchEntry, Notification},
    server::middleware::rpc::RpcServiceT,
    types::{error::INVALID_PARAMS_CODE, ErrorCode, ErrorObject, Id, Request},
    BatchResponseBuilder, MethodResponse,
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
            | "debug_traceTransaction"
            | "debug_traceBlockByNumber"
            | "debug_traceBlockByHash"
            | "debug_traceCall"
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
    )
}

/// Check if the response has a non-empty result.
/// Returns true if the result is null, an empty object {}, or an empty array [].
pub(crate) fn is_result_empty(response: &MethodResponse) -> bool {
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
            // debug_traceCall: params = [callObject, blockNumberOrTag?, traceConfig?]
            | "debug_traceCall"
    ) {
        return 1;
    }

    // 3rd position (index 2)
    if matches!(method, "eth_getStorageAt") {
        return 2;
    }

    0
}

/// Returns true if the raw block parameter is one of the five special block
/// tags. Used ONLY by the four debug trace methods, which must handle all five
/// tags (including `earliest`) with the local backend.
///
/// This intentionally diverges from `parse_block_param` (used by eth_* methods),
/// which maps `earliest` to `"0"` and thus routes it to legacy. The debug path
/// performs this pre-check BEFORE any height comparison and does not modify
/// `parse_block_param`, so the eth_* `earliest -> legacy` semantics are preserved.
#[inline]
fn is_special_block_tag(raw: &str) -> bool {
    matches!(raw, "latest" | "pending" | "safe" | "finalized" | "earliest")
}

/// Reads the raw (unparsed) string value of the block parameter at `index` from
/// a JSON-RPC params array. Returns `None` when the params are not an array, the
/// index is out of bounds, or the element is not a JSON string. Used by the debug
/// tag pre-check so a special tag can be detected before height resolution.
#[inline]
fn raw_block_param_at(params: &str, index: usize) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(params).ok()?;
    let arr = parsed.as_array()?;
    arr.get(index)?.as_str().map(String::from)
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
                crate::get_logs::handle_eth_get_logs(req, client, config, inner).await
            } else if method == "debug_traceBlockByHash" {
                // Debug trace methods route deterministically by the target block's
                // height relative to the cutoff.
                handle_debug_block_by_hash(req, client, config, inner).await
            } else if method == "debug_traceTransaction" {
                handle_debug_trace_transaction(req, client, config, inner).await
            } else if method == "debug_traceBlockByNumber" || method == "debug_traceCall" {
                // Special-tag pre-check: for the debug methods, all five tags
                // (latest/pending/safe/finalized/earliest) are served locally,
                // BEFORE any height comparison.
                if let Some(raw) = req
                    .params()
                    .as_str()
                    .and_then(|p| raw_block_param_at(p, block_param_pos(method)))
                    && is_special_block_tag(&raw)
                {
                    debug!(target:"xlayer_legacy_rpc", "Route to local (special-tag = {raw}) for method = {method}");
                    inner.call(req).await
                } else {
                    handle_block_param_methods(req, client, config, inner).await
                }
            } else if need_try_local_then_legacy(method) {
                handle_try_local_then_legacy(req, client, config, inner).await
            } else if need_parse_block(method) {
                handle_block_param_methods(req, client, config, inner).await
            } else {
                debug!(target:"xlayer_legacy_rpc", "No legacy routing for method = {}", method);
                // Default resorts to normal rpc calls.
                inner.call(req).await
            }
        }))
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        // Early return if legacy routing is disabled
        if !self.config.enabled {
            return Either::Left(self.inner.batch(req));
        }

        let service = self.clone();

        Either::Right(Box::pin(async move {
            // Collect all entries first to avoid lifetime issues
            let entries: Vec<_> = req.into_iter().collect();

            // Process all requests concurrently using FuturesOrdered
            // This significantly improves latency for batch requests with multiple calls
            let mut futures: FuturesOrdered<_> = entries
                .into_iter()
                .filter_map(|entry| match entry {
                    Ok(BatchEntry::Call(request)) => Some(Either::Right(service.call(request))),
                    Ok(BatchEntry::Notification(_notif)) => {
                        // Notifications should not be answered
                        // Note: we don't process notifications in batch context
                        None
                    }
                    Err(_) => {
                        // Return error response for malformed entries
                        Some(Either::Left(async {
                            MethodResponse::error(
                                Id::Null,
                                ErrorObject::from(ErrorCode::InvalidRequest),
                            )
                        }))
                    }
                })
                .collect();

            let mut batch_response = BatchResponseBuilder::new_with_limit(usize::MAX);
            while let Some(response) = futures.next().await {
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
    let params_ref = req.params();
    let Some(params) = params_ref.as_str() else {
        return MethodResponse::error(
            req.id(),
            ErrorObject::owned(INVALID_PARAMS_CODE, "Missing required params", None::<()>),
        );
    };
    let method = req.method_name();
    let block_param = crate::parse_block_param(params, block_param_pos(method));

    let cutoff_block = config.cutoff_block;
    if let Some(block_param) = block_param {
        let service = LegacyRpcRouterService { inner: inner.clone(), config, client };
        if can_use_block_hash_as_param(method) && crate::is_valid_32_bytes_string(&block_param) {
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
                    debug!(target:"xlayer_legacy_rpc", "Error getting block by hash = {err:?}, forwarding to legacy");
                    return service.forward_to_legacy(req).await;
                }
            }
        } else {
            match block_param.parse::<u64>() {
                Ok(block_num) => {
                    debug!(target:"xlayer_legacy_rpc", "block_num = {}", block_num);
                    if block_num < cutoff_block {
                        debug!(target:"xlayer_legacy_rpc", "Route to legacy for method (below cuttoff) = {}", method);
                        return service.forward_to_legacy(req).await;
                    }
                }
                Err(err) => {
                    debug!(target:"xlayer_legacy_rpc", "Failed to parse block num, err = {err:?}")
                }
            }
        }
    } else {
        debug!(target:"xlayer_legacy_rpc", "Failed to parse block param, got None");
    }

    debug!(target:"xlayer_legacy_rpc", "No legacy routing for method = {}", method);
    inner.call(req).await
}

/// Routes based on a resolved block height compared against the cutoff.
/// Shared by `debug_traceBlockByHash` and `debug_traceTransaction`:
/// - resolved height `< cutoff` -> legacy
/// - resolved height `>= cutoff` -> local
/// - resolved to `None` (not found) -> legacy
/// - resolve error -> local
async fn route_by_resolved_height<S>(
    req: Request<'_>,
    service: LegacyRpcRouterService<S>,
    resolved: Result<Option<u64>, String>,
    method: &str,
) -> MethodResponse
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    match resolved {
        Ok(Some(height)) => {
            if height < service.config.cutoff_block {
                debug!(target:"xlayer_legacy_rpc", "Route to legacy (below-cutoff, height = {height}) for method = {method}");
                return service.forward_to_legacy(req).await;
            }
            debug!(target:"xlayer_legacy_rpc", "Route to local (current-height, height = {height}) for method = {method}");
        }
        Ok(None) => {
            debug!(target:"xlayer_legacy_rpc", "Route to legacy (not-found) for method = {method}");
            return service.forward_to_legacy(req).await;
        }
        Err(err) => {
            debug!(target:"xlayer_legacy_rpc", "Route to local (local-error = {err}) for method = {method}");
        }
    }
    service.inner.call(req).await
}

/// Routing for `debug_traceBlockByHash`.
///
/// The block hash (params[0]) is resolved to a height via the local
/// `eth_getBlockByHash`, then compared against the cutoff:
/// - resolved height `< cutoff`            -> legacy
/// - resolved height `>= cutoff`           -> local
/// - valid hash, block not found locally   -> legacy (history may live on legacy)
/// - missing / invalid hash / internal err -> local (standard JSON-RPC error)
///
/// A dedicated handler (rather than reusing the eth_* hash branch in
/// `handle_block_param_methods`) is used because that branch does NOT route to
/// legacy when a resolved height is below cutoff (see its TODO), and changing it
/// would affect out-of-scope eth_* methods.
async fn handle_debug_block_by_hash<S>(
    req: Request<'_>,
    client: reqwest::Client,
    config: std::sync::Arc<crate::LegacyRpcRouterConfig>,
    inner: S,
) -> MethodResponse
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    let method = req.method_name().to_owned();

    // params[0] is the block hash; parse_block_param validates it (0x + 66 hex).
    let block_hash = req.params().as_str().and_then(|p| crate::parse_block_param(p, 0));

    let Some(block_hash) = block_hash else {
        debug!(target:"xlayer_legacy_rpc", "Route to local (invalid/missing block hash) for method = {method}");
        return inner.call(req).await;
    };

    let service = LegacyRpcRouterService { inner, config, client };
    let resolved = service.call_eth_get_block_by_hash(&block_hash, false).await;
    route_by_resolved_height(req, service, resolved, &method).await
}

/// Routing for `debug_traceTransaction`.
///
/// The transaction hash (params[0]) is resolved to its containing block height
/// via the local `eth_getTransactionByHash`, then compared against the cutoff.
/// Routing is NOT conditioned on the local trace succeeding — for a historical
/// transaction the request goes straight to legacy, replacing the old
/// try-local-then-legacy behavior:
/// - tx block height `< cutoff`            -> legacy (directly, no local trace)
/// - tx block height `>= cutoff`           -> local
/// - valid hash, tx not found locally      -> legacy
/// - invalid hash / internal locate error  -> local (standard JSON-RPC error)
async fn handle_debug_trace_transaction<S>(
    req: Request<'_>,
    client: reqwest::Client,
    config: std::sync::Arc<crate::LegacyRpcRouterConfig>,
    inner: S,
) -> MethodResponse
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    let method = req.method_name().to_owned();

    // Extract the tx hash from params[0] via safe JSON parsing. The helper below
    // re-validates it (is_valid_32_bytes_string) before interpolation.
    let tx_hash = req.params().as_str().and_then(|p| {
        serde_json::from_str::<serde_json::Value>(p)
            .ok()
            .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
            .and_then(|v| v.as_str().map(String::from))
    });

    let Some(tx_hash) = tx_hash else {
        debug!(target:"xlayer_legacy_rpc", "Route to local (invalid/missing tx hash) for method = {method}");
        return inner.call(req).await;
    };

    let service = LegacyRpcRouterService { inner, config, client };
    let resolved = service.get_transaction_block_number(&tx_hash).await;
    route_by_resolved_height(req, service, resolved, &method).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::core::middleware::{Batch, Notification, RpcServiceT};
    use jsonrpsee::types::error::PARSE_ERROR_CODE;
    use jsonrpsee::types::{ErrorObject, Id, Request};
    use jsonrpsee::MethodResponse;
    use serde_json::value::RawValue;
    use std::collections::HashMap;
    use std::future::Future;
    use std::sync::Arc;

    /// Cutoff used across tests (matches the existing lib.rs harness).
    const C: u64 = 1_000_000;
    /// C-1 in hex (below cutoff -> legacy).
    const HEX_BELOW: &str = "0xF423F";
    /// C in hex (at cutoff -> local).
    const HEX_AT: &str = "0xF4240";
    /// C+1 in hex (after cutoff -> local).
    const HEX_AFTER: &str = "0xF4241";
    /// A valid 32-byte hex hash (block or tx).
    const HASH: &str = "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
    /// Dead loopback endpoint: `forward_to_legacy` fails fast with INTERNAL_ERROR
    /// (-32603). We use that error as the observable signal that a request was
    /// routed to legacy — no external network is contacted.
    const DEAD_LEGACY: &str = "http://127.0.0.1:1";
    /// Distinct payload the local (inner) mock returns for unmapped methods.
    const LOCAL_OK: &str = r#"{"result":"LOCAL_OK"}"#;

    /// Inner mock returning a per-method canned response (method-aware), so the
    /// by-tx / by-hash resolution call and the local trace call can each yield a
    /// distinct payload. Unmapped methods return `LOCAL_OK`.
    #[derive(Clone)]
    struct MethodAwareMock {
        responses: HashMap<String, String>,
    }

    impl RpcServiceT for MethodAwareMock {
        type MethodResponse = MethodResponse;
        type NotificationResponse = MethodResponse;
        type BatchResponse = MethodResponse;

        fn call<'a>(
            &self,
            req: Request<'a>,
        ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
            let body = self
                .responses
                .get(req.method_name())
                .cloned()
                .unwrap_or_else(|| LOCAL_OK.to_string());
            Box::pin(async move {
                match serde_json::from_str::<serde_json::Value>(&body) {
                    Ok(json) => {
                        let result = json.get("result").cloned().unwrap_or(serde_json::Value::Null);
                        let payload = jsonrpsee_types::ResponsePayload::success(&result).into();
                        MethodResponse::response(Id::Number(1), payload, usize::MAX)
                    }
                    Err(_) => MethodResponse::error(
                        Id::Number(1),
                        ErrorObject::owned(PARSE_ERROR_CODE, "Parse error", None::<()>),
                    ),
                }
            })
        }

        fn batch<'a>(
            &self,
            _req: Batch<'a>,
        ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
            Box::pin(async {
                MethodResponse::error(
                    Id::Number(1),
                    ErrorObject::owned(-32600, "batch not supported in mock", None::<()>),
                )
            })
        }

        fn notification<'a>(
            &self,
            _n: Notification<'a>,
        ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
            Box::pin(async {
                MethodResponse::error(
                    Id::Number(1),
                    ErrorObject::owned(-32600, "Not implemented", None::<()>),
                )
            })
        }
    }

    fn service_with(
        responses: &[(&str, &str)],
        enabled: bool,
        legacy_endpoint: &str,
    ) -> LegacyRpcRouterService<MethodAwareMock> {
        let responses = responses.iter().map(|(m, r)| (m.to_string(), r.to_string())).collect();
        let config = crate::LegacyRpcRouterConfig {
            enabled,
            legacy_endpoint: legacy_endpoint.to_string(),
            cutoff_block: C,
            timeout: std::time::Duration::from_secs(1),
        };
        LegacyRpcRouterService {
            inner: MethodAwareMock { responses },
            config: Arc::new(config),
            client: reqwest::Client::new(),
        }
    }

    fn req(method: &str, params_json: &str) -> Request<'static> {
        let params_raw = RawValue::from_string(params_json.to_string()).unwrap();
        Request::owned(method.to_string(), Some(params_raw), Id::Number(1))
    }

    /// True when the response is the local mock's distinct success payload.
    fn is_local_ok(resp: &MethodResponse) -> bool {
        resp.is_success()
            && serde_json::from_str::<serde_json::Value>(resp.as_json().get())
                .ok()
                .and_then(|j| j.get("result").and_then(|r| r.as_str().map(String::from)))
                .as_deref()
                == Some("LOCAL_OK")
    }

    /// True when the response is the INTERNAL_ERROR (-32603) produced by
    /// forwarding to the dead legacy endpoint — i.e. the request was routed
    /// to legacy.
    fn is_routed_to_legacy(resp: &MethodResponse) -> bool {
        resp.is_error()
            && serde_json::from_str::<serde_json::Value>(resp.as_json().get())
                .ok()
                .and_then(|j| j.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64()))
                == Some(-32603)
    }

    // ---- pure helpers -------------------------------------------------------

    #[test]
    fn is_special_block_tag_matches_five_tags_only() {
        for tag in ["latest", "pending", "safe", "finalized", "earliest"] {
            assert!(is_special_block_tag(tag), "{tag} should be a special tag");
        }
        for other in ["0x1", "0xF4240", "", "LATEST", "0"] {
            assert!(!is_special_block_tag(other), "{other} should not be special");
        }
    }

    #[test]
    fn raw_block_param_at_reads_string_at_index() {
        assert_eq!(raw_block_param_at(r#"["earliest"]"#, 0).as_deref(), Some("earliest"));
        assert_eq!(raw_block_param_at(r#"[{}, "latest"]"#, 1).as_deref(), Some("latest"));
        assert_eq!(raw_block_param_at(r#"[{}]"#, 1), None); // out of bounds
        assert_eq!(raw_block_param_at(r#"[123]"#, 0), None); // non-string
        assert_eq!(raw_block_param_at(r#"{}"#, 0), None); // not an array
    }

    #[test]
    fn block_param_pos_debug_methods() {
        assert_eq!(block_param_pos("debug_traceCall"), 1);
        assert_eq!(block_param_pos("debug_traceBlockByNumber"), 0);
    }

    #[test]
    fn routing_sets_updated_for_debug_methods() {
        for m in [
            "debug_traceBlockByNumber",
            "debug_traceBlockByHash",
            "debug_traceCall",
            "debug_traceTransaction",
        ] {
            assert!(is_legacy_routable(m), "{m} should be legacy-routable");
        }
        assert!(!need_parse_block("debug_traceBlockByNumber"));
        assert!(!need_parse_block("debug_traceCall"));
        // debug_traceTransaction now has a dedicated deterministic handler.
        assert!(!need_try_local_then_legacy("debug_traceTransaction"));
        // eth_* hash methods keep the try-local-then-legacy behavior.
        assert!(need_try_local_then_legacy("eth_getTransactionByHash"));
    }

    // ---- debug_traceBlockByNumber ------------------------------------------

    #[tokio::test]
    async fn trace_block_by_number_below_cutoff_routes_legacy() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        let resp = svc.call(req("debug_traceBlockByNumber", &format!(r#"["{HEX_BELOW}"]"#))).await;
        assert!(is_routed_to_legacy(&resp));
    }

    #[tokio::test]
    async fn trace_block_by_number_at_or_after_cutoff_routes_local() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        for hex in [HEX_AT, HEX_AFTER] {
            let resp = svc.call(req("debug_traceBlockByNumber", &format!(r#"["{hex}"]"#))).await;
            assert!(is_local_ok(&resp), "hex={hex} should route local");
        }
    }

    #[tokio::test]
    async fn trace_block_by_number_special_tags_route_local() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        for tag in ["latest", "pending", "safe", "finalized", "earliest"] {
            let resp = svc.call(req("debug_traceBlockByNumber", &format!(r#"["{tag}"]"#))).await;
            assert!(is_local_ok(&resp), "tag={tag} (incl earliest) must route local");
        }
    }

    #[tokio::test]
    async fn trace_block_by_number_invalid_param_routes_local() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        let resp = svc.call(req("debug_traceBlockByNumber", r#"["0xZZZ"]"#)).await;
        assert!(!is_routed_to_legacy(&resp), "invalid param must not go to legacy");
    }

    // ---- debug_traceCall ---------------------------------------------------

    #[tokio::test]
    async fn trace_call_below_cutoff_routes_legacy() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        let resp = svc.call(req("debug_traceCall", &format!(r#"[{{}}, "{HEX_BELOW}"]"#))).await;
        assert!(is_routed_to_legacy(&resp));
    }

    #[tokio::test]
    async fn trace_call_at_cutoff_routes_local() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        let resp = svc.call(req("debug_traceCall", &format!(r#"[{{}}, "{HEX_AT}"]"#))).await;
        assert!(is_local_ok(&resp));
    }

    #[tokio::test]
    async fn trace_call_special_tags_and_default_route_local() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        for tag in ["latest", "pending", "safe", "finalized", "earliest"] {
            let resp = svc.call(req("debug_traceCall", &format!(r#"[{{}}, "{tag}"]"#))).await;
            assert!(is_local_ok(&resp), "tag={tag} (incl earliest) must route local");
        }
        // Default block (param absent) -> local.
        let resp = svc.call(req("debug_traceCall", r#"[{}]"#)).await;
        assert!(is_local_ok(&resp), "default block must route local");
    }

    // ---- debug_traceBlockByHash --------------------------------------------

    #[tokio::test]
    async fn trace_block_by_hash_below_cutoff_routes_legacy() {
        let svc = service_with(
            &[("eth_getBlockByHash", r#"{"result":{"number":"0xF423F"}}"#)],
            true,
            DEAD_LEGACY,
        );
        let resp = svc.call(req("debug_traceBlockByHash", &format!(r#"["{HASH}"]"#))).await;
        assert!(is_routed_to_legacy(&resp));
    }

    #[tokio::test]
    async fn trace_block_by_hash_at_cutoff_routes_local() {
        let svc = service_with(
            &[("eth_getBlockByHash", r#"{"result":{"number":"0xF4240"}}"#)],
            true,
            DEAD_LEGACY,
        );
        let resp = svc.call(req("debug_traceBlockByHash", &format!(r#"["{HASH}"]"#))).await;
        assert!(is_local_ok(&resp));
    }

    #[tokio::test]
    async fn trace_block_by_hash_not_found_routes_legacy() {
        let svc = service_with(&[("eth_getBlockByHash", r#"{"result":null}"#)], true, DEAD_LEGACY);
        let resp = svc.call(req("debug_traceBlockByHash", &format!(r#"["{HASH}"]"#))).await;
        assert!(is_routed_to_legacy(&resp));
    }

    #[tokio::test]
    async fn trace_block_by_hash_invalid_hash_routes_local() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        // 66-char string with a non-hex char -> parse_block_param rejects -> local.
        let bad = "0xZ234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        let resp = svc.call(req("debug_traceBlockByHash", &format!(r#"["{bad}"]"#))).await;
        assert!(is_local_ok(&resp));
    }

    // ---- debug_traceTransaction --------------------------------------------

    #[tokio::test]
    async fn trace_transaction_below_cutoff_routes_legacy() {
        let svc = service_with(
            &[("eth_getTransactionByHash", r#"{"result":{"blockNumber":"0xF423F"}}"#)],
            true,
            DEAD_LEGACY,
        );
        let resp = svc.call(req("debug_traceTransaction", &format!(r#"["{HASH}"]"#))).await;
        assert!(is_routed_to_legacy(&resp));
    }

    #[tokio::test]
    async fn trace_transaction_at_cutoff_routes_local() {
        let svc = service_with(
            &[("eth_getTransactionByHash", r#"{"result":{"blockNumber":"0xF4240"}}"#)],
            true,
            DEAD_LEGACY,
        );
        let resp = svc.call(req("debug_traceTransaction", &format!(r#"["{HASH}"]"#))).await;
        assert!(is_local_ok(&resp));
    }

    #[tokio::test]
    async fn trace_transaction_not_found_routes_legacy() {
        let svc =
            service_with(&[("eth_getTransactionByHash", r#"{"result":null}"#)], true, DEAD_LEGACY);
        let resp = svc.call(req("debug_traceTransaction", &format!(r#"["{HASH}"]"#))).await;
        assert!(is_routed_to_legacy(&resp));
    }

    #[tokio::test]
    async fn trace_transaction_invalid_hash_routes_local() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        let resp = svc.call(req("debug_traceTransaction", r#"["0xnothex"]"#)).await;
        assert!(is_local_ok(&resp));
    }

    #[tokio::test]
    async fn get_transaction_block_number_variants() {
        let svc = service_with(
            &[("eth_getTransactionByHash", r#"{"result":{"blockNumber":"0xF4240"}}"#)],
            true,
            DEAD_LEGACY,
        );
        assert_eq!(svc.get_transaction_block_number(HASH).await.unwrap(), Some(C));

        let svc =
            service_with(&[("eth_getTransactionByHash", r#"{"result":null}"#)], true, DEAD_LEGACY);
        assert_eq!(svc.get_transaction_block_number(HASH).await.unwrap(), None);

        // Invalid hash is rejected before any interpolation (JSON-injection guard).
        let svc = service_with(&[], true, DEAD_LEGACY);
        assert!(svc.get_transaction_block_number("0xbad").await.is_err());
    }

    // ---- config disabled ---------------------------------------------------

    #[tokio::test]
    async fn disabled_routes_all_debug_methods_local() {
        let svc = service_with(&[], false, DEAD_LEGACY);
        let cases = [
            ("debug_traceBlockByNumber", format!(r#"["{HEX_BELOW}"]"#)),
            ("debug_traceCall", format!(r#"[{{}}, "{HEX_BELOW}"]"#)),
            ("debug_traceBlockByHash", format!(r#"["{HASH}"]"#)),
            ("debug_traceTransaction", format!(r#"["{HASH}"]"#)),
        ];
        for (m, p) in cases {
            let resp = svc.call(req(m, &p)).await;
            assert!(is_local_ok(&resp), "method={m} must be local when disabled");
        }
    }

    // ---- eth_* earliest regression -----------------------------------------

    #[tokio::test]
    async fn eth_getbalance_earliest_still_routes_legacy() {
        let svc = service_with(&[], true, DEAD_LEGACY);
        let params = r#"["0x1111111111111111111111111111111111111111", "earliest"]"#;
        let resp = svc.call(req("eth_getBalance", params)).await;
        assert!(is_routed_to_legacy(&resp), "eth_* earliest must remain routed to legacy");
        // parse_block_param still maps earliest -> "0" (unchanged by this feature).
        assert_eq!(crate::parse_block_param(params, 1).as_deref(), Some("0"));
    }
}
