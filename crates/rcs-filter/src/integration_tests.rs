//! Integration tests for the RCS-facing worker layer. Two flavours, both network-free and
//! deterministic:
//!
//! - **Worker-function tests** drive the `pub(crate)` [`crate::worker`] entry points
//!   (`load_and_install` / `submit_once` / `query_once`) directly against a
//!   [`MockRcsClient`] — no spawning, no timers, fully deterministic. These pin the
//!   exact wire payload and every status → buffer transition.
//! - **Orchestration tests** spawn the real [`FilterHandle`] with its four background
//!   tasks under a paused tokio clock, driving virtual time so the loops (initial-load retry
//!   retry, hot-reload polling, batch submit, adjudication poll, timeout tick) actually run.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use alloy_primitives::{Address, Bytes, Log, LogData, B256, U256};
use async_trait::async_trait;
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use serde_json::json;

use crate::client::{
    QueryParams, QueryResponse, QueryTx, RcsClient, RulesResponse, SubmitRequest, SubmitResponse,
    VersionResponse,
};
use crate::config::FilterConfig;
use crate::handle::{
    FilterHandle, PreScreen, Screen, ScreenInput, Shared, TerminalEvent, TerminalReason,
};
use crate::matching::{self, MatchOutcome};
use crate::pool::{BufferEntry, BufferPool, BufferStatus};
use crate::rules::{load_rules, RuleSet};
use crate::test_support::{golden, log_builder, MockRcsClient, TestClock};
use crate::worker;

const START: u64 = 1_751_000_000;

/// A filter config with the master switch on (RCS URL is unused — the mock is injected
/// directly, not over HTTP).
fn enabled_config() -> FilterConfig {
    FilterConfig { enabled: true, rcs_base_url: "http://mock".into(), ..Default::default() }
}

fn scenario_a_rules() -> RuleSet {
    load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()])
}

fn transfer_batch_rules() -> RuleSet {
    let raw = r#"{"id":"batch-quota","event_abis":{"transferBatch":{"type":"event","name":"TransferBatch","inputs":[{"name":"operator","type":"address","indexed":true},{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"ids","type":"uint256[]","indexed":false},{"name":"values","type":"uint256[]","indexed":false}],"anonymous":false}},"audit_types":["quota"],"condition":true,"action":"audit","audit_timeout_action":"allow"}"#;
    load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()])
}

fn official_snapshot_rules() -> RuleSet {
    let response: RulesResponse =
        serde_json::from_str(include_str!("../testdata/rules-snapshot.json"))
            .expect("official RCS snapshot must deserialize");
    assert_eq!(response.protocol_version, 1);
    assert_eq!(response.content_version, 1_784_019_568);
    load_rules(response.protocol_version, response.content_version, response.rules)
}

fn official_snapshot_token() -> Address {
    "0x2a511132DD27cA58A80c74F9d75dC6a707935597".parse().unwrap()
}

fn official_snapshot_from() -> Address {
    "0xDA04189B410cEf73BD6564D21c87A2DA335E7c49".parse().unwrap()
}

fn transfer_log(amount: &str) -> Log {
    log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        U256::from_str(amount).unwrap(),
    )
}

/// The scenario-a transaction: `tx_hash=TX_A`, `origin=ORIGIN`,
/// `tx.to=CLAIM_CONTRACT`, `nonce=1`, `block_height=1_000_000`.
fn scenario_a_input(tx_hash: B256, logs: &[Log]) -> ScreenInput<'_> {
    ScreenInput {
        tx_hash,
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: U256::ZERO,
        block_height: 1_000_000,
        logs,
    }
}

// ============================================================================================
// Worker-function tests (direct pub(crate) calls; deterministic, no spawning)
// ============================================================================================

fn shared_with(clock: Arc<TestClock>, rules: RuleSet, ready: bool) -> Shared {
    let (terminal_events, _) = tokio::sync::broadcast::channel(1024);
    Shared {
        config: enabled_config(),
        rules: Arc::new(RwLock::new(Arc::new(rules))),
        pool: Arc::new(Mutex::new(BufferPool::default())),
        clock,
        ready: Arc::new(AtomicBool::new(ready)),
        terminal_events,
        metrics: crate::metrics::RcsFilterMetrics::default(),
    }
}

/// Builds a `NotSubmitted` buffer entry from a real scenario-a match so the submitted
/// payload carries the genuine `actions.quota` (not a hand-faked stub).
fn scenario_a_entry(tx_hash: B256, block_height: u64, now: u64) -> BufferEntry {
    let rules = Arc::new(scenario_a_rules());
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input = ScreenInput { block_height, ..scenario_a_input(tx_hash, &logs) };
    let (actions, timeout_action) = match matching::evaluate(&rules, &input) {
        MatchOutcome::Audit { actions, timeout_action } => (actions, timeout_action),
        other => panic!("expected Audit, got {other:?}"),
    };
    BufferEntry {
        generation: 0,
        tx_hash,
        origin: golden::origin(),
        contract_address: golden::claim_contract(),
        nonce: 1,
        block_height,
        status: BufferStatus::NotSubmitted,
        actions,
        rule_snapshot: rules,
        quota_consistency_hash: B256::ZERO,
        timeout_action,
        first_not_submitted_at: now,
        last_transition_at: now,
    }
}

fn independent_entry(id: u8, block_height: u64, now: u64) -> BufferEntry {
    let mut entry = scenario_a_entry(B256::repeat_byte(id), block_height, now);
    entry.origin = Address::repeat_byte(id);
    entry.nonce = u64::from(id);
    let action = entry.actions.get_mut("quota").unwrap().first_mut().unwrap();
    action.address = format!("{:#x}", Address::repeat_byte(id.wrapping_add(64)));
    action.params.insert(
        "to".to_string(),
        json!(format!("{:#x}", Address::repeat_byte(id.wrapping_add(128)))),
    );
    entry
}

#[derive(Debug, Default)]
struct GatedResponseClient {
    submit_started: tokio::sync::Notify,
    submit_release: tokio::sync::Notify,
    query_started: tokio::sync::Notify,
    query_release: tokio::sync::Notify,
    block_first_submit: AtomicBool,
    block_first_query: AtomicBool,
    submit_heights: Mutex<Vec<u64>>,
}

impl GatedResponseClient {
    fn new() -> Self {
        Self {
            block_first_submit: AtomicBool::new(true),
            block_first_query: AtomicBool::new(true),
            ..Default::default()
        }
    }
}

#[async_trait]
impl RcsClient for GatedResponseClient {
    async fn get_rules(&self) -> crate::Result<RulesResponse> {
        unreachable!()
    }

    async fn get_rules_version(&self) -> crate::Result<VersionResponse> {
        unreachable!()
    }

    async fn submit(&self, req: SubmitRequest) -> crate::Result<SubmitResponse> {
        self.submit_heights.lock().unwrap().push(req.xlayer_block_height);
        if self.block_first_submit.swap(false, Ordering::SeqCst) {
            self.submit_started.notify_one();
            self.submit_release.notified().await;
        }
        Ok(SubmitResponse {
            accepted: req.txs.into_iter().map(|tx| tx.tx_hash).collect(),
            rejected_malformed: Vec::new(),
        })
    }

    async fn query(&self, params: QueryParams) -> crate::Result<QueryResponse> {
        if self.block_first_query.swap(false, Ordering::SeqCst) {
            self.query_started.notify_one();
            self.query_release.notified().await;
        }
        let txs = match params {
            QueryParams::Status(status) if status == "approved" => vec![QueryTx {
                tx_hash: golden::TX_A.to_string(),
                status,
                decided_at: None,
                reason: None,
            }],
            _ => Vec::new(),
        };
        Ok(QueryResponse { txs })
    }
}

#[derive(Debug)]
struct ControlledSubmitClient {
    gates: Mutex<HashMap<u64, Arc<tokio::sync::Semaphore>>>,
    failed_heights: Mutex<HashSet<u64>>,
    started: Mutex<Vec<u64>>,
    completed: Mutex<Vec<u64>>,
    started_version: tokio::sync::watch::Sender<usize>,
    completed_version: tokio::sync::watch::Sender<usize>,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl ControlledSubmitClient {
    fn new() -> Self {
        let (started_version, _) = tokio::sync::watch::channel(0);
        let (completed_version, _) = tokio::sync::watch::channel(0);
        Self {
            gates: Mutex::new(HashMap::new()),
            failed_heights: Mutex::new(HashSet::new()),
            started: Mutex::new(Vec::new()),
            completed: Mutex::new(Vec::new()),
            started_version,
            completed_version,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        }
    }

    fn gate(&self, height: u64) {
        self.gates.lock().unwrap().insert(height, Arc::new(tokio::sync::Semaphore::new(0)));
    }

    fn release(&self, height: u64) {
        self.gates.lock().unwrap().get(&height).expect("registered gate").add_permits(1);
    }

    fn fail(&self, height: u64) {
        self.failed_heights.lock().unwrap().insert(height);
    }

    fn has_started(&self, height: u64) -> bool {
        self.started.lock().unwrap().contains(&height)
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }

    async fn wait_started(&self, height: u64) {
        let mut changes = self.started_version.subscribe();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if self.has_started(height) {
                    break;
                }
                changes.changed().await.expect("submit client remains alive");
            }
        })
        .await
        .expect("submit height started before timeout");
    }

    async fn wait_completed(&self, height: u64) {
        let mut changes = self.completed_version.subscribe();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if self.completed.lock().unwrap().contains(&height) {
                    break;
                }
                changes.changed().await.expect("submit client remains alive");
            }
        })
        .await
        .expect("submit height completed before timeout");
    }
}

struct ControlledActiveGuard<'a>(&'a ControlledSubmitClient);

impl Drop for ControlledActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl RcsClient for ControlledSubmitClient {
    async fn get_rules(&self) -> crate::Result<RulesResponse> {
        unreachable!()
    }

    async fn get_rules_version(&self) -> crate::Result<VersionResponse> {
        unreachable!()
    }

    async fn submit(&self, req: SubmitRequest) -> crate::Result<SubmitResponse> {
        let height = req.xlayer_block_height;
        let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(current, Ordering::SeqCst);
        let active = ControlledActiveGuard(self);
        let version = {
            let mut started = self.started.lock().unwrap();
            started.push(height);
            started.len()
        };
        self.started_version.send_replace(version);
        let gate = self.gates.lock().unwrap().get(&height).cloned();
        if let Some(gate) = gate {
            gate.acquire().await.expect("test gate remains open").forget();
        }
        if self.failed_heights.lock().unwrap().contains(&height) {
            return Err(crate::FilterError::Transport(format!("height {height} failed")));
        }
        let completed_version = {
            let mut completed = self.completed.lock().unwrap();
            completed.push(height);
            completed.len()
        };
        self.completed_version.send_replace(completed_version);
        drop(active);
        Ok(SubmitResponse {
            accepted: req.txs.into_iter().map(|tx| tx.tx_hash).collect(),
            rejected_malformed: Vec::new(),
        })
    }

