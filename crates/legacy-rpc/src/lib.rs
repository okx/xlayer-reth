mod conditions;
pub mod layer;
pub mod service;

use std::sync::Arc;

use jsonrpsee::{
    types::{ErrorObject, Request},
    MethodResponse,
};
use reqwest::Client;

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

/// Configuration for legacy RPC routing
#[derive(Clone, Debug)]
pub struct LegacyRpcRouterConfig {
    pub enabled: bool,
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

#[inline]
pub fn is_block_hash(hex: &str) -> bool {
    if hex.starts_with("0x") {
        // Check if it's a block hash (66 chars) or block number
        if hex.len() == 66 {
            true
        } else {
            false
        }
    } else {
        false
    }
}

/// Handles latest, pending, hash, hex number etc
#[inline]
pub(crate) fn parse_block_param(params: &str, index: usize, genesis_num: u64) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(params).ok()?;
    let arr = parsed.as_array()?;

    // Some params are optional.
    if index >= arr.len() {
        return None;
    }

    let block_param = arr.get(index)?;

    match block_param {
        serde_json::Value::String(s) => {
            match s.as_str() {
                // Don't route these to legacy (use current chain state)
                "latest" | "pending" | "safe" | "finalized" => None,

                // Route to legacy (set to genesis)
                "earliest" => Some(genesis_num.to_string()),

                // Parse hex block number/hash
                hex if hex.starts_with("0x") => {
                    // Check if it's a block hash (66 chars) or block number
                    if hex.len() == 66 {
                        // This is a block hash, not a number
                        // Return None to indicate can't extract number
                        Some(hex.into())
                    } else {
                        // Parse as block number
                        u64::from_str_radix(&hex[2..], 16).ok().map(|n| n.to_string())
                    }
                }

                _ => None,
            }
        }
        // decimal number not handled...
        // serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
}
