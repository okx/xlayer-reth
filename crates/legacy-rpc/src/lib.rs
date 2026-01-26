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
    ) -> Result<Option<u64>, String>
    where
        S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
    {
        // Validate the block hash before using it to prevent JSON injection
        if !is_valid_32_bytes_string(block_hash) {
            return Err(format!("Invalid block hash format: {block_hash}"));
        }

        // Construct the parameters JSON string - now safe because we validated the hash
        let params_str = format!(r#"["{block_hash}", {full_transactions}]"#);

        let method = "eth_getBlockByHash";
        // Replace expect() with proper error propagation using ?
        let params_raw = RawValue::from_string(params_str)
            .map_err(|e| format!("Failed to create JSON params: {e}"))?;
        let id = Id::Number(1);

        // Create request using borrowed data
        let request = Request::owned(method.into(), Some(params_raw), id);

        // Call inner service
        let res = self.inner.call(request).await;

        let response = serde_json::from_str::<serde_json::Value>(res.as_json().get())
            .map_err(|e| e.to_string())?;
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

/// Validates that a string is a valid 32-byte hexadecimal string (block hash or similar).
/// Checks that the string:
/// - Has the "0x" prefix
/// - Is exactly 66 characters long (0x + 64 hex chars = 32 bytes)
/// - Contains only valid hexadecimal digits after the prefix
///
/// This function prevents JSON injection attacks by ensuring all characters are valid hex.
#[inline]
pub fn is_valid_32_bytes_string(hex: &str) -> bool {
    // Must start with 0x and be exactly 66 characters
    if !hex.starts_with("0x") || hex.len() != 66 {
        return false;
    }

    // Check if all characters after 0x are valid hex - this prevents JSON injection
    hex[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Deprecated: Use is_valid_32_bytes_string instead.
/// This function only checks length and prefix, not hex validity.
#[inline]
#[deprecated(since = "0.1.0", note = "Use is_valid_32_bytes_string for proper validation")]
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
                        // Validate it's a proper 32-byte hex string to prevent JSON injection
                        if is_valid_32_bytes_string(hex) {
                            Some(hex.into())
                        } else {
                            // Invalid hex characters - reject it
                            None
                        }
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
                // Validate block hash to prevent JSON injection
                if is_valid_32_bytes_string(hash) {
                    Some(hash.clone())
                } else {
                    None
                }
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
                match serde_json::from_str::<serde_json::Value>(&response) {
                    Ok(json) => {
                        let result = json.get("result").cloned().unwrap_or(serde_json::Value::Null);
                        let payload = jsonrpsee_types::ResponsePayload::success(&result).into();
                        MethodResponse::response(Id::Number(1), payload, usize::MAX)
                    }
                    Err(_) => {
                        // Return error response for invalid JSON
                        MethodResponse::error(
                            Id::Number(1),
                            jsonrpsee::types::ErrorObjectOwned::owned(
                                jsonrpsee::types::error::PARSE_ERROR_CODE,
                                "Parse error",
                                None::<()>,
                            ),
                        )
                    }
                }
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

    #[tokio::test]
    async fn test_parse_block_param_rejects_json_injection_after_fix() {
        // This test verifies that the fix prevents JSON injection attacks.
        // After the fix, parse_block_param uses is_valid_32_bytes_string which
        // validates that all characters are valid hexadecimal digits.

        // Create a 66-character malicious string with a quote in the middle
        let malicious_hash = "0x1234567890abcdef1234567890abcdef12345\"7890abcdef1234567890abcdef";

        // Attacker provides valid JSON params with the malicious hash
        let params_json = serde_json::to_string(&vec![malicious_hash]).unwrap();

        // After fix: parse_block_param now rejects the malicious hash
        let parsed_block = parse_block_param(&params_json, 0);
        assert!(parsed_block.is_none(), "parse_block_param should reject invalid hex");

        // Verify is_valid_32_bytes_string correctly rejects it
        assert!(!is_valid_32_bytes_string(malicious_hash));

        // If somehow a malicious hash gets through, call_eth_get_block_by_hash
        // now validates the input and returns an error instead of panicking
        let service = create_test_service(r#"{"jsonrpc":"2.0","id":1,"result":null}"#);
        let result = service.call_eth_get_block_by_hash(malicious_hash, false).await;

        // Should return an error, not panic
        assert!(result.is_err(), "call_eth_get_block_by_hash should return error for invalid hash");
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Invalid block hash format"));
    }

    #[test]
    fn test_is_valid_32_bytes_string_security_validation() {
        // Valid block hash - should accept
        let valid_hash = "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        assert!(is_valid_32_bytes_string(valid_hash));

        // Test various attack vectors that should be rejected

        // 1. JSON injection with quote
        let with_quote = "0x1234567890abcdef1234567890abcdef12345\"7890abcdef1234567890abcdef";
        assert!(!is_valid_32_bytes_string(with_quote));

        // 2. JSON injection with backslash
        let with_backslash = "0x1234567890abcdef1234567890abcdef1234567\\90abcdef1234567890abcdef";
        assert!(!is_valid_32_bytes_string(with_backslash));

        // 3. Non-hex characters
        let with_non_hex = "0xGGGG567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        assert!(!is_valid_32_bytes_string(with_non_hex));

        // 4. SQL injection attempt
        let with_sql = "0x1234567890abcdef1234567890abcdef12345';DROP TABLE users;--cdef";
        assert!(!is_valid_32_bytes_string(with_sql));

        // 5. Wrong length
        let too_short = "0x1234567890abcdef";
        assert!(!is_valid_32_bytes_string(too_short));

        let too_long = "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef00";
        assert!(!is_valid_32_bytes_string(too_long));

        // 6. Missing 0x prefix
        let no_prefix = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        assert!(!is_valid_32_bytes_string(no_prefix));

        // 7. Unicode characters
        let with_unicode = "0x1234567890abcdef1234567890abcdef12345→7890abcdef1234567890abcdef";
        assert!(!is_valid_32_bytes_string(with_unicode));
    }

    #[tokio::test]
    async fn test_call_eth_get_block_by_hash_success() {
        let response = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "number": "0xf4240",
                "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                "parentHash": "0x0000000000000000000000000000000000000000000000000000000000000000"
            }
        }"#;

        let service = create_test_service(response);
        let result = service
            .call_eth_get_block_by_hash(
                "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                false,
            )
            .await;

        assert!(result.is_ok());
        let block_num = result.unwrap();
        assert!(block_num.is_some());
        assert_eq!(block_num, Some(1_000_000));
    }

    #[tokio::test]
    async fn test_call_eth_get_block_by_hash_not_found() {
        let response = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": null
        }"#;

        let service = create_test_service(response);
        let result = service
            .call_eth_get_block_by_hash(
                "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                false,
            )
            .await;

        assert!(result.is_ok());
        let block_num = result.unwrap();
        assert!(block_num.is_none());
    }

    #[tokio::test]
    async fn test_call_eth_get_block_by_hash_malformed_number() {
        // Test with response that has a malformed block number
        let response = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "number": "invalid_hex",
                "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                "parentHash": "0x0000000000000000000000000000000000000000000000000000000000000000"
            }
        }"#;

        let service = create_test_service(response);
        let result = service
            .call_eth_get_block_by_hash(
                "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                false,
            )
            .await;

        // This should succeed but return None because the hex parsing fails
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