    async fn query(&self, _params: QueryParams) -> crate::Result<QueryResponse> {
        unreachable!()
    }
}

#[derive(Debug)]
struct DelayedSubmitClient {
    delay: Duration,
}

#[async_trait]
impl RcsClient for DelayedSubmitClient {
    async fn get_rules(&self) -> crate::Result<RulesResponse> {
        unreachable!()
    }

    async fn get_rules_version(&self) -> crate::Result<VersionResponse> {
        unreachable!()
    }

    async fn submit(&self, req: SubmitRequest) -> crate::Result<SubmitResponse> {
        tokio::time::sleep(self.delay).await;
        Ok(SubmitResponse {
            accepted: req.txs.into_iter().map(|tx| tx.tx_hash).collect(),
            rejected_malformed: Vec::new(),
        })
    }

    async fn query(&self, _params: QueryParams) -> crate::Result<QueryResponse> {
        unreachable!()
    }
}

#[derive(Debug)]
struct ConflictingQueryClient;

#[async_trait]
impl RcsClient for ConflictingQueryClient {
    async fn get_rules(&self) -> crate::Result<RulesResponse> {
        unreachable!()
    }

    async fn get_rules_version(&self) -> crate::Result<VersionResponse> {
        unreachable!()
    }

    async fn submit(&self, _req: SubmitRequest) -> crate::Result<SubmitResponse> {
        unreachable!()
    }

    async fn query(&self, params: QueryParams) -> crate::Result<QueryResponse> {
        let txs = match params {
            QueryParams::Status(status) if status == "approved" => vec![
                QueryTx {
                    tx_hash: golden::TX_A.to_string(),
                    status: "approved".to_string(),
                    decided_at: Some(START as i64 + 2),
                    reason: None,
                },
                QueryTx {
                    tx_hash: golden::TX_A.to_string(),
                    status: "denied".to_string(),
                    decided_at: Some(START as i64 + 2),
                    reason: Some("conflicting duplicate row".to_string()),
                },
            ],
            _ => Vec::new(),
        };
        Ok(QueryResponse { txs })
    }
}

#[tokio::test]
async fn stale_submit_response_does_not_advance_reinserted_lifecycle() {
    let client = Arc::new(GatedResponseClient::new());
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    shared.pool_lock().insert(scenario_a_entry(golden::tx_a(), 1_000_000, START));

    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });
    client.submit_started.notified().await;

    let old_generation = shared.pool_lock().get(&golden::tx_a()).unwrap().generation;
    shared.pool_lock().remove(&golden::tx_a());
    shared.pool_lock().insert(scenario_a_entry(golden::tx_a(), 1_000_001, START + 1));
    let new_generation = shared.pool_lock().get(&golden::tx_a()).unwrap().generation;
    assert_ne!(old_generation, new_generation);
    client.submit_release.notify_one();
    task.await.unwrap().unwrap();

    assert_eq!(shared.pool_lock().get(&golden::tx_a()).unwrap().status, BufferStatus::NotSubmitted);
}

#[tokio::test]
async fn it_submit_response_after_deadline_is_ignored() {
    let client = Arc::new(GatedResponseClient::new());
    let clock = Arc::new(TestClock::new(START));
    let shared = shared_with(clock.clone(), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    shared.pool_lock().insert(scenario_a_entry(golden::tx_a(), 1_000_000, START));

    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });
    client.submit_started.notified().await;
    clock.set(START + 91);
    client.submit_release.notify_one();
    task.await.unwrap().unwrap();

    assert_eq!(
        shared.pool_lock().get(&golden::tx_a()).unwrap().status,
        BufferStatus::TimedOutAllow
    );
    assert!(events.try_recv().is_err(), "fail-open timeout emits no discard event");
}

#[tokio::test]
async fn it_late_submit_fail_close_emits_exactly_one_event() {
    let client = Arc::new(GatedResponseClient::new());
    let clock = Arc::new(TestClock::new(START));
    let shared = shared_with(clock.clone(), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    let mut entry = scenario_a_entry(golden::tx_a(), 1_000_000, START);
    entry.timeout_action = crate::rules::TimeoutAction::Deny;
    shared.pool_lock().insert(entry);

    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });
    client.submit_started.notified().await;
    clock.set(START + 91);
    client.submit_release.notify_one();
    task.await.unwrap().unwrap();

    assert_eq!(shared.pool_lock().get(&golden::tx_a()).unwrap().status, BufferStatus::Dropped);
    assert_eq!(
        events.try_recv().unwrap(),
        TerminalEvent {
            tx_hash: golden::tx_a(),
            generation: 1,
            reason: TerminalReason::FailCloseTimeout,
        }
    );
    assert!(events.try_recv().is_err(), "late submit must not emit a second event");
}

#[tokio::test]
async fn expired_later_height_is_never_submitted() {
    let client = Arc::new(GatedResponseClient::new());
    let clock = Arc::new(TestClock::new(START));
    let shared = shared_with(clock.clone(), scenario_a_rules(), true);
    shared.pool_lock().insert(scenario_a_entry(golden::tx_a(), 1_000_000, START));
    shared.pool_lock().insert(scenario_a_entry(golden::tx_c(), 1_000_001, START));

    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });
    client.submit_started.notified().await;
    clock.set(START + 91);
    client.submit_release.notify_one();
    task.await.unwrap().unwrap();

    assert_eq!(*client.submit_heights.lock().unwrap(), [1_000_000]);
    assert_eq!(
        shared.pool_lock().get(&golden::tx_a()).unwrap().status,
        BufferStatus::TimedOutAllow
    );
    assert_eq!(
        shared.pool_lock().get(&golden::tx_c()).unwrap().status,
        BufferStatus::TimedOutAllow
    );
}

#[tokio::test]
async fn it_query_timeout_race_is_deterministic() {
    let client = Arc::new(GatedResponseClient::new());
    let clock = Arc::new(TestClock::new(START));
    let shared = shared_with(clock.clone(), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    let mut entry = scenario_a_entry(golden::tx_a(), 1_000_000, START);
    entry.status = BufferStatus::Submitted;
    shared.pool_lock().insert(entry);

    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::query_once(&task_shared, &task_client).await });
    client.query_started.notified().await;
    clock.set(START + 91);
    client.query_release.notify_one();
    task.await.unwrap().unwrap();

    assert_eq!(
        shared.pool_lock().get(&golden::tx_a()).unwrap().status,
        BufferStatus::TimedOutAllow
    );
    assert!(events.try_recv().is_err(), "fail-open timeout emits no discard event");
}

#[tokio::test]
async fn it_timeout_first_fail_close_emits_once_and_late_query_is_ignored() {
    let client = Arc::new(GatedResponseClient::new());
    let clock = Arc::new(TestClock::new(START));
    let shared = shared_with(clock.clone(), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    let mut entry = scenario_a_entry(golden::tx_a(), 1_000_000, START);
    entry.status = BufferStatus::Submitted;
    entry.timeout_action = crate::rules::TimeoutAction::Deny;
    shared.pool_lock().insert(entry);

    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::query_once(&task_shared, &task_client).await });
    client.query_started.notified().await;
    clock.set(START + 91);
    let (resolved, _) = worker::timeout_once(&shared);
    assert_eq!(resolved, [(golden::tx_a(), 1, crate::pool::Resolution::Discard)]);
    client.query_release.notify_one();
    task.await.unwrap().unwrap();

    assert_eq!(shared.pool_lock().get(&golden::tx_a()).unwrap().status, BufferStatus::Dropped);
    assert_eq!(
        events.try_recv().unwrap(),
        TerminalEvent {
            tx_hash: golden::tx_a(),
            generation: 1,
            reason: TerminalReason::FailCloseTimeout,
        }
    );
    assert!(events.try_recv().is_err(), "late query must not emit a second terminal event");
}

#[tokio::test]
async fn conflicting_duplicate_query_rows_prefer_terminal_rejection() {
    let client: Arc<dyn RcsClient> = Arc::new(ConflictingQueryClient);
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    let mut entry = scenario_a_entry(golden::tx_a(), 1_000_000, START);
    entry.status = BufferStatus::Submitted;
    shared.pool_lock().insert(entry);

    worker::query_once(&shared, &client).await.expect("query ok");

    assert_eq!(shared.pool_lock().get(&golden::tx_a()).unwrap().status, BufferStatus::Dropped);
    assert_eq!(
        events.try_recv().unwrap(),
        TerminalEvent { tx_hash: golden::tx_a(), generation: 1, reason: TerminalReason::Denied }
    );
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn it_approved_survives_rcs_outage_past_total_timeout() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_unavailable(true);
    let client: Arc<dyn RcsClient> = mock;
    let clock = Arc::new(TestClock::new(START + 200));
    let shared = shared_with(clock, scenario_a_rules(), true);
    let mut entry = scenario_a_entry(golden::tx_a(), 1_000_000, START);
    entry.status = BufferStatus::Approved;
    shared.pool_lock().insert(entry);

    assert!(worker::query_once(&shared, &client).await.is_err());
    assert_eq!(shared.pool_lock().get(&golden::tx_a()).unwrap().status, BufferStatus::Approved);
    assert!(worker::timeout_once(&shared).0.is_empty());
    assert_eq!(shared.pool_lock().get(&golden::tx_a()).unwrap().status, BufferStatus::Approved);
}

#[tokio::test]
async fn stale_query_response_does_not_approve_reinserted_lifecycle() {
    let client = Arc::new(GatedResponseClient::new());
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    let mut first = scenario_a_entry(golden::tx_a(), 1_000_000, START);
    first.status = BufferStatus::Submitted;
    shared.pool_lock().insert(first);

    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::query_once(&task_shared, &task_client).await });
    client.query_started.notified().await;

    let old_generation = shared.pool_lock().get(&golden::tx_a()).unwrap().generation;
    shared.pool_lock().remove(&golden::tx_a());
    let mut replacement = scenario_a_entry(golden::tx_a(), 1_000_001, START + 1);
    replacement.status = BufferStatus::Submitted;
    shared.pool_lock().insert(replacement);
    let new_generation = shared.pool_lock().get(&golden::tx_a()).unwrap().generation;
    assert_ne!(old_generation, new_generation);
    client.query_release.notify_one();
    task.await.unwrap().unwrap();

    assert_eq!(shared.pool_lock().get(&golden::tx_a()).unwrap().status, BufferStatus::Submitted);
}

