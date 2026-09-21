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
            // Read the body as raw bytes and hand it to `build_legacy_response`,
            // which captures the legacy `result`/`error` without walking the
            // response's nested structure. Decoding the full body into a
            // `serde_json::Value` here would cap nesting at the parser's default
            // recursion limit and reject deep-but-valid results; forwarding the
            // raw value avoids that limit and any recursion over response depth.
            Ok(response) => match response.bytes().await {
                Ok(bytes) => build_legacy_response(request_id, &bytes),
                // Failure to read the response body is treated as a legacy parse
                // failure, preserving the existing INTERNAL_ERROR contract.
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

    /// Resolves the block height that contains the given transaction via the
    /// local `eth_getTransactionByHash`, reading `result.blockNumber`.
    ///
    /// Used by `debug_traceTransaction` routing to compare the transaction's
    /// block against the cutoff. Returns:
    /// - `Ok(Some(height))` when the transaction is found with a numeric `blockNumber`
    /// - `Ok(None)` when the transaction is not found locally (`result` is `null`)
    /// - `Err(_)` when the hash is invalid, the response is unparseable, or a found
    ///   transaction has a missing/non-numeric `blockNumber` (e.g. pending tx)
    ///
    /// The hash is validated with `is_valid_32_bytes_string` before interpolation
    /// to prevent JSON injection.
    pub async fn get_transaction_block_number(&self, hash: &str) -> Result<Option<u64>, String>
    where
        S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
    {
        // Validate the tx hash before using it to prevent JSON injection.
        if !is_valid_32_bytes_string(hash) {
            return Err(format!("Invalid tx hash format: {hash}"));
        }

        // Safe now that the hash is validated to contain only 0x + hex.
        let params_str = format!(r#"["{hash}"]"#);
        let method = "eth_getTransactionByHash";
        let params_raw = RawValue::from_string(params_str)
            .map_err(|e| format!("Failed to create JSON params: {e}"))?;
        let id = Id::Number(1);

        let request = Request::owned(method.into(), Some(params_raw), id);
        let res = self.inner.call(request).await;

        let response = serde_json::from_str::<serde_json::Value>(res.as_json().get())
            .map_err(|e| e.to_string())?;

        match response.get("result") {
            // Transaction not found locally.
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(result) => {
                let block_num = result
                    .get("blockNumber")
                    .and_then(|n| n.as_str())
                    .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok());
                match block_num {
                    Some(n) => Ok(Some(n)),
                    // Found but no numeric blockNumber (e.g. pending tx): treat as
                    // a local/internal condition so the request stays local.
                    None => Err("Missing or invalid blockNumber in transaction".to_string()),
                }
            }
        }
    }
}

