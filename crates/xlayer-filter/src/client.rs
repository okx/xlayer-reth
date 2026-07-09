//! RCS REST client (contract §2). The filter is a **client only** — it never exposes a
//! REST service. Message schemas are transcribed verbatim from the binding contract §2;
//! the [`RcsClient`] trait is injected so tests can supply a hand-written double and
//! integration tests a real-HTTP mock.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{FilterError, Result};

/// `GET /rules` response (contract §2.2). `rules` may be an empty array.
#[derive(Debug, Clone, Deserialize)]
pub struct RulesResponse {
    pub protocol_version: u32,
    pub content_version: u64,
    #[serde(default)]
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
    /// Named ABI-decoded parameters. Ordered deterministically for canonical hashing.
    pub params: BTreeMap<String, String>,
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
    #[serde(default)]
    pub accepted: Vec<String>,
    #[serde(default)]
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
    #[serde(default)]
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
        let http = reqwest::Client::builder()
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
        let resp = self
            .http
            .get(self.url("/rules"))
            .send()
            .await
            .map_err(|e| FilterError::Transport(e.to_string()))?;
        decode_json(resp).await
    }

    async fn get_rules_version(&self) -> Result<VersionResponse> {
        let resp = self
            .http
            .get(self.url("/rules/version"))
            .send()
            .await
            .map_err(|e| FilterError::Transport(e.to_string()))?;
        decode_json(resp).await
    }

    async fn submit(&self, req: SubmitRequest) -> Result<SubmitResponse> {
        let resp = self
            .http
            .post(self.url("/permission-requests/submit"))
            .json(&req)
            .send()
            .await
            .map_err(|e| FilterError::Transport(e.to_string()))?;
        decode_json(resp).await
    }

    async fn query(&self, q: QueryParams) -> Result<QueryResponse> {
        let mut req = self.http.get(self.url("/permission-requests/query"));
        req = match q {
            QueryParams::Status(s) => req.query(&[("status", s)]),
            QueryParams::TxHashes(hashes) => req.query(&[("tx_hashes", hashes.join(","))]),
        };
        let resp = req.send().await.map_err(|e| FilterError::Transport(e.to_string()))?;
        decode_json(resp).await
    }
}

/// Decodes a successful (2xx) JSON response, mapping non-2xx to [`FilterError::UnexpectedStatus`].
async fn decode_json<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    if !status.is_success() {
        return Err(FilterError::UnexpectedStatus(status.as_u16()));
    }
    resp.json::<T>().await.map_err(|e| FilterError::Decode(e.to_string()))
}