/// The submit payload matches scenario a **verbatim**,
/// and an accepted hash advances `NotSubmitted → Submitted`.
#[tokio::test]
async fn submit_once_builds_contract_scenario_a_payload_verbatim() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    shared.pool.lock().unwrap().insert(scenario_a_entry(golden::tx_a(), 1_000_000, START));

    worker::submit_once(&shared, &client).await.expect("submit ok");

    // Wire payload — every field must match scenario a.
    let req = mock.last_submit().expect("a submit request was recorded");
    assert_eq!(req.xlayer_block_height, 1_000_000);
    assert_eq!(req.txs.len(), 1);
    let tx = &req.txs[0];
    assert_eq!(tx.tx_hash, golden::TX_A);
    assert_eq!(tx.origin, golden::ORIGIN);
    assert_eq!(tx.contract_address, golden::CLAIM_CONTRACT);
    assert_eq!(tx.nonce, 1);
    let quota = tx.actions.get("quota").expect("quota audit type");
    assert_eq!(quota.len(), 1);
    assert_eq!(quota[0].name, "transfer");
    assert_eq!(quota[0].address, golden::TOKEN_X);
    assert_eq!(quota[0].params.get("from").unwrap(), golden::BRIDGE_ERC20);
    assert_eq!(quota[0].params.get("to").unwrap(), golden::RECIPIENT);
    assert_eq!(quota[0].params.get("value").unwrap(), golden::ONE_TOKEN);

    // The default mock echoes accepted → the entry advances to Submitted.
    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_a()).unwrap().status,
        BufferStatus::Submitted
    );
}

/// A hash returned only in `rejected_malformed` (never in `accepted`) stays
/// `NotSubmitted` for a later retry.
#[tokio::test]
async fn submit_once_keeps_rejected_malformed_not_submitted() {
    let mock = Arc::new(MockRcsClient::new());
    mock.register_submit_response(&[], &[golden::TX_A]);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    shared.pool.lock().unwrap().insert(scenario_a_entry(golden::tx_a(), 1_000_000, START));

    worker::submit_once(&shared, &client).await.expect("submit ok");

    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_a()).unwrap().status,
        BufferStatus::NotSubmitted
    );
}

/// Transactions are submitted grouped by `xlayer_block_height` — one request per height.
#[tokio::test]
async fn submit_once_groups_by_block_height() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    {
        let mut pool = shared.pool.lock().unwrap();
        pool.insert(scenario_a_entry(golden::tx_a(), 1_000_000, START));
        pool.insert(scenario_a_entry(golden::tx_c(), 1_000_010, START));
    }

    worker::submit_once(&shared, &client).await.expect("submit ok");

    let reqs = mock.submitted_requests();
    assert_eq!(reqs.len(), 2, "one request per block height");
    let heights: Vec<u64> = reqs.iter().map(|r| r.xlayer_block_height).collect();
    assert_eq!(heights, [1_000_000, 1_000_010], "K=1 preserves height order");
    assert!(reqs.iter().all(|r| r.txs.len() == 1));
}

#[tokio::test]
async fn submit_group_isolation_continues_after_one_height_fails() {
    let mock = Arc::new(MockRcsClient::new());
    mock.fail_submit_height(1_000_000);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    {
        let mut pool = shared.pool.lock().unwrap();
        pool.insert(scenario_a_entry(golden::tx_a(), 1_000_000, START));
        pool.insert(scenario_a_entry(golden::tx_c(), 1_000_010, START));
    }

    assert!(worker::submit_once(&shared, &client).await.is_err());
    let pool = shared.pool.lock().unwrap();
    assert_eq!(pool.get(&golden::tx_a()).unwrap().status, BufferStatus::NotSubmitted);
    assert_eq!(pool.get(&golden::tx_c()).unwrap().status, BufferStatus::Submitted);
}

fn submit_batch_histograms(snapshotter: &Snapshotter) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut groups = Vec::new();
    let mut max_in_flight = Vec::new();
    let mut durations = Vec::new();
    for (key, _, _, value) in snapshotter.snapshot().into_vec() {
        let DebugValue::Histogram(values) = value else { continue };
        let values = values.into_iter().map(|value| value.into_inner()).collect::<Vec<_>>();
        match key.key().name() {
            name if name.ends_with("submit_batch_groups") => groups = values,
            name if name.ends_with("submit_batch_max_in_flight") => max_in_flight = values,
            name if name.ends_with("submit_batch_duration_seconds") => durations = values,
            _ => {}
        }
    }
    (groups, max_in_flight, durations)
}

fn assert_single_group_batch_metrics(snapshotter: &Snapshotter) {
    let (groups, max_in_flight, durations) = submit_batch_histograms(snapshotter);
    assert_eq!(groups, [1.0]);
    assert_eq!(max_in_flight, [1.0]);
    assert_eq!(durations.len(), 1);
    assert!(durations[0] >= 0.0);
}

fn attach_isolated_submit_batch_metrics(shared: &mut Shared) {
    // The production derive uses metric call-site handles that may already have been initialized
    // by another parallel test. Unique test-only call sites keep this recorder deterministic.
    shared.metrics.submit_batch_groups = metrics::histogram!("rcs_filter_test.submit_batch_groups");
    shared.metrics.submit_batch_max_in_flight =
        metrics::histogram!("rcs_filter_test.submit_batch_max_in_flight");
    shared.metrics.submit_batch_duration_seconds =
        metrics::histogram!("rcs_filter_test.submit_batch_duration_seconds");
}

#[tokio::test(flavor = "current_thread")]
async fn submit_batch_metrics_cover_empty_completed_and_error_batches() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _guard = metrics::set_default_local_recorder(&recorder);

    let mut empty = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    attach_isolated_submit_batch_metrics(&mut empty);
    let empty_client: Arc<dyn RcsClient> = Arc::new(MockRcsClient::new());
    worker::submit_once(&empty, &empty_client).await.unwrap();
    assert_eq!(submit_batch_histograms(&snapshotter), (vec![], vec![], vec![]));

    let mut completed = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    attach_isolated_submit_batch_metrics(&mut completed);
    completed.pool_lock().insert(scenario_a_entry(golden::tx_a(), 1_000_000, START));
    let completed_client: Arc<dyn RcsClient> = Arc::new(MockRcsClient::new());
    worker::submit_once(&completed, &completed_client).await.unwrap();
    assert_single_group_batch_metrics(&snapshotter);

    let mock = Arc::new(MockRcsClient::new());
    mock.fail_submit_height(1_000_010);
    let error_client: Arc<dyn RcsClient> = mock;
    let mut error = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    attach_isolated_submit_batch_metrics(&mut error);
    error.pool_lock().insert(scenario_a_entry(golden::tx_c(), 1_000_010, START));
    assert!(worker::submit_once(&error, &error_client).await.is_err());
    assert_single_group_batch_metrics(&snapshotter);
}

fn concurrent_submit_shared(entries: impl IntoIterator<Item = BufferEntry>) -> Shared {
    submit_shared_with_concurrency(entries, 4)
}

fn submit_shared_with_concurrency(
    entries: impl IntoIterator<Item = BufferEntry>,
    max_concurrency: usize,
) -> Shared {
    let mut shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    shared.config.submit_max_concurrency = max_concurrency;
    let mut pool = shared.pool_lock();
    for entry in entries {
        pool.insert(entry);
    }
    drop(pool);
    shared
}

#[tokio::test]
async fn submit_once_refills_slots_beyond_limit_while_first_group_is_blocked() {
    let heights = [1_000_001, 1_000_002, 1_000_003, 1_000_004, 1_000_005, 1_000_006];
    let client = Arc::new(ControlledSubmitClient::new());
    for height in &heights[..4] {
        client.gate(*height);
    }
    let shared =
        concurrent_submit_shared(heights.into_iter().enumerate().map(|(index, height)| {
            independent_entry(u8::try_from(index + 1).unwrap(), height, START)
        }));
    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });

    for height in &heights[..4] {
        client.wait_started(*height).await;
    }
    assert_eq!(client.max_active(), 4);

    client.release(heights[1]);
    client.wait_started(heights[4]).await;
    client.wait_started(heights[5]).await;
    assert!(!client.completed.lock().unwrap().contains(&heights[0]));
    assert!(client.max_active() <= 4);

    client.release(heights[0]);
    client.release(heights[2]);
    client.release(heights[3]);
    task.await.unwrap().unwrap();
    assert!(shared.pool_lock().status_counts().iter().enumerate().all(|(index, count)| {
        if index == 1 {
            *count == 6
        } else {
            *count == 0
        }
    }));
}

#[tokio::test(start_paused = true)]
async fn four_way_concurrency_reduces_eight_group_latency_by_four_times() {
    async fn run(max_concurrency: usize) -> (Duration, [usize; 6]) {
        let entries = (0..8).map(|index| {
            independent_entry(
                u8::try_from(index + 101).unwrap(),
                1_001_000 + u64::try_from(index).unwrap(),
                START,
            )
        });
        let shared = submit_shared_with_concurrency(entries, max_concurrency);
        let client: Arc<dyn RcsClient> =
            Arc::new(DelayedSubmitClient { delay: Duration::from_millis(100) });
        let started = tokio::time::Instant::now();

        worker::submit_once(&shared, &client).await.unwrap();

        (started.elapsed(), shared.pool_lock().status_counts())
    }

    let (serial_elapsed, serial_statuses) = run(1).await;
    let (concurrent_elapsed, concurrent_statuses) = run(4).await;

    assert_eq!(serial_elapsed, Duration::from_millis(800));
    assert_eq!(concurrent_elapsed, Duration::from_millis(200));
    assert_eq!(serial_elapsed, concurrent_elapsed * 4);
    assert_eq!(serial_statuses, [0, 8, 0, 0, 0, 0]);
    assert_eq!(concurrent_statuses, serial_statuses);
}

