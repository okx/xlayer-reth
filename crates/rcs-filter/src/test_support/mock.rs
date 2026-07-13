//! Hand-written [`RcsClient`] test double with the FR-10 control plane (TD §4.10). In-memory
//! and network-free — suitable for module tests. Golden payloads are registered verbatim
//! from contract §4 via [`crate::test_support::golden`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::client::{
    QueryParams, QueryResponse, QueryTx, RcsClient, RulesResponse, SubmitRequest, SubmitResponse,
    VersionResponse,
};
use crate::error::{FilterError, Result};

/// One registered query result.
#[derive(Debug, Clone)]
struct QueryState {
    status: String,
    decided_at: Option<i64>,
    reason: Option<String>,
}

#[derive(Debug, Default)]
struct MockState {
    protocol_version: u32,
    content_version: u64,
    rules: Vec<Value>,
    submit_accepted: Option<Vec<String>>,
    submit_rejected: Vec<String>,
    query_states: HashMap<String, QueryState>,
    calls: Vec<String>,
    /// Every `submit` request body received (in order), for payload-verbatim assertions (§6.4).
    submitted: Vec<SubmitRequest>,
    unavailable: bool,
}

/// In-process mock RCS server double.
#[derive(Debug, Clone)]
pub struct MockRcsClient {
    state: Arc<Mutex<MockState>>,
}

impl Default for MockRcsClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MockRcsClient {
    /// Creates a mock with `protocol_version=1`, `content_version=1`, no rules.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState {
                protocol_version: 1,
                content_version: 1,
                ..Default::default()
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.state.lock().expect("mock state lock")
    }

    /// Registers the rule fixtures (each a JSON rule object string, e.g. golden RULE_SCENARIO_A).
    pub fn set_rules_fixture(&self, rules: &[&str]) {
        let parsed =
            rules.iter().map(|r| serde_json::from_str(r).expect("valid rule json")).collect();
        self.lock().rules = parsed;
    }

    /// Bumps `content_version` (simulates an RCS-side rule content change, FR-3).
    pub fn bump_rules_version(&self) {
        self.lock().content_version += 1;
    }

    /// Overrides the advertised `protocol_version` (FR-2/FR-3 unsupported-version tests).
    pub fn set_rules_protocol_version(&self, pv: u32) {
        self.lock().protocol_version = pv;
    }

    /// Registers the `202` submit response (accepted / rejected_malformed hash lists).
    pub fn register_submit_response(&self, accepted: &[&str], rejected: &[&str]) {
        let mut s = self.lock();
        s.submit_accepted = Some(accepted.iter().map(|x| x.to_string()).collect());
        s.submit_rejected = rejected.iter().map(|x| x.to_string()).collect();
    }

    /// Registers the query status for a tx_hash.
    pub fn register_query_state(&self, tx_hash: &str, status: &str, decided_at: Option<i64>) {
        self.lock().query_states.insert(
            tx_hash.to_string(),
            QueryState { status: status.to_string(), decided_at, reason: None },
        );
    }

    /// Removes a registered query state (tx becomes silently absent, contract §2.5).
    pub fn unregister_query_state(&self, tx_hash: &str) {
        self.lock().query_states.remove(tx_hash);
    }

    /// Makes all endpoints fail as if RCS is unreachable (FR-2/FR-6 fault injection).
    pub fn set_unavailable(&self, unavailable: bool) {
        self.lock().unavailable = unavailable;
    }

    /// Number of recorded calls to `endpoint` (`get_rules`/`get_rules_version`/`submit`/`query`).
    pub fn call_count(&self, endpoint: &str) -> usize {
        self.lock().calls.iter().filter(|c| c.as_str() == endpoint).count()
    }

    /// Full ordered call log.
    pub fn call_log(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    /// Every `submit` request body received (that reached the server, in order), for
    /// payload-verbatim assertions.
    pub fn submitted_requests(&self) -> Vec<SubmitRequest> {
        self.lock().submitted.clone()
    }

    /// The most recent `submit` request body, if any.
    pub fn last_submit(&self) -> Option<SubmitRequest> {
        self.lock().submitted.last().cloned()
    }

    fn record(&self, endpoint: &str) {
        self.lock().calls.push(endpoint.to_string());
    }
}

