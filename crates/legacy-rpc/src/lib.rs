pub mod get_logs;
pub mod layer;
pub mod service;

use std::sync::Arc;

use jsonrpsee::{
    core::middleware::RpcServiceT,
    types::{
        error::{CALL_EXECUTION_FAILED_CODE, INTERNAL_ERROR_CODE},
        ErrorObject, Request,
    },
    MethodResponse,
};
use jsonrpsee_types::Id;
use reqwest::Client;
use serde_json::value::RawValue;

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
                        let code = error
                            .get("code")
                            .and_then(|c| c.as_i64())
                            .unwrap_or(CALL_EXECUTION_FAILED_CODE as i64)
                            as i32;
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
                            ErrorObject::owned(
                                INTERNAL_ERROR_CODE,
                                "Invalid legacy response",
                                None::<()>,
                            ),
                        )
                    }
                }
                Err(e) => MethodResponse::error(
                    request_id,
                    ErrorObject::owned(
                        INTERNAL_ERROR_CODE,
                        format!("Legacy parse error: {e}"),
                        None::<()>,
                    ),
                ),
            },
            Err(e) => {
                tracing::error!(target: "rpc::legacy", error = %e, "Legacy RPC request failed");
                MethodResponse::error(
                    request_id,
                    ErrorObject::owned(
                        INTERNAL_ERROR_CODE,
                        format!("Legacy RPC error: {e}"),
                        None::<()>,
                    ),
                )
            }
        }
    }

    pub async fn call_eth_get_block_by_hash(
        &self,
        block_hash: &str,
        full_transactions: bool,
    ) -> Result<Option<u64>, serde_json::Error>
    where
        S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
    {
        // Construct the parameters JSON string
        let params_str = format!(r#"["{block_hash}", {full_transactions}]"#);

        let method = "eth_getBlockByHash";
        let params_raw = RawValue::from_string(params_str).expect("Valid JSON params");
        let id = Id::Number(1);

        // Create request using borrowed data
        let request = Request::owned(method.into(), Some(params_raw), id);

        // Call inner service
        let res = self.inner.call(request).await;

        let response = serde_json::from_str::<serde_json::Value>(res.as_json().get())?;
        let block_num = response
            .get("result")
            .and_then(|result| result.get("number"))
            .and_then(|n| n.as_str())
            .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok());

        Ok(block_num)
    }

    pub async fn get_transaction_by_hash(
        &self,
        hash: &str,
    ) -> Result<Option<String>, serde_json::Error>
    where
        S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
    {
        // Construct the parameters JSON string
        let params_str = format!(r#"["{hash}"]"#);
        let method = "eth_getTransactionByHash";
        let id = Id::Number(1);

        // Convert params string to RawValue
        let params_raw = match RawValue::from_string(params_str) {
            Ok(raw) => raw,
            Err(_) => return Ok(None),
        };

        let request = Request::owned(method.to_string(), Some(params_raw), id);

        let res = self.inner.call(request).await;

        let response = serde_json::from_str::<serde_json::Value>(res.as_json().get())?;
        let txhash = response
            .get("result")
            .and_then(|result| result.get("hash"))
            .and_then(|v| v.as_str().map(String::from));

        Ok(txhash)
    }
}

#[inline]
pub fn is_block_hash(hex: &str) -> bool {
    if hex.starts_with("0x") {
        // Check if it's a block hash (66 chars) or block number
        hex.len() == 66
    } else {
        false
    }
}