#[tokio::test]
async fn submit_once_preserves_same_nonce_order_while_refilling_independent_work() {
    let heights = [1_000_011, 1_000_012, 1_000_013, 1_000_014, 1_000_015, 1_000_016];
    let mut entries = heights
        .into_iter()
        .enumerate()
        .map(|(index, height)| independent_entry(u8::try_from(index + 11).unwrap(), height, START))
        .collect::<Vec<_>>();
    entries[4].origin = entries[0].origin;
    entries[4].nonce = entries[0].nonce;

    let client = Arc::new(ControlledSubmitClient::new());
    for height in &heights[..4] {
        client.gate(*height);
    }
    let shared = concurrent_submit_shared(entries);
    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });

    for height in &heights[..4] {
        client.wait_started(*height).await;
    }
    client.release(heights[1]);
    client.wait_started(heights[5]).await;
    assert!(!client.has_started(heights[4]), "same-nonce successor bypassed predecessor");

    client.release(heights[0]);
    client.wait_started(heights[4]).await;
    client.release(heights[2]);
    client.release(heights[3]);
    task.await.unwrap().unwrap();
    assert!(client.max_active() <= 4);
}

#[tokio::test]
async fn submit_once_preserves_same_quota_order_while_refilling_independent_work() {
    let heights = [1_000_021, 1_000_022, 1_000_023, 1_000_024, 1_000_025, 1_000_026];
    let mut entries = heights
        .into_iter()
        .enumerate()
        .map(|(index, height)| independent_entry(u8::try_from(index + 21).unwrap(), height, START))
        .collect::<Vec<_>>();
    let first_action = entries[0].actions["quota"][0].clone();
    let successor_action = entries[4].actions.get_mut("quota").unwrap().first_mut().unwrap();
    successor_action.address = first_action.address;
    successor_action.params.insert("to".to_string(), first_action.params["to"].clone());

    let client = Arc::new(ControlledSubmitClient::new());
    for height in &heights[..4] {
        client.gate(*height);
    }
    let shared = concurrent_submit_shared(entries);
    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });

    for height in &heights[..4] {
        client.wait_started(*height).await;
    }
    client.release(heights[1]);
    client.wait_started(heights[5]).await;
    assert!(!client.has_started(heights[4]), "same-quota successor bypassed predecessor");

    client.release(heights[0]);
    client.wait_started(heights[4]).await;
    client.release(heights[2]);
    client.release(heights[3]);
    task.await.unwrap().unwrap();
    assert!(client.max_active() <= 4);
}

#[tokio::test]
async fn failed_conflict_predecessor_releases_successor() {
    let heights = [1_000_031, 1_000_032];
    let first = independent_entry(31, heights[0], START);
    let mut second = independent_entry(32, heights[1], START);
    second.origin = first.origin;
    second.nonce = first.nonce;
    let first_hash = first.tx_hash;
    let second_hash = second.tx_hash;
    let shared = concurrent_submit_shared([first, second]);
    let client = Arc::new(ControlledSubmitClient::new());
    client.fail(heights[0]);
    let trait_client: Arc<dyn RcsClient> = client.clone();

    assert!(worker::submit_once(&shared, &trait_client).await.is_err());
    assert_eq!(*client.started.lock().unwrap(), heights);
    let pool = shared.pool_lock();
    assert_eq!(pool.get(&first_hash).unwrap().status, BufferStatus::NotSubmitted);
    assert_eq!(pool.get(&second_hash).unwrap().status, BufferStatus::Submitted);
}

#[tokio::test]
async fn concurrent_partial_failure_keeps_independent_groups_isolated() {
    let heights = [1_000_061, 1_000_062, 1_000_063];
    let entries = heights
        .into_iter()
        .enumerate()
        .map(|(index, height)| independent_entry(u8::try_from(index + 61).unwrap(), height, START))
        .collect::<Vec<_>>();
    let hashes = entries.iter().map(|entry| entry.tx_hash).collect::<Vec<_>>();
    let shared = concurrent_submit_shared(entries);
    let client = Arc::new(ControlledSubmitClient::new());
    client.fail(heights[1]);
    let trait_client: Arc<dyn RcsClient> = client.clone();

    let error = worker::submit_once(&shared, &trait_client).await.unwrap_err();

    assert_eq!(error.to_string(), format!("rcs transport error: height {} failed", heights[1]));
    let pool = shared.pool_lock();
    assert_eq!(pool.get(&hashes[0]).unwrap().status, BufferStatus::Submitted);
    assert_eq!(pool.get(&hashes[1]).unwrap().status, BufferStatus::NotSubmitted);
    assert_eq!(pool.get(&hashes[2]).unwrap().status, BufferStatus::Submitted);
}

#[tokio::test]
async fn out_of_order_independent_responses_update_only_their_own_generations() {
    let heights = [1_000_071, 1_000_072];
    let entries = heights
        .into_iter()
        .enumerate()
        .map(|(index, height)| independent_entry(u8::try_from(index + 71).unwrap(), height, START))
        .collect::<Vec<_>>();
    let hashes = entries.iter().map(|entry| entry.tx_hash).collect::<Vec<_>>();
    let shared = concurrent_submit_shared(entries);
    let client = Arc::new(ControlledSubmitClient::new());
    client.gate(heights[0]);
    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });

    client.wait_started(heights[0]).await;
    client.wait_completed(heights[1]).await;
    {
        let pool = shared.pool_lock();
        assert_eq!(pool.get(&hashes[0]).unwrap().status, BufferStatus::NotSubmitted);
        assert_eq!(pool.get(&hashes[1]).unwrap().status, BufferStatus::Submitted);
    }
    client.release(heights[0]);
    task.await.unwrap().unwrap();
    let pool = shared.pool_lock();
    assert_eq!(pool.get(&hashes[0]).unwrap().status, BufferStatus::Submitted);
    assert_eq!(pool.get(&hashes[1]).unwrap().status, BufferStatus::Submitted);
}

#[tokio::test]
async fn all_failed_groups_are_attempted_and_lowest_height_error_is_returned() {
    let heights = [1_000_051, 1_000_052, 1_000_053];
    let entries = heights
        .into_iter()
        .enumerate()
        .map(|(index, height)| independent_entry(u8::try_from(index + 51).unwrap(), height, START));
    let shared = concurrent_submit_shared(entries);
    let client = Arc::new(ControlledSubmitClient::new());
    for height in heights {
        client.fail(height);
    }
    let trait_client: Arc<dyn RcsClient> = client.clone();

    let error = worker::submit_once(&shared, &trait_client).await.unwrap_err();

    assert_eq!(error.to_string(), format!("rcs transport error: height {} failed", heights[0]));
    let mut started = client.started.lock().unwrap().clone();
    started.sort_unstable();
    assert_eq!(started, heights);
    assert_eq!(shared.pool_lock().status_counts(), [3, 0, 0, 0, 0, 0]);
}

#[tokio::test]
async fn serial_fallback_preserves_height_order_and_lowest_error() {
    let heights = [1_000_081, 1_000_082, 1_000_083];
    let entries = heights
        .into_iter()
        .enumerate()
        .map(|(index, height)| independent_entry(u8::try_from(index + 81).unwrap(), height, START))
        .collect::<Vec<_>>();
    let hashes = entries.iter().map(|entry| entry.tx_hash).collect::<Vec<_>>();
    let shared = submit_shared_with_concurrency(entries, 1);
    let client = Arc::new(ControlledSubmitClient::new());
    client.fail(heights[0]);
    client.fail(heights[2]);
    let trait_client: Arc<dyn RcsClient> = client.clone();

    let error = worker::submit_once(&shared, &trait_client).await.unwrap_err();

    assert_eq!(*client.started.lock().unwrap(), heights);
    assert_eq!(error.to_string(), format!("rcs transport error: height {} failed", heights[0]));
    let pool = shared.pool_lock();
    assert_eq!(pool.get(&hashes[0]).unwrap().status, BufferStatus::NotSubmitted);
    assert_eq!(pool.get(&hashes[1]).unwrap().status, BufferStatus::Submitted);
    assert_eq!(pool.get(&hashes[2]).unwrap().status, BufferStatus::NotSubmitted);
}

#[tokio::test]
async fn transaction_inserted_during_slow_batch_is_processed_by_next_batch() {
    let heights = [1_000_091, 1_000_092];
    let first = independent_entry(91, heights[0], START);
    let second = independent_entry(92, heights[1], START);
    let hashes = [first.tx_hash, second.tx_hash];
    let shared = concurrent_submit_shared([first]);
    let client = Arc::new(ControlledSubmitClient::new());
    client.gate(heights[0]);
    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });

    client.wait_started(heights[0]).await;
    shared.pool_lock().insert(second);
    assert!(!client.has_started(heights[1]), "new lifecycle leaked into an existing snapshot");
    client.release(heights[0]);
    task.await.unwrap().unwrap();

    let trait_client: Arc<dyn RcsClient> = client.clone();
    worker::submit_once(&shared, &trait_client).await.unwrap();
    assert_eq!(*client.started.lock().unwrap(), heights);
    let pool = shared.pool_lock();
    assert_eq!(pool.get(&hashes[0]).unwrap().status, BufferStatus::Submitted);
    assert_eq!(pool.get(&hashes[1]).unwrap().status, BufferStatus::Submitted);
}

#[tokio::test]
async fn unknown_audit_type_is_a_global_submit_barrier() {
    let heights = [1_000_041, 1_000_042, 1_000_043, 1_000_044];
    let mut entries = heights
        .into_iter()
        .enumerate()
        .map(|(index, height)| independent_entry(u8::try_from(index + 41).unwrap(), height, START))
        .collect::<Vec<_>>();
    let custom_items = entries[2].actions.remove("quota").unwrap();
    entries[2].actions.insert("custom".to_string(), custom_items);

    let client = Arc::new(ControlledSubmitClient::new());
    client.gate(heights[0]);
    client.gate(heights[1]);
    client.gate(heights[2]);
    let shared = concurrent_submit_shared(entries);
    let task_shared = shared.clone();
    let task_client: Arc<dyn RcsClient> = client.clone();
    let task = tokio::spawn(async move { worker::submit_once(&task_shared, &task_client).await });

    client.wait_started(heights[0]).await;
    client.wait_started(heights[1]).await;
    assert!(!client.has_started(heights[2]));
    client.release(heights[0]);
    client.wait_completed(heights[0]).await;
    assert!(
        !client.has_started(heights[2]),
        "exclusive group started before prior segment drained"
    );
    client.release(heights[1]);
    client.wait_started(heights[2]).await;
    assert!(!client.has_started(heights[3]), "post-barrier group bypassed exclusive group");
    client.release(heights[2]);
    client.wait_started(heights[3]).await;
    task.await.unwrap().unwrap();
}