#[async_trait]
impl RcsClient for MockRcsClient {
    async fn get_rules(&self) -> Result<RulesResponse> {
        self.record("get_rules");
        let s = self.lock();
        if s.unavailable {
            return Err(FilterError::Transport("mock unavailable".into()));
        }
        let body = json!({
            "protocol_version": s.protocol_version,
            "content_version": s.content_version,
            "rules": Value::Array(s.rules.clone()),
        });
        serde_json::from_value(body).map_err(|e| FilterError::Decode(e.to_string()))
    }

    async fn get_rules_version(&self) -> Result<VersionResponse> {
        self.record("get_rules_version");
        let s = self.lock();
        if s.unavailable {
            return Err(FilterError::Transport("mock unavailable".into()));
        }
        Ok(VersionResponse {
            protocol_version: s.protocol_version,
            content_version: s.content_version,
        })
    }

    async fn submit(&self, req: SubmitRequest) -> Result<SubmitResponse> {
        self.record("submit");
        let mut s = self.lock();
        if s.unavailable {
            return Err(FilterError::Transport("mock unavailable".into()));
        }
        s.submitted.push(req.clone());
        // Default: echo all submitted hashes as accepted (idempotent RCS, contract §2.4).
        let accepted = s
            .submit_accepted
            .clone()
            .unwrap_or_else(|| req.txs.iter().map(|t| t.tx_hash.clone()).collect());
        Ok(SubmitResponse { accepted, rejected_malformed: s.submit_rejected.clone() })
    }

    async fn query(&self, q: QueryParams) -> Result<QueryResponse> {
        self.record("query");
        let s = self.lock();
        if s.unavailable {
            return Err(FilterError::Transport("mock unavailable".into()));
        }
        let txs = match q {
            QueryParams::TxHashes(hashes) => {
                hashes.iter().filter_map(|h| s.query_states.get(h).map(|st| to_tx(h, st))).collect()
            }
            QueryParams::Status(status) => s
                .query_states
                .iter()
                .filter(|(_, st)| st.status == status)
                .map(|(h, st)| to_tx(h, st))
                .collect(),
        };
        Ok(QueryResponse { txs })
    }
}

fn to_tx(tx_hash: &str, st: &QueryState) -> QueryTx {
    QueryTx {
        tx_hash: tx_hash.to_string(),
        status: st.status.clone(),
        decided_at: st.decided_at,
        reason: st.reason.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::golden;

    #[tokio::test]
    async fn get_rules_returns_registered_fixture() {
        let mock = MockRcsClient::new();
        mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
        let resp = mock.get_rules().await.expect("ok");
        assert_eq!(resp.protocol_version, 1);
        assert_eq!(resp.rules.len(), 1);
        assert_eq!(mock.call_count("get_rules"), 1);
    }

    #[tokio::test]
    async fn unavailable_errors() {
        let mock = MockRcsClient::new();
        mock.set_unavailable(true);
        assert!(mock.get_rules().await.is_err());
    }

    #[tokio::test]
    async fn submit_echoes_and_query_returns_state() {
        let mock = MockRcsClient::new();
        mock.register_query_state(golden::TX_A, "approved", Some(1_751_000_002));
        let resp = mock.query(QueryParams::TxHashes(vec![golden::TX_A.to_string()])).await.unwrap();
        assert_eq!(resp.txs.len(), 1);
        assert_eq!(resp.txs[0].status, "approved");
        // Absent hash → silently missing.
        let empty =
            mock.query(QueryParams::TxHashes(vec![golden::TX_B.to_string()])).await.unwrap();
        assert!(empty.txs.is_empty());
    }
}