/// Handles latest, pending, hash, hex number etc
#[inline]
pub(crate) fn parse_block_param(params: &str, index: usize) -> Option<String> {
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

                // Route to legacy (not genesis, as local has no data)
                "earliest" => Some("0".into()),

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
        // Handle object format: {"blockHash": "0x..."} or {"blockNumber": "0x..."}
        serde_json::Value::Object(obj) => {
            if let Some(serde_json::Value::String(hash)) = obj.get("blockHash") {
                Some(hash.clone())
            } else if let Some(serde_json::Value::String(num)) = obj.get("blockNumber") {
                // Handle blockNumber in object format
                if let Some(stripped) = num.strip_prefix("0x") {
                    u64::from_str_radix(stripped, 16).ok().map(|n| n.to_string())
                } else {
                    Some(num.clone())
                }
            } else {
                None
            }
        }
        // decimal number not handled...
        // serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

#[inline]
fn parse_tx_hash_param(params: &str, index: usize) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(params).ok()?;
    let arr = parsed.as_array()?;

    if index >= arr.len() {
        return None;
    }

    let tx_hash = arr.get(index)?;

    match tx_hash {
        serde_json::Value::String(s) => {
            // Validate it's a valid transaction hash (0x + 64 hex chars = 66 total)
            if s.starts_with("0x") && s.len() == 66 {
                Some(s.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::core::middleware::RpcServiceT;
    use jsonrpsee::types::{Id, Request};
    use jsonrpsee::MethodResponse;
    use std::future::Future;
    use std::sync::Arc;

    // Mock RPC service that returns predefined responses
    #[derive(Clone)]
    struct MockRpcService {
        response: String,
    }

    impl RpcServiceT for MockRpcService {
        type MethodResponse = MethodResponse;
        type NotificationResponse = MethodResponse;
        type BatchResponse = Vec<MethodResponse>;

        fn call<'a>(
            &self,
            _req: Request<'a>,
        ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
            let response = self.response.clone();
            Box::pin(async move {
                // Parse the response JSON and create a MethodResponse
                let json: serde_json::Value = serde_json::from_str(&response).unwrap();
                let result = json.get("result").cloned().unwrap_or(serde_json::Value::Null);

                let payload = jsonrpsee_types::ResponsePayload::success(&result).into();
                MethodResponse::response(Id::Number(1), payload, usize::MAX)
            })
        }

        fn batch<'a>(
            &self,
            _req: jsonrpsee::core::middleware::Batch<'a>,
        ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
            Box::pin(async { vec![] })
        }

        fn notification<'a>(
            &self,
            _n: jsonrpsee::core::middleware::Notification<'a>,
        ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
            Box::pin(async {
                MethodResponse::error(
                    Id::Number(1),
                    jsonrpsee::types::ErrorObjectOwned::owned(
                        -32600,
                        "Not implemented",
                        None::<()>,
                    ),
                )
            })
        }
    }

    fn create_test_service(response: &str) -> LegacyRpcRouterService<MockRpcService> {
        let config = LegacyRpcRouterConfig {
            enabled: true,
            legacy_endpoint: "https://testrpc.xlayer.tech/terigon".to_string(),
            cutoff_block: 1_000_000,
            timeout: std::time::Duration::from_secs(10),
        };

        let mock_service = MockRpcService { response: response.to_string() };

        LegacyRpcRouterService {
            inner: mock_service,
            config: Arc::new(config),
            client: reqwest::Client::new(),
        }
    }

    #[tokio::test]
    async fn test_get_transaction_by_hash_found() {
        let response = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "blockHash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                "blockNumber": "0xf4240",
                "hash": "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                "from": "0x1111111111111111111111111111111111111111",
                "to": "0x2222222222222222222222222222222222222222"
            }
        }"#;

        let service = create_test_service(response);
        let tx = service
            .get_transaction_by_hash(
                "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
            )
            .await;

        assert!(tx.is_ok());
        let tx = tx.unwrap();
        assert!(tx.is_some());
        assert_eq!(
            tx,
            Some("0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890".into())
        );
    }

    #[test]
    fn test_parse_tx_hash_param_valid() {
        let cases = [
            (
                r#"["0xcf2563e07aa150208b2e9b30655d710c339c83263b8ec185f813ea572aadac18"]"#,
                Some("0xcf2563e07aa150208b2e9b30655d710c339c83263b8ec185f813ea572aadac18"),
            ),
            (r#"["cf2563e07aa150208b2e9b30655d710c339c83263b8ec185f813ea572aadac18"]"#, None),
            (r#"["0xcf2563e07aa150208b2e9b30655d710c339c83263b8ec185f813ea572aadac1"]"#, None),
        ];

        for c in cases {
            let params = c.0;
            let result = super::parse_tx_hash_param(params, 0);
            assert_eq!(result, c.1.map(String::from));
        }
    }
}