/// At startup, a supported protocol version installs the rules and latches `ready`.
#[tokio::test]
async fn load_and_install_installs_and_marks_ready() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), RuleSet::default(), false);

    let installed = worker::load_and_install(&shared, &client).await.expect("no transport error");
    assert!(installed, "supported protocol version installs");
    assert!(shared.ready.load(Ordering::Acquire));
    assert_eq!(shared.rules.read().unwrap().rules.len(), 1);
}

/// An unsupported `protocol_version` is rejected — `Ok(false)`, rules untouched,
/// `ready` not latched (initial loading keeps retrying; runtime keeps the old rules).
#[tokio::test]
async fn load_and_install_rejects_unsupported_protocol() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    mock.set_rules_protocol_version(2);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), RuleSet::default(), false);

    let installed = worker::load_and_install(&shared, &client).await.expect("no transport error");
    assert!(!installed, "unsupported protocol version is rejected");
    assert!(!shared.ready.load(Ordering::Acquire), "ready stays false");
    assert_eq!(shared.rules.read().unwrap().rules.len(), 0, "rules untouched");
}

/// An unreachable RCS surfaces as a transport error (the caller retries with backoff).
#[tokio::test]
async fn load_and_install_errors_when_unavailable() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_unavailable(true);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), RuleSet::default(), false);

    assert!(worker::load_and_install(&shared, &client).await.is_err());
    assert!(!shared.ready.load(Ordering::Acquire));
}

#[tokio::test]
async fn partial_query_failure_still_applies_successful_statuses() {
    let mock = Arc::new(MockRcsClient::new());
    mock.fail_query_status("pending");
    mock.register_query_state(golden::TX_A, "approved", Some(START as i64));
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    let mut entry = scenario_a_entry(golden::tx_a(), 1_000_000, START);
    entry.status = BufferStatus::Submitted;
    shared.pool.lock().unwrap().insert(entry);

    assert!(worker::query_once(&shared, &client).await.is_err());
    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_a()).unwrap().status,
        BufferStatus::Approved
    );
    assert_eq!(
        mock.queried_modes(),
        ["pending", "approved", "denied", "outdated"].map(|s| format!("status:{s}"))
    );
}

#[tokio::test]
async fn it_malformed_reload_keeps_old_rules() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), RuleSet::default(), false);
    assert!(worker::load_and_install(&shared, &client).await.unwrap());
    let previous = shared.rules.read().unwrap().clone();

    mock.set_rules_response_override(json!({
        "protocol_version": 1,
        "content_version": 2,
        "rules": [{
            "id": "malformed",
            "event_abis": {"e": {"type": "event", "name": "E", "anonymous": false}},
            "condition": true,
            "action": "deny"
        }]
    }));
    assert!(worker::load_and_install(&shared, &client).await.is_err());
    let current = shared.rules.read().unwrap();
    assert!(Arc::ptr_eq(&previous, &current));
    assert_eq!(current.content_version, 1);
}

/// A rule that deserializes fine (unlike `it_malformed_reload_keeps_old_rules`'s
/// wire-decode failure above) but fails `compile_rule`'s semantic validation (here: empty
/// `event_abis`, a "dead rule" that could never match anything) must reject the *entire*
/// update, not silently install the one valid sibling rule and drop the bad one. Regression
/// guard for a real bug report: a rule authored without `event_abis` to test its `condition`
/// in isolation was silently dropped, making the condition look broken when actually the rule
/// never loaded at all.
#[tokio::test]
async fn it_partially_invalid_reload_rejects_whole_batch_and_keeps_old_rules() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), RuleSet::default(), false);
    assert!(worker::load_and_install(&shared, &client).await.unwrap());
    let previous = shared.rules.read().unwrap().clone();
    assert_eq!(previous.rules.len(), 1);

    mock.set_rules_response_override(json!({
        "protocol_version": 1,
        "content_version": 2,
        "rules": [
            serde_json::from_str::<serde_json::Value>(golden::RULE_SCENARIO_B).unwrap(),
            json!({
                "id": "no-events-condition-only",
                "event_abis": {},
                "condition": {"==": [1, 1]},
                "action": "deny"
            })
        ]
    }));

    let err = worker::load_and_install(&shared, &client)
        .await
        .expect_err("a batch with any invalid rule must be rejected as a whole");
    assert!(matches!(err, crate::FilterError::InvalidRules(1, _)), "got {err:?}");

    let current = shared.rules.read().unwrap();
    assert!(
        Arc::ptr_eq(&previous, &current),
        "old rules must stay installed when the new batch is rejected"
    );
    assert_eq!(current.content_version, 1, "content_version must not advance on rejection");
}

#[tokio::test]
async fn it_missing_rules_never_marks_version_installed() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_response_override(json!({
        "protocol_version": 1,
        "content_version": 7
    }));
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), RuleSet::default(), false);

    assert!(worker::load_and_install(&shared, &client).await.is_err());
    assert!(!shared.ready.load(Ordering::Acquire));
    assert_eq!(shared.rules.read().unwrap().content_version, 0);
}

#[tokio::test]
async fn it_anonymous_audit_payload_matches_contract() {
    let raw = r#"{"id":"anonymous-transfer","event_abis":{"transfer":{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":true}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
    let rules = load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()]);
    let normal = transfer_log(golden::ONE_TOKEN);
    let logs = vec![Log {
        address: normal.address,
        data: LogData::new_unchecked(
            normal.data.topics()[1..].to_vec(),
            Bytes::copy_from_slice(&normal.data.data),
        ),
    }];
    let handle =
        FilterHandle::for_test(enabled_config(), rules.clone(), Arc::new(TestClock::new(START)));
    assert_eq!(handle.screen_tx(&scenario_a_input(golden::tx_a(), &logs)), Screen::AuditPending);
    let entry = handle.with_pool(|pool| pool.get(&golden::tx_a()).cloned()).unwrap();

    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), rules, true);
    shared.pool.lock().unwrap().insert(entry);
    worker::submit_once(&shared, &client).await.unwrap();

    let request = mock.last_submit().unwrap();
    let action = &request.txs[0].actions["quota"][0];
    assert_eq!(action.name, "transfer");
    assert_eq!(action.address, golden::TOKEN_X);
    assert_eq!(action.params["from"], golden::BRIDGE_ERC20);
    assert_eq!(action.params["to"], golden::RECIPIENT);
    assert_eq!(action.params["value"], golden::ONE_TOKEN);
}

#[tokio::test]
async fn it_transfer_batch_payload_preserves_abi_arrays_through_submit() {
    let rules = transfer_batch_rules();
    let logs = vec![log_builder::erc1155_transfer_batch(
        golden::token_x(),
        golden::origin(),
        golden::bridge_erc20(),
        golden::recipient(),
        vec![U256::from(7), U256::from(7)],
        vec![U256::from(100), U256::from(200)],
    )];
    let handle =
        FilterHandle::for_test(enabled_config(), rules.clone(), Arc::new(TestClock::new(START)));
    assert_eq!(handle.screen_tx(&scenario_a_input(golden::tx_a(), &logs)), Screen::AuditPending);
    let entry = handle.with_pool(|pool| pool.get(&golden::tx_a()).cloned()).unwrap();

    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), rules, true);
    shared.pool.lock().unwrap().insert(entry);
    worker::submit_once(&shared, &client).await.unwrap();

    let request = mock.last_submit().unwrap();
    let action = &request.txs[0].actions["quota"][0];
    assert_eq!(action.name, "transferBatch");
    assert_eq!(action.params["operator"], json!(golden::ORIGIN));
    assert_eq!(action.params["from"], json!(golden::BRIDGE_ERC20));
    assert_eq!(action.params["to"], json!(golden::RECIPIENT));
    assert_eq!(action.params["ids"], json!(["7", "7"]));
    assert_eq!(action.params["values"], json!(["100", "200"]));

    let wire = serde_json::to_value(&request).unwrap();
    assert_eq!(wire["txs"][0]["actions"]["quota"][0]["params"]["ids"], json!(["7", "7"]));
    assert_eq!(wire["txs"][0]["actions"]["quota"][0]["params"]["values"], json!(["100", "200"]));
}

#[test]
fn official_snapshot_compiles_and_matches_all_supported_transfer_shapes() {
    let rules = official_snapshot_rules();
    assert_eq!(rules.rules.len(), 1);
    assert_eq!(rules.rules[0].events.len(), 3);
    assert_eq!(rules.index.len(), 3);

    let logs = vec![
        log_builder::erc20_transfer(
            official_snapshot_token(),
            official_snapshot_from(),
            golden::recipient(),
            U256::from(100),
        ),
        log_builder::erc1155_transfer_single(
            official_snapshot_token(),
            golden::origin(),
            official_snapshot_from(),
            golden::recipient(),
            U256::from(7),
            U256::from(11),
        ),
        log_builder::erc1155_transfer_batch(
            official_snapshot_token(),
            golden::origin(),
            official_snapshot_from(),
            golden::recipient(),
            vec![U256::from(7), U256::from(7)],
            vec![U256::from(100), U256::from(200)],
        ),
    ];
    let MatchOutcome::Audit { actions, .. } =
        matching::evaluate(&rules, &scenario_a_input(golden::tx_a(), &logs))
    else {
        panic!("official snapshot must audit supported transfer logs")
    };
    let quota = &actions["quota"];
    assert_eq!(quota.len(), 3);
    assert_eq!(quota[0].name, "transfer");
    assert_eq!(quota[0].params["value"], json!("100"));
    assert_eq!(quota[1].name, "transferSingle");
    assert_eq!(quota[1].params["id"], json!("7"));
    assert_eq!(quota[1].params["value"], json!("11"));
    assert_eq!(quota[2].name, "transferBatch");
    assert_eq!(quota[2].params["ids"], json!(["7", "7"]));
    assert_eq!(quota[2].params["values"], json!(["100", "200"]));

    let nonmatching = [
        log_builder::erc20_transfer(
            golden::token_x(),
            official_snapshot_from(),
            golden::recipient(),
            U256::from(100),
        ),
        log_builder::erc20_transfer(
            official_snapshot_token(),
            golden::bridge_erc20(),
            golden::recipient(),
            U256::from(100),
        ),
        log_builder::erc1155_transfer_single(
            golden::token_x(),
            golden::origin(),
            official_snapshot_from(),
            golden::recipient(),
            U256::from(7),
            U256::from(11),
        ),
        log_builder::erc1155_transfer_batch(
            official_snapshot_token(),
            golden::origin(),
            golden::bridge_erc20(),
            golden::recipient(),
            vec![U256::from(7)],
            vec![U256::from(100)],
        ),
    ];
    for log in nonmatching {
        assert_eq!(
            matching::evaluate(&rules, &scenario_a_input(golden::tx_b(), &[log])),
            MatchOutcome::Allow
        );
    }
}

