//! RCS REST client (contract §2). The filter is a **client only** — it never exposes a
//! REST service. Message schemas are transcribed verbatim from the binding contract §2;
//! the [`RcsClient`] trait is injected so tests can supply a hand-written double and
//! integration tests a real-HTTP mock.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{FilterError, Result};

/// Hard cap for every RCS JSON response. The current canonical rules snapshot is only a few
/// kilobytes; this leaves ample growth room while preventing a faulty peer from forcing an
/// unbounded allocation during response decoding.
const MAX_RESPONSE_BODY_BYTES: usize = 16 * 1024 * 1024;

/// `GET /rules` response (contract §2.2). `rules` may be an empty array.
#[derive(Debug, Clone, Deserialize)]
pub struct RulesResponse {
    pub protocol_version: u32,
    pub content_version: u64,
    pub rules: Vec<crate::rules::RawRule>,
}

/// `GET /rules/version` response (contract §2.3) — no `rules`.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct VersionResponse {
    pub protocol_version: u32,
    pub content_version: u64,
}

/// One decoded audit event as submitted to RCS (contract §2.4 `actions[<type>][]`).
/// `params` values are kept as strings (addresses lower-cased hex, uint256 decimal) to
/// preserve 18-digit precision (contract §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItem {
    /// Rule-local event name (the `event_abis` map key).
    pub name: String,
    /// `log.address` of the triggering log (e.g. the ERC20 token contract) — **not** `tx.to`.
    pub address: String,
    /// Named ABI-decoded parameters. Scalar integers remain decimal strings to preserve
    /// `uint256` precision; ABI arrays are represented as JSON arrays of those scalar values.
    /// Map ordering is deterministic for canonical hashing.
    pub params: BTreeMap<String, serde_json::Value>,
}

/// One transaction in a `POST /permission-requests/submit` batch (contract §2.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubmitTx {
    pub tx_hash: String,
    pub origin: String,
    /// `tx.to` — observational/audit only (contract §2.4).
    pub contract_address: String,
    pub nonce: u64,
    /// `{ audit_type: [ActionItem] }`; today the only key is `"quota"`.
    pub actions: BTreeMap<String, Vec<ActionItem>>,
}

/// `POST /permission-requests/submit` request body (contract §2.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubmitRequest {
    pub xlayer_block_height: u64,
    pub txs: Vec<SubmitTx>,
}

/// `POST /permission-requests/submit` `202 Accepted` response (contract §2.4). Both fields
/// are plain `tx_hash` string arrays.
#[derive(Debug, Clone, Deserialize)]
pub struct SubmitResponse {
    pub accepted: Vec<String>,
    pub rejected_malformed: Vec<String>,
}

/// `GET /permission-requests/query` mutually-exclusive query modes (contract §2.5).
#[derive(Debug, Clone)]
pub enum QueryParams {
    /// `?status=pending|approved|denied|outdated`
    Status(String),
    /// `?tx_hashes=0x..,0x..`
    TxHashes(Vec<String>),
}

/// One adjudication result row (contract §2.5).
#[derive(Debug, Clone, Deserialize)]
pub struct QueryTx {
    pub tx_hash: String,
    /// `pending|approved|denied|outdated` (`completed` only in `?tx_hashes=` mode).
    pub status: String,
    #[serde(default)]
    pub decided_at: Option<i64>,
    /// Free text; the filter never parses this, only logs it.
    #[serde(default)]
    pub reason: Option<String>,
}

/// `GET /permission-requests/query` `200 OK` response (contract §2.5). A `tx_hash` the RCS
/// has never seen (or already swept) is silently absent — not an error.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryResponse {
    pub txs: Vec<QueryTx>,
}

/// Client for the four RCS REST endpoints (contract §2). Injected so tests can mock it.
#[async_trait]
pub trait RcsClient: Send + Sync + std::fmt::Debug {
    /// `GET /rules` — full rule pull (contract §2.2).
    async fn get_rules(&self) -> Result<RulesResponse>;
    /// `GET /rules/version` — lightweight version probe (contract §2.3).
    async fn get_rules_version(&self) -> Result<VersionResponse>;
    /// `POST /permission-requests/submit` — batch submit, expects `202` (contract §2.4).
    async fn submit(&self, req: SubmitRequest) -> Result<SubmitResponse>;
    /// `GET /permission-requests/query` — adjudication poll (contract §2.5).
    async fn query(&self, q: QueryParams) -> Result<QueryResponse>;
}

