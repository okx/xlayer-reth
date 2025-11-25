pub mod layer;
pub mod service;

use jsonrpsee::{
    types::{ErrorObject, Request},
    MethodResponse,
};
use reqwest::Client;
use std::sync::Arc;

/// Configuration for legacy RPC routing
#[derive(Clone, Debug)]
pub struct LegacyRpcRouterConfig {
    pub legacy_endpoint: String,
    pub cutoff_block: u64,
    pub timeout: std::time::Duration,
}

/// XLayer legacy routing service
#[derive(Clone)]
pub struct LegacyRpcRouterService<S> {
    inner: S,
    config: Arc<LegacyRpcRouterConfig>,
    client: Client,
}

impl<S> LegacyRpcRouterService<S> {
    /// Extract block number from request params based on method
    fn extract_block_number(&self, req: &Request<'_>) -> Option<u64> {
        let p = req.params();
        let params = p.as_str()?;
        let method = req.method_name();

        // Parse based on method signature
        // e.g., eth_getBlockByNumber has block as first param
        // eth_getBalance has block as second param
        match method {
            "eth_getBlockByNumber" | "eth_getBlockTransactionCountByNumber" => {
                self.parse_block_param(params, 0)
            }
            "eth_getBalance"
            | "eth_getCode"
            | "eth_getStorageAt"
            | "eth_getTransactionCount"
            | "eth_call" => self.parse_block_param(params, 1),
            _ => None,
        }
    }

    fn parse_block_param(&self, params: &str, index: usize) -> Option<u64> {
        let parsed: serde_json::Value = serde_json::from_str(params).ok()?;
        let arr = parsed.as_array()?;
        let block_param = arr.get(index)?;

        match block_param {
            serde_json::Value::String(s) => {
                // Handle "latest", "pending", "earliest", or hex number
                if s == "latest" || s == "pending" {
                    None // Don't route to legacy
                } else if s == "earliest" {
                    Some(0)
                } else if let Some(hex) = s.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16).ok()
                } else {
                    None
                }
            }
            serde_json::Value::Number(n) => n.as_u64(),
            _ => None,
        }
    }

    fn should_route_to_legacy(&self, req: &Request<'_>) -> bool {
        // TODO: We may want to filter certain methods.
        let _method = req.method_name();

        // Check block number against cutoff
        if let Some(block_num) = self.extract_block_number(req) {
            return block_num < self.config.cutoff_block;
        }

        false
    }

    async fn forward_to_legacy(&self, req: Request<'_>) -> MethodResponse {
        let request_id = req.id().clone();

        // Build JSON-RPC request body
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": req.method_name(),
            "params": req.params().as_str()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .unwrap_or(serde_json::Value::Null),
            "id": 1
        });

        match self.client.post(&self.config.legacy_endpoint).json(&body).send().await {
            Ok(response) => match response.json::<serde_json::Value>().await {
                Ok(json) => {
                    if let Some(result) = json.get("result") {
                        let payload = jsonrpsee_types::ResponsePayload::success(result).into();
                        MethodResponse::response(request_id, payload, usize::MAX)
                    } else if let Some(error) = json.get("error") {
                        let code =
                            error.get("code").and_then(|c| c.as_i64()).unwrap_or(-32000) as i32;
                        let message = error
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("Legacy RPC error");
                        MethodResponse::error(
                            request_id,
                            ErrorObject::owned(code, message, None::<()>),
                        )
                    } else {
                        MethodResponse::error(
                            request_id,
                            ErrorObject::owned(-32603, "Invalid legacy response", None::<()>),
                        )
                    }
                }
                Err(e) => MethodResponse::error(
                    request_id,
                    ErrorObject::owned(-32603, format!("Legacy parse error: {e}"), None::<()>),
                ),
            },
            Err(e) => {
                tracing::error!(target: "rpc::legacy", error = %e, "Legacy RPC request failed");
                MethodResponse::error(
                    request_id,
                    ErrorObject::owned(-32603, format!("Legacy RPC error: {e}"), None::<()>),
                )
            }
        }
    }
}