#[tokio::test]
async fn it_official_snapshot_all_event_shapes_submit_exact_wire() {
    let rules = official_snapshot_rules();
    let cases = vec![
        (
            golden::tx_a(),
            log_builder::erc20_transfer(
                official_snapshot_token(),
                official_snapshot_from(),
                golden::recipient(),
                U256::from(100),
            ),
            "transfer",
            json!({
                "from": format!("{:#x}", official_snapshot_from()),
                "to": golden::RECIPIENT,
                "value": "100"
            }),
        ),
        (
            golden::tx_b(),
            log_builder::erc1155_transfer_single(
                official_snapshot_token(),
                golden::origin(),
                official_snapshot_from(),
                golden::recipient(),
                U256::from(7),
                U256::from(11),
            ),
            "transferSingle",
            json!({
                "operator": golden::ORIGIN,
                "from": format!("{:#x}", official_snapshot_from()),
                "to": golden::RECIPIENT,
                "id": "7",
                "value": "11"
            }),
        ),
        (
            golden::tx_c(),
            log_builder::erc1155_transfer_batch(
                official_snapshot_token(),
                golden::origin(),
                official_snapshot_from(),
                golden::recipient(),
                vec![U256::from(7), U256::from(7)],
                vec![U256::from(100), U256::from(200)],
            ),
            "transferBatch",
            json!({
                "operator": golden::ORIGIN,
                "from": format!("{:#x}", official_snapshot_from()),
                "to": golden::RECIPIENT,
                "ids": ["7", "7"],
                "values": ["100", "200"]
            }),
        ),
    ];

    for (hash, log, name, params) in cases {
        let logs = [log];
        let handle = FilterHandle::for_test(
            enabled_config(),
            rules.clone(),
            Arc::new(TestClock::new(START)),
        );
        assert_eq!(handle.screen_tx(&scenario_a_input(hash, &logs)), Screen::AuditPending);
        let entry = handle.with_pool(|pool| pool.get(&hash).cloned()).unwrap();
        let mock = Arc::new(MockRcsClient::new());
        let client: Arc<dyn RcsClient> = mock.clone();
        let shared = shared_with(Arc::new(TestClock::new(START)), rules.clone(), true);
        shared.pool.lock().unwrap().insert(entry);
        worker::submit_once(&shared, &client).await.unwrap();

        let request = mock.last_submit().unwrap();
        assert_eq!(request.txs.len(), 1);
        assert_eq!(
            serde_json::to_value(&request.txs[0].actions["quota"][0]).unwrap(),
            json!({
                "name": name,
                "address": format!("{:#x}", official_snapshot_token()),
                "params": params
            })
        );
    }
}

#[test]
fn transfer_batch_approval_consistency_is_array_exact() {
    let rules = official_snapshot_rules();
    let original = vec![log_builder::erc1155_transfer_batch(
        official_snapshot_token(),
        golden::origin(),
        official_snapshot_from(),
        golden::recipient(),
        vec![U256::from(7), U256::from(7)],
        vec![U256::from(100), U256::from(200)],
    )];
    let approve = |logs: &[Log]| {
        let handle = FilterHandle::for_test(
            enabled_config(),
            rules.clone(),
            Arc::new(TestClock::new(START)),
        );
        assert_eq!(handle.screen_tx(&scenario_a_input(golden::tx_a(), logs)), Screen::AuditPending);
        handle.with_pool(|pool| {
            pool.apply_submit_response_current(&[golden::TX_A.to_string()], START);
            pool.apply_query_status_current(&golden::tx_a(), "approved", START);
        });
        handle
    };

    let unchanged = approve(&original);
    assert_eq!(
        unchanged.screen_tx(&scenario_a_input(golden::tx_a(), &original)),
        Screen::AuditApproved
    );

    let reordered = vec![log_builder::erc1155_transfer_batch(
        official_snapshot_token(),
        golden::origin(),
        official_snapshot_from(),
        golden::recipient(),
        vec![U256::from(7), U256::from(7)],
        vec![U256::from(200), U256::from(100)],
    )];
    let reordered_handle = approve(&original);
    assert_eq!(
        reordered_handle.screen_tx(&scenario_a_input(golden::tx_a(), &reordered)),
        Screen::Drop
    );

    let changed = vec![log_builder::erc1155_transfer_batch(
        official_snapshot_token(),
        golden::origin(),
        official_snapshot_from(),
        golden::recipient(),
        vec![U256::from(7), U256::from(7)],
        vec![U256::from(100), U256::from(201)],
    )];
    let changed_handle = approve(&original);
    assert_eq!(changed_handle.screen_tx(&scenario_a_input(golden::tx_a(), &changed)), Screen::Drop);
}

#[test]
fn later_physical_log_changes_approval_consistency_hash() {
    let rules = scenario_a_rules();
    let original = vec![transfer_log("100"), transfer_log("200")];
    let handle = FilterHandle::for_test(enabled_config(), rules, Arc::new(TestClock::new(START)));
    assert_eq!(
        handle.screen_tx(&scenario_a_input(golden::tx_a(), &original)),
        Screen::AuditPending
    );
    handle.with_pool(|pool| {
        pool.apply_submit_response_current(&[golden::TX_A.to_string()], START);
        pool.apply_query_status_current(&golden::tx_a(), "approved", START);
    });

    let changed_later_log = vec![transfer_log("100"), transfer_log("201")];
    assert_eq!(
        handle.screen_tx(&scenario_a_input(golden::tx_a(), &changed_later_log)),
        Screen::Drop
    );
}

/// Adjudication mapping: `Submitted → Pending` (any non-absent response) then
/// `Pending → Approved` (status=approved), driven through `query_once`.
#[tokio::test]
async fn query_once_maps_submitted_through_pending_to_approved() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    {
        let mut e = scenario_a_entry(golden::tx_a(), 1_000_000, START);
        e.status = BufferStatus::Submitted;
        shared.pool.lock().unwrap().insert(e);
    }

    mock.register_query_state(golden::TX_A, "pending", Some(START as i64));
    worker::query_once(&shared, &client).await.expect("query ok");
    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_a()).unwrap().status,
        BufferStatus::Pending
    );

    mock.register_query_state(golden::TX_A, "approved", Some(START as i64 + 2));
    worker::query_once(&shared, &client).await.expect("query ok");
    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_a()).unwrap().status,
        BufferStatus::Approved
    );
}

/// A `denied` status tombstones the entry as `Dropped` in place.
#[tokio::test]
async fn denied_emits_one_discard_event() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    {
        let mut e = scenario_a_entry(golden::tx_c(), 1_000_010, START);
        e.status = BufferStatus::Submitted;
        shared.pool.lock().unwrap().insert(e);
    }

    // Filter must not branch on `reason` — only log it.
    mock.register_query_state_with_reason(
        golden::TX_C,
        "denied",
        Some(START as i64),
        Some("daily quota exhausted: diagnostic text only"),
    );
    worker::query_once(&shared, &client).await.expect("query ok");

    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_c()).unwrap().status,
        BufferStatus::Dropped
    );
    assert_eq!(
        events.try_recv().unwrap(),
        TerminalEvent { tx_hash: golden::tx_c(), generation: 1, reason: TerminalReason::Denied }
    );
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn outdated_emits_discard_event() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let clock = Arc::new(TestClock::new(START));
    let shared = shared_with(clock.clone(), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    let mut entry = scenario_a_entry(golden::tx_d(), 1_000_020, START);
    entry.status = BufferStatus::Submitted;
    shared.pool.lock().unwrap().insert(entry);

    mock.register_query_state(golden::TX_D, "pending", Some(START as i64));
    worker::query_once(&shared, &client).await.expect("query ok");
    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_d()).unwrap().status,
        BufferStatus::Pending
    );
    assert!(events.try_recv().is_err());

    clock.set(START + 2);
    mock.register_query_state(golden::TX_D, "approved", Some(START as i64 + 2));
    worker::query_once(&shared, &client).await.expect("query ok");
    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_d()).unwrap().status,
        BufferStatus::Approved
    );
    assert!(events.try_recv().is_err());

    clock.set(START + 35);
    mock.register_query_state(golden::TX_D, "outdated", Some(START as i64 + 2));
    worker::query_once(&shared, &client).await.expect("query ok");

    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_d()).unwrap().status,
        BufferStatus::Dropped
    );
    assert_eq!(
        events.try_recv().unwrap(),
        TerminalEvent { tx_hash: golden::tx_d(), generation: 1, reason: TerminalReason::Outdated }
    );
    assert!(events.try_recv().is_err());
    clock.set(START + 55);
    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_d()).unwrap().status,
        BufferStatus::Dropped,
        "RCS release delay does not re-admit an outdated transaction"
    );
    let logs = [transfer_log(golden::ONE_TOKEN)];
    let handle = FilterHandle::for_test(
        enabled_config(),
        scenario_a_rules(),
        Arc::new(TestClock::new(START)),
    );
    handle.with_pool(|pool| {
        pool.insert(shared.pool.lock().unwrap().get(&golden::tx_d()).unwrap().clone());
    });
    assert_eq!(
        handle.screen_tx(&scenario_a_input(golden::tx_d(), &logs)),
        Screen::Drop,
        "outdated transactions can never be packaged"
    );
}

#[test]
fn fail_close_emits_discard_event() {
    let clock = Arc::new(TestClock::new(START));
    let shared = shared_with(clock.clone(), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    let mut entry = scenario_a_entry(golden::tx_c(), 1_000_010, START);
    entry.timeout_action = crate::rules::TimeoutAction::Deny;
    shared.pool.lock().unwrap().insert(entry);
    clock.set(START + 91);

    let (resolved, _) = worker::timeout_once(&shared);
    assert_eq!(resolved, vec![(golden::tx_c(), 1, crate::pool::Resolution::Discard)]);
    assert_eq!(
        events.try_recv().unwrap(),
        TerminalEvent {
            tx_hash: golden::tx_c(),
            generation: 1,
            reason: TerminalReason::FailCloseTimeout,
        }
    );
}

#[tokio::test]
async fn duplicate_terminal_updates_are_idempotent() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    let mut entry = scenario_a_entry(golden::tx_c(), 1_000_010, START);
    entry.status = BufferStatus::Submitted;
    shared.pool.lock().unwrap().insert(entry);
    mock.register_query_state(golden::TX_C, "denied", Some(START as i64));

    worker::query_once(&shared, &client).await.unwrap();
    worker::query_once(&shared, &client).await.unwrap();
    assert!(events.try_recv().is_ok());
    assert!(events.try_recv().is_err());
}

