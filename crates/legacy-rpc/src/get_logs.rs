//! Handles logic for deciding how to route for `eth_getLogs`.
//!
//! We will use (from_block, to_block) params to decide such
//! routing logic.
//!
//! 1. Pure Legacy
//!    Condition: to_block < cutoff_block
//!    Timeline:  [====== Legacy ======][cutoff][====== Local ======]
//!    Filter:    [from --- to]
//!
//! 2. Pure local
//!    Condition: from_block >= cutoff_block
//!    Timeline:  [====== Legacy ======][cutoff][====== Local ======]
//!    Filter:                                   [from --- to]
//!
//! 3. Hybrid
//!    Condition: from_block < cutoff_block && to_block >= cutoff_block
//!    Timeline:  [====== Legacy ======][cutoff][====== Local ======]
//!    Filter:    [from -------------- across -------------- to]
//!    Results will be sorted (eg. block num, txn index, log index).
//!
//! Special Cases
//! from_block: earliest
//!     These get converted to 0
//! to_block: latest/pending/finalized/safe
//!     These get converted to u64::MAX
use crate::LegacyRpcRouterService;
use jsonrpsee::MethodResponse;
use jsonrpsee_types::{Id, Request};
use serde_json::value::RawValue;
use tracing::debug;

/// Parse a block number string to u64
/// Returns None for "latest", "pending", "safe", "finalized"
/// Returns Some(0) for "earliest"
/// Returns Some(block_num) for hex block numbers
#[inline]
fn parse_block_number_string(s: &str) -> Option<u64> {
    match s {
        // Don't route these to legacy (use current chain state)
        "latest" | "pending" | "safe" | "finalized" => Some(u64::MAX),

        // Route to legacy
        "earliest" => Some(0),

        // Parse hex block number
        hex if hex.starts_with("0x") => u64::from_str_radix(&hex[2..], 16).ok(),

        _ => None,
    }
}

/// Parse eth_getLogs params to extract fromBlock and toBlock
/// Returns (from_block, to_block) as Option<(Option<u64>, Option<u64>)>
///
/// - Some((from, to)) - both blocks specified
/// - Some((from, u64::MAX)) - only fromBlock specified
/// - Some((u64::MAX, to)) - only toBlock specified
/// - Some((u64::MAX, u64::MAX)) - neither specified (will use latest)
/// - None - invalid params
#[inline]
fn parse_eth_get_logs_params(params: &str) -> Option<(u64, u64)> {
    let parsed: serde_json::Value = serde_json::from_str(params).ok()?;
    let arr = parsed.as_array()?;

    // eth_getLogs takes a single filter object parameter
    if arr.is_empty() {
        return None;
    }

    let filter = arr.first()?;
    let filter_obj = filter.as_object()?;

    // Parse fromBlock
    let from_block = filter_obj
        .get("fromBlock")
        .and_then(|v| v.as_str())
        .and_then(parse_block_number_string)
        .unwrap_or(u64::MAX);

    // Parse toBlock
    let to_block = filter_obj
        .get("toBlock")
        .and_then(|v| v.as_str())
        .and_then(parse_block_number_string)
        .unwrap_or(u64::MAX);

    Some((from_block, to_block))
}

/// Modify eth_getLogs request to use custom fromBlock and toBlock
/// Returns a new Request with modified parameters
fn modify_eth_get_logs_params<'a>(
    original_req: &Request<'a>,
    from_block: Option<u64>,
    to_block: Option<u64>,
) -> Option<Request<'a>> {
    let _p = original_req.params();
    let params_str = _p.as_str()?;
    let mut parsed: serde_json::Value = serde_json::from_str(params_str).ok()?;

    let arr = parsed.as_array_mut()?;
    if arr.is_empty() {
        return None;
    }

    let filter = arr.get_mut(0)?;
    let filter_obj = filter.as_object_mut()?;

    // Modify fromBlock if provided
    if let Some(from) = from_block {
        filter_obj
            .insert("fromBlock".to_string(), serde_json::Value::String(format!("0x{from:x}")));
    }

    // Modify toBlock if provided
    if let Some(to) = to_block {
        filter_obj.insert("toBlock".to_string(), serde_json::Value::String(format!("0x{to:x}")));
    }

    // Serialize back to string
    let new_params_str = serde_json::to_string(&parsed).ok()?;

    // Create new RawValue
    let params_raw = RawValue::from_string(new_params_str).ok()?;

    // Create new Request with modified params
    Some(Request::owned(
        original_req.method_name().to_string(),
        Some(params_raw),
        original_req.id(),
    ))
}