/// Production `reqwest`-backed RCS client.
#[derive(Debug, Clone)]
pub struct ReqwestRcsClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestRcsClient {
    /// Builds a client against `base_url` (trailing slash trimmed).
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Self::with_timeouts(
            base_url,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(3),
        )
    }

    /// Builds a client with explicit connect and total-request timeout budgets.
    pub fn with_timeouts(
        base_url: impl Into<String>,
        connect_timeout: std::time::Duration,
        request_timeout: std::time::Duration,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .map_err(|e| FilterError::Transport(e.to_string()))?;
        Ok(Self { base_url: base_url.into().trim_end_matches('/').to_string(), http })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

#[async_trait]
impl RcsClient for ReqwestRcsClient {
    async fn get_rules(&self) -> Result<RulesResponse> {
        let resp = self.http.get(self.url("/rules")).send().await.map_err(transport_error)?;
        decode_json(resp, reqwest::StatusCode::OK).await
    }

    async fn get_rules_version(&self) -> Result<VersionResponse> {
        let resp =
            self.http.get(self.url("/rules/version")).send().await.map_err(transport_error)?;
        decode_json(resp, reqwest::StatusCode::OK).await
    }

    async fn submit(&self, req: SubmitRequest) -> Result<SubmitResponse> {
        let resp = self
            .http
            .post(self.url("/permission-requests/submit"))
            .json(&req)
            .send()
            .await
            .map_err(transport_error)?;
        decode_json(resp, reqwest::StatusCode::ACCEPTED).await
    }

    async fn query(&self, q: QueryParams) -> Result<QueryResponse> {
        let mut req = self.http.get(self.url("/permission-requests/query"));
        req = match q {
            QueryParams::Status(s) => req.query(&[("status", s)]),
            QueryParams::TxHashes(hashes) => req.query(&[("tx_hashes", hashes.join(","))]),
        };
        let resp = req.send().await.map_err(transport_error)?;
        decode_json(resp, reqwest::StatusCode::OK).await
    }
}

/// Decodes JSON only when the endpoint's exact expected status is returned.
async fn decode_json<T: serde::de::DeserializeOwned>(
    mut resp: reqwest::Response,
    expected: reqwest::StatusCode,
) -> Result<T> {
    let status = resp.status();
    if status != expected {
        return Err(FilterError::UnexpectedStatus(status.as_u16()));
    }

    if resp.content_length().is_some_and(|length| length > MAX_RESPONSE_BODY_BYTES as u64) {
        return Err(response_too_large());
    }
    let mut body = Vec::with_capacity(
        resp.content_length().unwrap_or_default().min(MAX_RESPONSE_BODY_BYTES as u64) as usize,
    );
    while let Some(chunk) = resp.chunk().await.map_err(transport_error)? {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BODY_BYTES {
            return Err(response_too_large());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|error| FilterError::Decode(error.to_string()))
}

fn response_too_large() -> FilterError {
    FilterError::Decode(format!("RCS response body exceeds {MAX_RESPONSE_BODY_BYTES} bytes"))
}

fn transport_error(error: reqwest::Error) -> FilterError {
    if error.is_timeout() {
        FilterError::Timeout(error.to_string())
    } else {
        FilterError::Transport(error.to_string())
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_http_script(
        responses: Vec<(u16, String, Duration)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut request_lines = Vec::new();
            for (status, body, body_delay) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 2048];
                loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&request);
                request_lines.push(text.lines().next().unwrap_or_default().to_string());
                let reason = match status {
                    200 => "OK",
                    201 => "Created",
                    202 => "Accepted",
                    204 => "No Content",
                    500 => "Internal Server Error",
                    _ => "Response",
                };
                let headers = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                if !body_delay.is_zero() {
                    tokio::time::sleep(body_delay).await;
                }
                let _ = stream.write_all(body.as_bytes()).await;
            }
            request_lines
        });
        (format!("http://{address}"), task)
    }

    async fn spawn_oversized_response() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_RESPONSE_BODY_BYTES + 1
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
        });
        (format!("http://{address}"), task)
    }

    #[test]
    fn required_response_fields_fail_decoding_when_missing() {
        assert!(serde_json::from_value::<RulesResponse>(json!({
            "protocol_version": 1,
            "content_version": 1
        }))
        .is_err());
        assert!(serde_json::from_value::<SubmitResponse>(json!({
            "accepted": []
        }))
        .is_err());
        assert!(serde_json::from_value::<SubmitResponse>(json!({
            "rejected_malformed": []
        }))
        .is_err());
        assert!(serde_json::from_value::<QueryResponse>(json!({})).is_err());
    }

    #[tokio::test]
    async fn endpoint_specific_statuses_are_enforced() {
        let submit_body = r#"{"accepted":[],"rejected_malformed":[]}"#;
        for status in [200, 201, 204, 500] {
            let (url, server) =
                spawn_http_script(vec![(status, submit_body.to_string(), Duration::ZERO)]).await;
            let client = ReqwestRcsClient::new(url).unwrap();
            let result =
                client.submit(SubmitRequest { xlayer_block_height: 1, txs: Vec::new() }).await;
            assert!(matches!(result, Err(FilterError::UnexpectedStatus(code)) if code == status));
            server.await.unwrap();
        }

        let (url, server) =
            spawn_http_script(vec![(202, submit_body.to_string(), Duration::ZERO)]).await;
        ReqwestRcsClient::new(url)
            .unwrap()
            .submit(SubmitRequest { xlayer_block_height: 1, txs: Vec::new() })
            .await
            .unwrap();
        server.await.unwrap();

        for status in [202, 204, 500] {
            let (url, server) =
                spawn_http_script(vec![(status, r#"{"txs":[]}"#.to_string(), Duration::ZERO)])
                    .await;
            let result = ReqwestRcsClient::new(url)
                .unwrap()
                .query(QueryParams::Status("pending".into()))
                .await;
            assert!(matches!(result, Err(FilterError::UnexpectedStatus(code)) if code == status));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn get_and_query_decode_only_valid_200_json() {
        let (url, server) = spawn_http_script(vec![
            (
                200,
                r#"{"protocol_version":1,"content_version":1,"rules":[]}"#.to_string(),
                Duration::ZERO,
            ),
            (200, r#"{"txs":[]}"#.to_string(), Duration::ZERO),
        ])
        .await;
        let client = ReqwestRcsClient::new(url).unwrap();
        assert!(client.get_rules().await.is_ok());
        assert!(client.query(QueryParams::Status("approved".into())).await.is_ok());
        server.await.unwrap();

        let (url, server) = spawn_http_script(vec![(200, "not-json".into(), Duration::ZERO)]).await;
        assert!(matches!(
            ReqwestRcsClient::new(url).unwrap().get_rules().await,
            Err(FilterError::Decode(_))
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn status_query_encoding_never_uses_tx_hashes() {
        let responses =
            (0..4).map(|_| (200, r#"{"txs":[]}"#.to_string(), Duration::ZERO)).collect();
        let (url, server) = spawn_http_script(responses).await;
        let client = ReqwestRcsClient::new(url).unwrap();
        for status in ["pending", "approved", "denied", "outdated"] {
            client.query(QueryParams::Status(status.into())).await.unwrap();
        }
        let requests = server.await.unwrap();
        for (request, status) in requests.iter().zip(["pending", "approved", "denied", "outdated"])
        {
            assert!(request.contains(&format!("?status={status}")), "{request}");
            assert!(!request.contains("tx_hashes"), "{request}");
        }
    }

    #[tokio::test]
    async fn tx_hashes_query_encodes_only_tx_hashes() {
        let (url, server) =
            spawn_http_script(vec![(200, r#"{"txs":[]}"#.to_string(), Duration::ZERO)]).await;
        let client = ReqwestRcsClient::new(url).unwrap();
        client.query(QueryParams::TxHashes(vec!["0xaaaa".into(), "0xbbbb".into()])).await.unwrap();

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("?tx_hashes=0xaaaa%2C0xbbbb"), "{}", requests[0]);
        assert!(!requests[0].contains("status="), "{}", requests[0]);
    }

    #[tokio::test]
    async fn rules_endpoints_reject_non_200() {
        for path in ["rules", "version"] {
            let (url, server) = spawn_http_script(vec![(
                202,
                r#"{"protocol_version":1,"content_version":1,"rules":[]}"#.to_string(),
                Duration::ZERO,
            )])
            .await;
            let client = ReqwestRcsClient::new(url).unwrap();
            let result = if path == "rules" {
                client.get_rules().await.map(|_| ())
            } else {
                client.get_rules_version().await.map(|_| ())
            };
            assert!(matches!(result, Err(FilterError::UnexpectedStatus(202))));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn response_body_stall_hits_total_request_timeout() {
        let (url, server) =
            spawn_http_script(vec![(200, r#"{"txs":[]}"#.to_string(), Duration::from_secs(1))])
                .await;
        let client = ReqwestRcsClient::with_timeouts(
            url,
            Duration::from_millis(50),
            Duration::from_millis(100),
        )
        .unwrap();
        assert!(matches!(
            client.query(QueryParams::Status("pending".into())).await,
            Err(FilterError::Timeout(_))
        ));
        server.abort();
    }

    #[tokio::test]
    async fn oversized_response_is_rejected_before_body_allocation() {
        let (url, server) = spawn_oversized_response().await;
        let result = ReqwestRcsClient::new(url).unwrap().get_rules().await;
        assert!(matches!(result, Err(FilterError::Decode(message)) if message.contains("exceeds")));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rules_version_and_required_fields_are_enforced_over_http() {
        let (url, server) = spawn_http_script(vec![(
            200,
            r#"{"protocol_version":1,"content_version":7}"#.to_string(),
            Duration::ZERO,
        )])
        .await;
        let version = ReqwestRcsClient::new(url).unwrap().get_rules_version().await.unwrap();
        assert_eq!(version.protocol_version, 1);
        assert_eq!(version.content_version, 7);
        server.await.unwrap();

        for body in [r#"{"protocol_version":1}"#, r#"{"content_version":7}"#] {
            let (url, server) =
                spawn_http_script(vec![(200, body.to_string(), Duration::ZERO)]).await;
            assert!(matches!(
                ReqwestRcsClient::new(url).unwrap().get_rules_version().await,
                Err(FilterError::Decode(_))
            ));
            server.await.unwrap();
        }

        let (url, server) =
            spawn_http_script(vec![(202, r#"{"accepted":[]}"#.to_string(), Duration::ZERO)]).await;
        let result = ReqwestRcsClient::new(url)
            .unwrap()
            .submit(SubmitRequest { xlayer_block_height: 1, txs: Vec::new() })
            .await;
        assert!(matches!(result, Err(FilterError::Decode(_))));
        server.await.unwrap();

        let (url, server) =
            spawn_http_script(vec![(200, r#"{}"#.to_string(), Duration::ZERO)]).await;
        assert!(matches!(
            ReqwestRcsClient::new(url).unwrap().query(QueryParams::Status("pending".into())).await,
            Err(FilterError::Decode(_))
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn client_recovers_after_stalled_response_without_recreation() {
        let (url, server) = spawn_http_script(vec![
            (200, r#"{"txs":[]}"#.to_string(), Duration::from_millis(150)),
            (200, r#"{"txs":[]}"#.to_string(), Duration::ZERO),
        ])
        .await;
        let client = ReqwestRcsClient::with_timeouts(
            url,
            Duration::from_millis(25),
            Duration::from_millis(75),
        )
        .unwrap();
        assert!(matches!(
            client.query(QueryParams::Status("pending".into())).await,
            Err(FilterError::Timeout(_))
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(client.query(QueryParams::Status("pending".into())).await.is_ok());
        server.await.unwrap();
    }
}