#[test]
fn channel_lag_reconciles_all_dropped_hashes() {
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    let mut a = scenario_a_entry(golden::tx_c(), 1_000_010, START);
    a.status = BufferStatus::Dropped;
    let mut b = scenario_a_entry(golden::tx_d(), 1_000_020, START);
    b.status = BufferStatus::Dropped;
    let mut pool = shared.pool.lock().unwrap();
    pool.insert(a);
    pool.insert(b);
    let hashes = pool.dropped_hashes();
    assert_eq!(hashes.len(), 2);
    assert!(hashes.contains(&golden::tx_c()));
    assert!(hashes.contains(&golden::tx_d()));
}

/// An absent tx_hash (RCS has never seen it / already swept) leaves the entry
/// untouched — no optimistic pass; the timeout task owns the fallback.
#[tokio::test]
async fn query_once_absent_status_leaves_entry_untouched() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    {
        let mut e = scenario_a_entry(golden::tx_a(), 1_000_000, START);
        e.status = BufferStatus::Submitted;
        shared.pool.lock().unwrap().insert(e);
    }

    // No query state registered → the mock returns an empty result set.
    worker::query_once(&shared, &client).await.expect("query ok");
    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_a()).unwrap().status,
        BufferStatus::Submitted
    );
}

// ============================================================================================
// Orchestration tests (real FilterHandle + spawned workers, paused virtual clock)
// ============================================================================================

/// Advances virtual time in one step so parked worker timers fire (tokio auto-advances to the
/// next deadline while the test task sleeps).
async fn spin(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// Spins until the filter latches `ready` (startup rule load completed), bounded.
async fn wait_ready(h: &FilterHandle) {
    for _ in 0..100 {
        if h.is_ready() {
            return;
        }
        spin(300).await;
    }
    panic!("filter never became ready");
}

/// Worked example, end-to-end through the real workers: screen → batch submit (payload
/// captured) → adjudication poll to Approved → pre-package consistency pass → release.
#[tokio::test(start_paused = true)]
async fn e2e_scenario_a_happy_path() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock);
    wait_ready(&h).await;

    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input = scenario_a_input(golden::tx_a(), &logs);
    assert_eq!(h.screen_tx(&input), Screen::AuditPending);
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::NotSubmitted));

    // batch_window is 200ms → the submit task fires.
    spin(500).await;
    let tx = &mock.last_submit().expect("submitted").txs[0];
    assert_eq!(tx.tx_hash, golden::TX_A);
    assert_eq!(tx.nonce, 1);
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Submitted));

    // RCS approves → the adjudication poll advances the entry.
    mock.register_query_state(golden::TX_A, "approved", Some(START as i64 + 2));
    spin(1500).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));

    // Pre-package re-simulation with unchanged logs → consistency pass → release.
    assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));
}

/// A `deny` rule match drops the tx locally and produces **zero** RCS
/// permission-request calls (no submit, no query).
#[tokio::test(start_paused = true)]
async fn scenario_b_deny_makes_zero_rcs_calls() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_B]);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock);
    wait_ready(&h).await;

    let logs = vec![log_builder::erc20_transfer(
        golden::token_x(),
        golden::blacklisted_from(),
        golden::recipient(),
        golden::one_token(),
    )];
    let input = ScreenInput {
        tx_hash: golden::tx_b(),
        origin: golden::origin_b(),
        tx_to: Some(golden::claim_contract()),
        nonce: 0,
        value: U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
    };
    assert_eq!(h.screen_tx(&input), Screen::Deny);
    assert!(h.buffer_status(&golden::tx_b()).is_none(), "deny tx is not buffered");

    // Let several submit/query cycles elapse — nothing to submit or query.
    spin(3000).await;
    assert_eq!(mock.call_count("submit"), 0);
    assert_eq!(mock.call_count("query"), 0);
}

/// When the node cannot reach RCS at startup, block production stays live with empty rules while the
/// rules worker retries, then readiness latches once RCS returns.
#[tokio::test(start_paused = true)]
async fn startup_retries_until_rcs_available_then_recovers() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_unavailable(true);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock);

    // While unavailable: filtering is not ready, but the background load keeps retrying.
    spin(1500).await;
    assert!(!h.is_ready(), "node must not be ready before rules load");
    assert!(mock.call_count("get_rules") >= 2, "startup load is retried");
    let logs = [transfer_log(golden::ONE_TOKEN)];
    assert_eq!(
        h.screen_tx(&scenario_a_input(golden::tx_a(), &logs)),
        Screen::Allow,
        "empty initial rules preserve block production"
    );

    // RCS recovers and the first rules snapshot becomes active.
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    mock.set_unavailable(false);
    spin(35_000).await; // cover the (capped 30s) backoff
    assert!(h.is_ready(), "filter recovers once RCS is reachable");
}

/// A `content_version` bump reloads rules for **new** transactions, while a transaction already
/// buffered keeps its cached decision and is not matched again after hot-reload.
#[tokio::test(start_paused = true)]
async fn hot_reload_applies_to_new_tx_but_not_buffered() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock);
    wait_ready(&h).await;

    // Buffer a scenario-a tx under the original rules.
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let buffered = scenario_a_input(golden::tx_a(), &logs);
    assert_eq!(h.screen_tx(&buffered), Screen::AuditPending);

    // Submit and approve the buffered transaction under the original rules.
    spin(500).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Submitted));
    mock.register_query_state(golden::TX_A, "approved", Some(START as i64 + 2));
    spin(1500).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));

    // Hot-swap to an empty rule set after approval.
    mock.set_rules_fixture(&[]);
    mock.bump_rules_version();
    spin(3000).await; // > rules_version_poll_interval (2s)
    assert!(mock.call_count("get_rules") >= 2, "hot-reload pulled the new rules");

    // A fresh tx now sees the empty rule set → allowed.
    let fresh = scenario_a_input(golden::tx_c(), &logs);
    assert_eq!(h.screen_tx(&fresh), Screen::Allow);

    // The already-buffered tx is re-evaluated with its submit-time rule snapshot, so unchanged
    // logs still pass even though the globally active rule set is now empty.
    assert_eq!(h.screen_tx(&buffered), Screen::AuditApproved);

    // The snapshot only pins rule semantics: the latest execution logs remain authoritative.
    let changed_logs = vec![transfer_log("2000000000000000000")];
    let changed = scenario_a_input(golden::tx_a(), &changed_logs);
    assert_eq!(h.screen_tx(&changed), Screen::Drop);
}

/// At runtime, a version bump advertising an unsupported
/// protocol is rejected — the node keeps the old rules and keeps mining (stays ready).
#[tokio::test(start_paused = true)]
async fn unsupported_protocol_at_runtime_keeps_old_rules() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock);
    wait_ready(&h).await;

    // Advertise an unsupported protocol on the next content version.
    mock.set_rules_protocol_version(2);
    mock.bump_rules_version();
    spin(3000).await;

    assert!(h.is_ready(), "node keeps mining");
    // Old scenario-a rules are still active → a matching fresh tx still audits.
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let fresh = scenario_a_input(golden::tx_c(), &logs);
    assert_eq!(h.screen_tx(&fresh), Screen::AuditPending);
}

/// An audit tx whose RCS is unavailable for 50s (< the 90s outer timeout)
/// must not be resolved early. Recovery preserves its original retry clock and allows RCS to
/// complete adjudication before the outer deadline.
#[tokio::test(start_paused = true)]
async fn rcs_failover_recovers_within_grace_period() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock.clone());
    wait_ready(&h).await;

    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input = scenario_a_input(golden::tx_a(), &logs);
    assert_eq!(h.screen_tx(&input), Screen::AuditPending);

    // RCS goes dark. Cumulative time reaches 50s — still inside the grace period.
    mock.set_unavailable(true);
    clock.set(START + 50);
    spin(2000).await;
    let status = h.buffer_status(&golden::tx_a()).unwrap();
    assert!(!status.is_terminal(), "no early resolution before 90s (got {status:?})");

    // The replacement RCS becomes active within the grace period and returns the adjudication.
    mock.set_unavailable(false);
    mock.register_query_state(golden::TX_A, "approved", Some(START as i64 + 50));
    for _ in 0..120 {
        if h.buffer_status(&golden::tx_a()) == Some(BufferStatus::Approved) {
            break;
        }
        spin(250).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));
    h.with_pool(|pool| {
        assert_eq!(pool.get(&golden::tx_a()).unwrap().first_not_submitted_at, START);
    });
    assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
}

#[tokio::test(start_paused = true)]
async fn approved_outage_past_total_timeout_still_checks_consistency() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock.clone());
    wait_ready(&h).await;

    let original_logs = vec![transfer_log(golden::ONE_TOKEN)];
    let original = scenario_a_input(golden::tx_a(), &original_logs);
    assert_eq!(h.screen_tx(&original), Screen::AuditPending);
    mock.register_query_state(golden::TX_A, "approved", Some(START as i64 + 2));
    for _ in 0..120 {
        if h.buffer_status(&golden::tx_a()) == Some(BufferStatus::Approved) {
            break;
        }
        spin(250).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));

    mock.set_unavailable(true);
    clock.set(START + 200);
    spin(1500).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));
    assert_eq!(h.screen_tx(&original), Screen::AuditApproved);

    let changed_logs = vec![transfer_log(golden::TWO_TOKENS)];
    let changed = scenario_a_input(golden::tx_a(), &changed_logs);
    assert_eq!(h.screen_tx(&changed), Screen::Drop);
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
}

/// Submit fails while RCS is down (tx stays
/// NotSubmitted and is retried), then the whole flow completes once RCS returns.
#[tokio::test(start_paused = true)]
async fn not_submitted_loop_recovers_after_rcs_returns() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock);
    wait_ready(&h).await;

    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input = scenario_a_input(golden::tx_a(), &logs);
    assert_eq!(h.screen_tx(&input), Screen::AuditPending);

    // Submit repeatedly fails → the tx stays NotSubmitted and is retried.
    mock.set_unavailable(true);
    spin(1000).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::NotSubmitted));
    assert!(mock.call_count("submit") >= 1, "submit was retried while unavailable");

    // RCS returns and approves → the flow completes to release.
    mock.set_unavailable(false);
    mock.register_query_state(golden::TX_A, "approved", Some(START as i64 + 2));
    // Advance in increments so the capped submit backoff and the following query poll both run.
    for _ in 0..120 {
        if h.buffer_status(&golden::tx_a()) == Some(BufferStatus::Approved) {
            break;
        }
        spin(250).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(
        h.buffer_status(&golden::tx_a()),
        Some(BufferStatus::Approved),
        "submit calls={}, query calls={}",
        mock.call_count("submit"),
        mock.call_count("query")
    );
    assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
}