/// Merge two eth_getLogs responses
fn merge_eth_get_logs_responses(
    legacy_response: MethodResponse,
    local_response: MethodResponse,
    request_id: Id,
) -> MethodResponse {
    // Parse both responses
    let legacy_json = legacy_response.as_json().get();
    let local_json = local_response.as_json().get();

    let legacy_parsed: serde_json::Value = match serde_json::from_str(legacy_json) {
        Ok(v) => v,
        Err(_) => return legacy_response, // Fallback to legacy on error
    };

    let local_parsed: serde_json::Value = match serde_json::from_str(local_json) {
        Ok(v) => v,
        Err(_) => return local_response, // Fallback to local on error
    };

    // Extract results arrays
    let legacy_result =
        legacy_parsed.get("result").and_then(|r| r.as_array()).cloned().unwrap_or_default();

    let local_result =
        local_parsed.get("result").and_then(|r| r.as_array()).cloned().unwrap_or_default();

    // Merge the arrays
    let mut merged_logs = legacy_result;
    merged_logs.extend(local_result);

    // Sort by block number, then transaction index, then log index
    merged_logs.sort_by(|a, b| {
        // Compare block numbers
        let block_a = a
            .get("blockNumber")
            .and_then(|v| v.as_str())
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);

        let block_b = b
            .get("blockNumber")
            .and_then(|v| v.as_str())
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);

        // Separated by cutoff point, won't need to compare equal block numbers
        block_a.cmp(&block_b)
    });

    // Create merged response
    let merged_result = serde_json::Value::Array(merged_logs);
    let payload = jsonrpsee_types::ResponsePayload::success(&merged_result).into();

    MethodResponse::response(request_id, payload, usize::MAX)
}

/// Handle eth_getLogs routing logic.
///
/// Determines whether to route to legacy, local, or use hybrid approach
/// based on the block range in the request.
pub(crate) async fn handle_eth_get_logs<S>(
    req: Request<'_>,
    client: reqwest::Client,
    config: std::sync::Arc<crate::LegacyRpcRouterConfig>,
    inner: S,
) -> MethodResponse
where
    S: jsonrpsee::server::middleware::rpc::RpcServiceT<MethodResponse = MethodResponse>
        + Send
        + Sync
        + Clone
        + 'static,
{
    let service = LegacyRpcRouterService { inner: inner.clone(), config, client };
    let _p = req.params(); // keeps compiler quiet
    let params = _p.as_str().unwrap();

    let cutoff_block = service.config.cutoff_block;
    if let Some((from_block, to_block)) = parse_eth_get_logs_params(params) {
        if to_block < cutoff_block {
            debug!(
                target:"xlayer_legacy_rpc",
                "eth_getLogs pure legacy routing (from_block = {}, to_block = {})",
                from_block, to_block
            );
            // Pure legacy
            return service.forward_to_legacy(req).await;
        } else if from_block >= cutoff_block {
            debug!(
                target:"xlayer_legacy_rpc",
                "eth_getLogs pure local routing (from_block = {}, to_block = {})",
                from_block, to_block
            );
            // Pure local
            return inner.call(req).await;
        } else {
            // Hybrid: split into two requests

            // 1. Legacy request: fromBlock to cutoff-1
            let legacy_req =
                modify_eth_get_logs_params(&req, Some(from_block), Some(cutoff_block - 1));

            // 2. Local request: cutoff to toBlock
            let local_req = modify_eth_get_logs_params(&req, Some(cutoff_block), Some(to_block));

            if let (Some(legacy_req), Some(local_req)) = (legacy_req, local_req) {
                debug!(
                    target:"xlayer_legacy_rpc",
                    "eth_getLogs hybrid routing (from_block = {}, {}) and ({}, to_block = {})",
                    from_block,
                    cutoff_block - 1,
                    cutoff_block,
                    to_block
                );

                // Call both and merge results
                let (legacy_response, local_response) =
                    tokio::join!(async { service.forward_to_legacy(legacy_req).await }, async {
                        inner.call(local_req).await
                    });

                // Merge the results
                return merge_eth_get_logs_responses(legacy_response, local_response, req.id());
            }

            debug!(target:"xlayer_legacy_rpc", "No legacy routing for method = eth_getLogs");

            // Fallback to normal if modification failed
            return inner.call(req).await;
        }
    }

    // If parsing fails, use normal routing
    inner.call(req).await
}

#[cfg(test)]
mod tests {
    use jsonrpsee::MethodResponse;
    use jsonrpsee_types::{Id, Request};
    use serde_json::value::RawValue;