/// Builds the JSON-RPC response returned to the caller from a raw legacy RPC
/// response body, reusing the caller's original request id.
///
/// The body's top-level fields are captured as unparsed [`RawValue`]s: serde_json
/// scans a `RawValue` iteratively rather than materializing the nested value, so a
/// legacy `result` of arbitrary depth is forwarded byte-for-byte without being
/// bounded by the deserializer's nesting limit and without recursing over
/// attacker-influenced response depth. A present-but-null `result` is retained
/// because its map key still exists.
///
/// Error contract is preserved: a standard JSON-RPC error passes through its
/// original `code`/`message`; a body carrying neither `result` nor `error`, and a
/// body that is not valid JSON, both map to `INTERNAL_ERROR_CODE`.
fn build_legacy_response(request_id: Id<'_>, body: &[u8]) -> MethodResponse {
    match serde_json::from_slice::<std::collections::HashMap<String, Box<RawValue>>>(body) {
        Ok(fields) => {
            if let Some(result) = fields.get("result") {
                // Forward the legacy result verbatim; serializing a RawValue emits
                // its stored text directly, so deep results are not re-walked.
                let payload = jsonrpsee_types::ResponsePayload::success(&**result).into();
                MethodResponse::response(request_id, payload, usize::MAX)
            } else if let Some(error) = fields.get("error") {
                // Error objects are shallow; read code/message from the captured
                // fragment to preserve passthrough of the legacy error.
                let error: serde_json::Value =
                    serde_json::from_str(error.get()).unwrap_or(serde_json::Value::Null);
                let code = error
                    .get("code")
                    .and_then(|c| c.as_i64())
                    .unwrap_or(CALL_EXECUTION_FAILED_CODE as i64) as i32;
                let message =
                    error.get("message").and_then(|m| m.as_str()).unwrap_or("Legacy RPC error");
                MethodResponse::error(request_id, ErrorObject::owned(code, message, None::<()>))
            } else {
                MethodResponse::error(
                    request_id,
                    ErrorObject::owned(INTERNAL_ERROR_CODE, "Invalid legacy response", None::<()>),
                )
            }
        }
        Err(e) => MethodResponse::error(
            request_id,
            ErrorObject::owned(INTERNAL_ERROR_CODE, format!("Legacy parse error: {e}"), None::<()>),
        ),
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

    // ---- build_legacy_response: deep + contract-preserving forwarding -------

    /// Builds a JSON-RPC response body whose `result` is `depth` levels of nested
    /// single-key objects wrapping `leaf`. Assembled iteratively so the test
    /// harness itself never recurses over the depth under test.
    fn deep_object_body(id: u64, depth: usize, leaf: &str) -> String {
        let mut body = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":"#);
        for _ in 0..depth {
            body.push_str(r#"{"a":"#);
        }
        body.push_str(leaf);
        for _ in 0..depth {
            body.push('}');
        }
        body.push('}');
        body
    }

    /// Same as [`deep_object_body`] but nests arrays instead of objects.
    fn deep_array_body(id: u64, depth: usize, leaf: &str) -> String {
        let mut body = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":"#);
        for _ in 0..depth {
            body.push('[');
        }
        body.push_str(leaf);
        for _ in 0..depth {
            body.push(']');
        }
        body.push('}');
        body
    }

    /// The reported failure reproduces around 550 levels; exceed it in tests.
    const DEEP: usize = 600;

    #[test]
    fn deep_object_result_forwarded_without_parse_error() {
        // Legacy body id differs from the caller's id to prove the response is
        // rebuilt with the caller's request id, not the legacy body's id.
        let body = deep_object_body(1, DEEP, r#""DEEP_LEAF_MARKER""#);
        let resp = build_legacy_response(Id::Number(42), body.as_bytes());

        assert!(resp.is_success(), "deep valid object result must be a success response");
        let json = resp.as_json().get();
        assert!(!json.contains("Legacy parse error"), "deep-but-valid JSON is not a parse error");
        // The innermost value survives -> the entire nested structure was forwarded.
        assert!(json.contains("DEEP_LEAF_MARKER"), "innermost value must be forwarded verbatim");
        // Response carries the caller's original request id.
        assert!(json.contains(r#""id":42"#), "response id must equal the caller's request id");
    }

    #[test]
    fn deep_array_result_forwarded_without_parse_error() {
        let body = deep_array_body(1, DEEP, r#""ARRAY_LEAF_MARKER""#);
        let resp = build_legacy_response(Id::Number(1), body.as_bytes());

        assert!(resp.is_success(), "deep valid array result must be a success response");
        let json = resp.as_json().get();
        assert!(!json.contains("Legacy parse error"));
        assert!(json.contains("ARRAY_LEAF_MARKER"));
    }

    #[test]
    fn shallow_object_result_round_trips() {
        // Shallow results stay compatible with pre-fix behavior.
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"number":"0xf4240","hash":"0xabc"}}"#;
        let resp = build_legacy_response(Id::Number(1), body.as_bytes());

        assert!(resp.is_success());
        let v: serde_json::Value = serde_json::from_str(resp.as_json().get()).unwrap();
        assert_eq!(v["result"]["number"], serde_json::json!("0xf4240"));
        assert_eq!(v["result"]["hash"], serde_json::json!("0xabc"));
    }

    #[test]
    fn null_result_is_forwarded_as_null() {
        // A present-but-null result must forward as null, not be mistaken for a
        // response lacking `result`.
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let resp = build_legacy_response(Id::Number(1), body.as_bytes());

        assert!(resp.is_success(), "null result must be a success response");
        let v: serde_json::Value = serde_json::from_str(resp.as_json().get()).unwrap();
        assert!(v.get("result").is_some(), "result field must be present");
        assert!(v["result"].is_null(), "result must remain null");
    }

    #[test]
    fn scalar_results_preserve_type_and_value() {
        let cases = [
            ("true", serde_json::json!(true)),
            ("12345", serde_json::json!(12345)),
            (r#""hello""#, serde_json::json!("hello")),
        ];
        for (raw, expected) in cases {
            let body = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{raw}}}"#);
            let resp = build_legacy_response(Id::Number(1), body.as_bytes());

            assert!(resp.is_success(), "scalar result {raw} must be a success");
            let v: serde_json::Value = serde_json::from_str(resp.as_json().get()).unwrap();
            assert_eq!(v["result"], expected, "scalar {raw} must round-trip by type and value");
        }
    }

    #[test]
    fn legacy_error_is_passed_through() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"execution reverted"}}"#;
        let resp = build_legacy_response(Id::Number(1), body.as_bytes());

        assert!(resp.is_error(), "legacy error must remain an error response");
        let v: serde_json::Value = serde_json::from_str(resp.as_json().get()).unwrap();
        assert_eq!(v["error"]["code"], serde_json::json!(-32000), "error code must pass through");
        assert_eq!(
            v["error"]["message"],
            serde_json::json!("execution reverted"),
            "error message must pass through"
        );
    }

    #[test]
    fn malformed_body_maps_to_internal_error() {
        let resp = build_legacy_response(Id::Number(1), b"{ not valid json ");

        assert!(resp.is_error(), "malformed body must be an error, service stays up");
        let v: serde_json::Value = serde_json::from_str(resp.as_json().get()).unwrap();
        assert_eq!(v["error"]["code"], serde_json::json!(-32603));
        assert!(
            v["error"]["message"].as_str().unwrap().contains("Legacy parse error"),
            "malformed body must report a legacy parse failure"
        );
    }

    #[test]
    fn response_without_result_or_error_maps_to_invalid() {
        let body = r#"{"jsonrpc":"2.0","id":1}"#;
        let resp = build_legacy_response(Id::Number(1), body.as_bytes());

        assert!(resp.is_error());
        let v: serde_json::Value = serde_json::from_str(resp.as_json().get()).unwrap();
        assert_eq!(v["error"]["code"], serde_json::json!(-32603));
        assert_eq!(v["error"]["message"], serde_json::json!("Invalid legacy response"));
    }
}
