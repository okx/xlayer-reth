//! RCS REST client. The filter is a **client only** — it never exposes a REST service.
//! Message schemas match the RCS wire protocol;
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

/// `GET /rules` response. `rules` may be an empty array.
///
/// Deserialized through [`RulesResponseWire`] so an unknown key at the response **root** is
/// rejected too. Adding `#[serde(flatten)]` here alone would not reject anything: this type
/// never reaches `compile_rule`, so there is no per-rule gate to carry the failure. Instead the
/// conversion fails, `get_rules()` surfaces it as a decode [`FilterError`], and
/// `load_and_install` never reaches `install_rules` — last-known-good is retained.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RulesResponseWire")]
pub struct RulesResponse {
    pub protocol_version: u32,
    pub content_version: u64,
    pub rules: Vec<crate::rules::RawRule>,
}

/// Wire shape of [`RulesResponse`]: the three real fields plus every key the root does not
/// declare. `protocol_version` and `content_version` stay **required**, exactly as before.
#[derive(Debug, Deserialize)]
struct RulesResponseWire {
    protocol_version: u32,
    content_version: u64,
    rules: Vec<crate::rules::RawRule>,
    #[serde(flatten)]
    unknown: crate::rules::UnknownFields,
}

impl TryFrom<RulesResponseWire> for RulesResponse {
    type Error = String;

    fn try_from(wire: RulesResponseWire) -> std::result::Result<Self, String> {
        crate::rules::reject_unknown(crate::rules::RULES_RESPONSE_LOCATION, &wire.unknown)?;
        Ok(Self {
            protocol_version: wire.protocol_version,
            content_version: wire.content_version,
            rules: wire.rules,
        })
    }
}

/// `GET /rules/version` response — no `rules`.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct VersionResponse {
    pub protocol_version: u32,
    pub content_version: u64,
}

/// One decoded audit event as submitted to RCS in `actions[<type>][]`.
/// `params` values are kept as strings (addresses lower-cased hex, uint256 decimal) to
/// preserve 18-digit precision.
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

/// One transaction in a `POST /permission-requests/submit` batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubmitTx {
    pub tx_hash: String,
    pub origin: String,
    /// `tx.to` — observational/audit only.
    pub contract_address: String,
    pub nonce: u64,
    /// `{ audit_type: [ActionItem] }`; today the only key is `"quota"`.
    pub actions: BTreeMap<String, Vec<ActionItem>>,
}

/// `POST /permission-requests/submit` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubmitRequest {
    pub xlayer_block_height: u64,
    pub txs: Vec<SubmitTx>,
}

/// `POST /permission-requests/submit` `202 Accepted` response. Both fields
/// are plain `tx_hash` string arrays.
#[derive(Debug, Clone, Deserialize)]
pub struct SubmitResponse {
    pub accepted: Vec<String>,
    pub rejected_malformed: Vec<String>,
}

/// `GET /permission-requests/query` mutually-exclusive query modes.
#[derive(Debug, Clone)]
pub enum QueryParams {
    /// `?status=pending|approved|denied|outdated`
    Status(String),
    /// `?tx_hashes=0x..,0x..`
    TxHashes(Vec<String>),
}

/// One adjudication result row.
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

/// `GET /permission-requests/query` `200 OK` response. A `tx_hash` the RCS
/// has never seen (or already swept) is silently absent — not an error.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryResponse {
    pub txs: Vec<QueryTx>,
}

/// Client for the four RCS REST endpoints. Injected so tests can mock it.
#[async_trait]
pub trait RcsClient: Send + Sync + std::fmt::Debug {
    /// `GET /rules` — full rule pull.
    async fn get_rules(&self) -> Result<RulesResponse>;
    /// `GET /rules/version` — lightweight version probe.
    async fn get_rules_version(&self) -> Result<VersionResponse>;
    /// `POST /permission-requests/submit` — batch submit, expects `202`.
    async fn submit(&self, req: SubmitRequest) -> Result<SubmitResponse>;
    /// `GET /permission-requests/query` — adjudication poll.
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

#[path = "../tests/client__wire_tests.rs"]
#[cfg(test)]
mod wire_tests;