/// A terminal tombstone is evicted by the timeout task once it is older than
/// `terminal_entry_retention`, so the pool does not grow without bound.
#[tokio::test(start_paused = true)]
async fn terminal_tombstone_is_pruned_after_retention() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock.clone());
    wait_ready(&h).await;

    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input = scenario_a_input(golden::tx_a(), &logs);
    assert_eq!(h.screen_tx(&input), Screen::AuditPending);

    // No adjudication ever arrives → the 90s outer timeout tombstones it (fail-open).
    clock.set(START + 91);
    spin(2000).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::TimedOutAllow));
    assert_eq!(h.buffered_len(), 1);

    // Past the 300s retention (measured from the terminal transition at +91) → evicted.
    clock.set(START + 91 + 301);
    spin(2000).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), None, "tombstone evicted");
    assert_eq!(h.buffered_len(), 0, "pool bounded");
}

fn approved_handle_for_lifecycle_test(logs: &[Log]) -> FilterHandle {
    let handle = FilterHandle::for_test(
        enabled_config(),
        scenario_a_rules(),
        Arc::new(TestClock::new(START)),
    );
    let input = scenario_a_input(golden::tx_a(), logs);
    assert_eq!(handle.screen_tx(&input), Screen::AuditPending);
    handle.with_pool(|pool| {
        pool.apply_submit_response_current(&[golden::TX_A.to_string()], START);
        pool.apply_query_status_current(&golden::tx_a(), "approved", START);
    });
    handle
}

#[test]
fn it_payload_cancel_does_not_cache_approval() {
    let original_logs = vec![transfer_log(golden::ONE_TOKEN)];
    let handle = approved_handle_for_lifecycle_test(&original_logs);
    assert_eq!(
        handle.screen_tx(&scenario_a_input(golden::tx_a(), &original_logs)),
        Screen::AuditApproved
    );
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));

    let changed_logs = vec![transfer_log(golden::TWO_TOKENS)];
    assert_eq!(handle.screen_tx(&scenario_a_input(golden::tx_a(), &changed_logs)), Screen::Drop);
}

#[test]
fn it_parent_change_rechecks_approval() {
    let parent_a_logs = vec![transfer_log(golden::ONE_TOKEN)];
    let handle = approved_handle_for_lifecycle_test(&parent_a_logs);
    assert_eq!(
        handle.screen_tx(&scenario_a_input(golden::tx_a(), &parent_a_logs)),
        Screen::AuditApproved
    );

    let parent_b_logs = vec![transfer_log(golden::THREE_TOKENS)];
    assert_eq!(handle.screen_tx(&scenario_a_input(golden::tx_a(), &parent_b_logs)), Screen::Drop);
}

#[test]
fn it_concurrent_payloads_do_not_share_permit() {
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let handle = Arc::new(approved_handle_for_lifecycle_test(&logs));
    let first = {
        let handle = handle.clone();
        let logs = logs.clone();
        std::thread::spawn(move || handle.screen_tx(&scenario_a_input(golden::tx_a(), &logs)))
    };
    let second = {
        let handle = handle.clone();
        std::thread::spawn(move || {
            let changed = vec![transfer_log(golden::TWO_TOKENS)];
            handle.screen_tx(&scenario_a_input(golden::tx_a(), &changed))
        })
    };

    let results = [first.join().unwrap(), second.join().unwrap()];
    assert!(results.contains(&Screen::Drop));
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
}

#[test]
fn it_canonical_notification_cleans_buffer() {
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let handle = approved_handle_for_lifecycle_test(&logs);
    assert_eq!(handle.screen_tx(&scenario_a_input(golden::tx_a(), &logs)), Screen::AuditApproved);
    assert_eq!(handle.remove_canonical_transactions(&[golden::tx_a()]), 1);
    assert_eq!(handle.buffer_status(&golden::tx_a()), None);
}

#[test]
fn it_reorg_rescreens_reinserted_tx() {
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let handle = approved_handle_for_lifecycle_test(&logs);
    assert_eq!(handle.remove_canonical_transactions(&[golden::tx_a()]), 1);
    assert_eq!(handle.screen_tx(&scenario_a_input(golden::tx_a(), &logs)), Screen::AuditPending);
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::NotSubmitted));
}

#[tokio::test]
async fn it_rcs_denied_removes_txpool_root() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    let mut entry = scenario_a_entry(golden::tx_c(), 1_000_010, START);
    entry.status = BufferStatus::Submitted;
    shared.pool.lock().unwrap().insert(entry);
    let txpool = Arc::new(Mutex::new(std::collections::HashSet::from([golden::tx_c()])));
    mock.register_query_state(golden::TX_C, "denied", Some(START as i64));

    worker::query_once(&shared, &client).await.unwrap();
    let event = events.recv().await.unwrap();
    txpool.lock().unwrap().remove(&event.tx_hash);
    assert!(!txpool.lock().unwrap().contains(&golden::tx_c()));
}

#[tokio::test]
async fn it_discard_parks_nonce_descendant() {
    let root = golden::tx_c();
    let descendant = golden::tx_d();
    let txpool = Arc::new(Mutex::new(std::collections::HashSet::from([root, descendant])));
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    let mut events = shared.terminal_events.subscribe();
    let mut entry = scenario_a_entry(root, 1_000_010, START);
    entry.status = BufferStatus::Submitted;
    shared.pool.lock().unwrap().insert(entry);
    shared.emit_terminal(TerminalEvent {
        tx_hash: root,
        generation: 1,
        reason: TerminalReason::Denied,
    });

    let event = events.recv().await.unwrap();
    txpool.lock().unwrap().remove(&event.tx_hash);
    let pool = txpool.lock().unwrap();
    assert!(!pool.contains(&root));
    assert!(pool.contains(&descendant), "nonce descendant remains available to be parked");
}

#[test]
fn it_rebroadcast_dropped_hash_is_reconciled() {
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let handle = FilterHandle::for_test(
        enabled_config(),
        scenario_a_rules(),
        Arc::new(TestClock::new(START)),
    );
    let input = scenario_a_input(golden::tx_a(), &logs);
    assert_eq!(handle.screen_tx(&input), Screen::AuditPending);
    handle.with_pool(|pool| {
        pool.apply_submit_response_current(&[golden::TX_A.to_string()], START);
        pool.apply_query_status_current(&golden::tx_a(), "denied", START);
    });

    assert_eq!(handle.pre_screen(&golden::tx_a()), PreScreen::Drop);
    assert_eq!(handle.dropped_hashes(), vec![golden::tx_a()]);
}

#[test]
fn it_pending_transactions_do_not_execute_each_flashblock() {
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let handle = FilterHandle::for_test(
        enabled_config(),
        scenario_a_rules(),
        Arc::new(TestClock::new(START)),
    );
    let input = scenario_a_input(golden::tx_a(), &logs);
    assert_eq!(handle.screen_tx(&input), Screen::AuditPending);
    let mut executions = 0;
    for _ in 0..360 {
        if handle.pre_screen(&golden::tx_a()) == PreScreen::Execute {
            executions += 1;
        }
    }
    assert_eq!(executions, 0);
}

/// An unsupported protocol advertised by the lightweight probe is filtered out
/// without ever pulling the full `/rules` body (busy-loop suppression), and — crucially — a
/// later protocol fix that keeps the *same* `content_version` recovers automatically (no sticky
/// rejected-version state).
#[tokio::test(start_paused = true)]
async fn unsupported_protocol_skips_pull_and_recovers_without_content_bump() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock);
    wait_ready(&h).await;
    let base = mock.call_count("get_rules"); // startup load

    // Advertise an unsupported protocol on a new content version → the probe filters it out;
    // the full body is never pulled, no matter how many polls elapse.
    mock.set_rules_protocol_version(2);
    mock.bump_rules_version();
    spin(8000).await;
    assert_eq!(
        mock.call_count("get_rules"),
        base,
        "unsupported protocol must not trigger a full /rules pull"
    );

    // Ops fixes the protocol back to a supported one WITHOUT bumping content_version → the next
    // poll must pull and install (recovery does not require a further content bump).
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_B]);
    mock.set_rules_protocol_version(1);
    spin(3000).await;
    assert_eq!(
        mock.call_count("get_rules"),
        base + 1,
        "recovers and pulls once protocol is supported"
    );
    // The new rule set is now active: a scenario-b blacklisted tx is denied.
    let logs = vec![log_builder::erc20_transfer(
        golden::token_x(),
        golden::blacklisted_from(),
        golden::recipient(),
        golden::one_token(),
    )];
    let input = ScreenInput {
        tx_hash: golden::tx_b(),
        origin: golden::origin_b(),
        tx_to: Some(golden::claim_contract()),
        nonce: 0,
        value: U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
    };
    assert_eq!(h.screen_tx(&input), Screen::Deny, "recovered rules took effect");
}

/// A poisoned pool mutex (a panic while the lock was held) does not permanently brick the
/// filter — the poison-recovering accessor still hands back the guard.
#[test]
fn pool_lock_recovers_from_poisoning() {
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    let pool = shared.pool.clone();
    let joined = std::thread::spawn(move || {
        let _g = pool.lock().unwrap();
        panic!("poison the pool lock");
    })
    .join();
    assert!(joined.is_err(), "the helper thread panicked while holding the lock");

    // The mutex is now poisoned; the recovering accessor must still work.
    assert!(shared.pool_lock().is_empty());
}

/// The rules `RwLock` also recovers from poisoning — `current_rules`/`rules_write` must
/// keep working so a panic while the write lock was held cannot brick rule access.
#[test]
fn rules_lock_recovers_from_poisoning() {
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    assert_eq!(shared.current_rules().rules.len(), 1);

    let rules = shared.rules.clone();
    let joined = std::thread::spawn(move || {
        let _g = rules.write().unwrap();
        panic!("poison the rules lock");
    })
    .join();
    assert!(joined.is_err(), "the helper thread panicked while holding the write lock");

    // Read side recovers.
    assert_eq!(shared.current_rules().rules.len(), 1);
    // Write side recovers (swap in an empty rule set).
    *shared.rules_write() = Arc::new(RuleSet::default());
    assert_eq!(shared.current_rules().rules.len(), 0);
}