    #[test]
    fn test_parse_eth_get_logs_params_both_blocks() {
        let cases = [
            (r#"[{"fromBlock":"0x1","toBlock":"latest"}]"#, Some((1, u64::MAX))),
            (r#"[{"fromBlock":"0x1","toBlock":"earliest"}]"#, Some((1, 0))),
            (r#"[{"fromBlock":"latest","toBlock":"0x64"}]"#, Some((u64::MAX, 100))),
            (r#"[{"fromBlock":"earliest","toBlock":"0x64"}]"#, Some((0, 100))),
            (r#"[{"fromBlock":"0x1","toBlock":"0x64"}]"#, Some((1, 100))),
            (r#"[{"fromBlock":"0x1"}]"#, Some((1, u64::MAX))),
            (r#"[{"toBlock":"0x64"}]"#, Some((u64::MAX, 100))),
            (r#"[{}]"#, Some((u64::MAX, u64::MAX))),
        ];

        for c in cases {
            let params = c.0;
            let result = super::parse_eth_get_logs_params(params);
            assert_eq!(result, c.1);
        }
    }

    #[test]
    fn test_modify_eth_get_logs_params_both_blocks() {
        // Original request
        let params = r#"[{"fromBlock":"0x1","toBlock":"0x64"}]"#;
        let params_raw = RawValue::from_string(params.to_string()).unwrap();
        let request = Request::owned("eth_getLogs".to_string(), Some(params_raw), Id::Number(1));

        // Modify to use custom blocks
        let modified = super::modify_eth_get_logs_params(&request, Some(100), Some(200));

        assert!(modified.is_some());
        let modified_req = modified.unwrap();

        // Parse the modified params
        let _p = modified_req.params();
        let modified_params = _p.as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(modified_params).unwrap();
        let filter = parsed.as_array().unwrap().get(0).unwrap();

        assert_eq!(filter.get("fromBlock").and_then(|v| v.as_str()), Some("0x64"));
        assert_eq!(filter.get("toBlock").and_then(|v| v.as_str()), Some("0xc8"));
    }

    #[test]
    fn test_merge_eth_get_logs_responses_both_have_logs() {
        // Create legacy response with 2 logs
        let legacy_json = r#"{
              "jsonrpc": "2.0",
              "id": 1,
              "result": [
                  {
                      "address": "0x1234567890123456789012345678901234567890",
                      "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"],
                      "data": "0x0000000000000000000000000000000000000000000000000000000000000001",
                      "blockNumber": "0x63",
                      "transactionHash": "0xaaa",
                      "logIndex": "0x0"
                  },
                  {
                      "address": "0x1234567890123456789012345678901234567890",
                      "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"],
                      "data": "0x0000000000000000000000000000000000000000000000000000000000000002",
                      "blockNumber": "0x64",
                      "transactionHash": "0xbbb",
                      "logIndex": "0x0"
                  }
              ]
          }"#;

        // Create local response with 2 logs
        let local_json = r#"{
              "jsonrpc": "2.0",
              "id": 1,
              "result": [
                  {
                      "address": "0x1234567890123456789012345678901234567890",
                      "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"],
                      "data": "0x0000000000000000000000000000000000000000000000000000000000000003",
                      "blockNumber": "0x65",
                      "transactionHash": "0xccc",
                      "logIndex": "0x0"
                  },
                  {
                      "address": "0x1234567890123456789012345678901234567890",
                      "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"],
                      "data": "0x0000000000000000000000000000000000000000000000000000000000000004",
                      "blockNumber": "0x66",
                      "transactionHash": "0xddd",
                      "logIndex": "0x0"
                  }
              ]
          }"#;

        // Create MethodResponses
        let legacy_value: serde_json::Value = serde_json::from_str(legacy_json).unwrap();
        let local_value: serde_json::Value = serde_json::from_str(local_json).unwrap();

        let legacy_payload =
            jsonrpsee_types::ResponsePayload::success(legacy_value.get("result").unwrap()).into();
        let local_payload =
            jsonrpsee_types::ResponsePayload::success(local_value.get("result").unwrap()).into();

        let legacy_response = MethodResponse::response(Id::Number(1), legacy_payload, usize::MAX);
        let local_response = MethodResponse::response(Id::Number(1), local_payload, usize::MAX);

        // Merge
        let merged =
            super::merge_eth_get_logs_responses(legacy_response, local_response, Id::Number(1));

        // Parse merged response
        let merged_json = merged.as_json().get();
        let merged_parsed: serde_json::Value = serde_json::from_str(merged_json).unwrap();
        let result = merged_parsed.get("result").unwrap().as_array().unwrap();

        // Should have 4 logs total (2 from legacy + 2 from local)
        assert_eq!(result.len(), 4);

        // Check that logs are in order (sorted)
        assert_eq!(result[0].get("blockNumber").unwrap().as_str(), Some("0x63"));
        assert_eq!(result[1].get("blockNumber").unwrap().as_str(), Some("0x64"));
        assert_eq!(result[2].get("blockNumber").unwrap().as_str(), Some("0x65"));
        assert_eq!(result[3].get("blockNumber").unwrap().as_str(), Some("0x66"));
    }
}
