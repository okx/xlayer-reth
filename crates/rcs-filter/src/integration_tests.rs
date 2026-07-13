//! Integration tests for the RCS-facing worker layer (spec §6.4 worked example + §7.2
//! integration checklist). Two flavours, both network-free and deterministic:
//!
//! - **Worker-function tests** drive the `pub(crate)` [`crate::worker`] entry points
//!   (`load_and_install` / `submit_once` / `query_once`) directly against a
//!   [`MockRcsClient`] — no spawning, no timers, fully deterministic. These pin the
//!   contract wire payload (§6.4 断言点1) and every status → buffer transition.
//! - **Orchestration tests** spawn the real [`FilterHandle`] with its four background
//!   tasks under a paused tokio clock, driving virtual time so the loops (startup blocking
//!   retry, hot-reload polling, batch submit, adjudication poll, timeout tick) actually run.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use alloy_primitives::{Log, B256, U256};

use crate::client::RcsClient;
use crate::config::FilterConfig;
use crate::handle::{FilterHandle, Screen, ScreenInput, Shared};
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

fn transfer_log(amount: &str) -> Log {
    log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        U256::from_str(amount).unwrap(),
    )
}

/// The scenario-a transaction (contract §4 constants): `tx_hash=TX_A`, `origin=ORIGIN`,
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
    Shared {
        config: enabled_config(),
        rules: Arc::new(RwLock::new(Arc::new(rules))),
        pool: Arc::new(Mutex::new(BufferPool::default())),
        clock,
        ready: Arc::new(AtomicBool::new(ready)),
    }
}

/// Builds a `NotSubmitted` buffer entry from a real scenario-a match so the submitted
/// payload carries the genuine `actions.quota` (not a hand-faked stub).
fn scenario_a_entry(tx_hash: B256, block_height: u64, now: u64) -> BufferEntry {
    let rules = scenario_a_rules();
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input = ScreenInput { block_height, ..scenario_a_input(tx_hash, &logs) };
    let (actions, timeout_action) = match matching::evaluate(&rules, &input) {
        MatchOutcome::Audit { actions, timeout_action } => (actions, timeout_action),
        other => panic!("expected Audit, got {other:?}"),
    };
    BufferEntry {
        tx_hash,
        origin: golden::origin(),
        contract_address: golden::claim_contract(),
        nonce: 1,
        block_height,
        status: BufferStatus::NotSubmitted,
        actions,
        quota_consistency_hash: B256::ZERO,
        timeout_action,
        first_not_submitted_at: now,
        last_transition_at: now,
    }
}

/// §6.4 断言点1 + FR-10 AC1: the submit payload matches contract §4 scenario a **verbatim**,
/// and an accepted hash advances `NotSubmitted → Submitted`.
#[tokio::test]
async fn submit_once_builds_contract_scenario_a_payload_verbatim() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    shared.pool.lock().unwrap().insert(scenario_a_entry(golden::tx_a(), 1_000_000, START));

    worker::submit_once(&shared, &client).await.expect("submit ok");

    // Wire payload — every field against contract §4 scenario a.
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

/// FR-5 AC2: a hash returned only in `rejected_malformed` (never in `accepted`) stays
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

/// FR-5: transactions are submitted grouped by `xlayer_block_height` — one request per height.
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
    assert!(heights.contains(&1_000_000) && heights.contains(&1_000_010));
    assert!(reqs.iter().all(|r| r.txs.len() == 1));
}

/// FR-2 startup load: a supported protocol version installs the rules and latches `ready`.
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

/// FR-3 §5.3: an unsupported `protocol_version` is rejected — `Ok(false)`, rules untouched,
/// `ready` not latched (startup would keep blocking; runtime would keep the old rules).
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

/// FR-2: an unreachable RCS surfaces as a transport error (the caller retries with backoff).
#[tokio::test]
async fn load_and_install_errors_when_unavailable() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_unavailable(true);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), RuleSet::default(), false);

    assert!(worker::load_and_install(&shared, &client).await.is_err());
    assert!(!shared.ready.load(Ordering::Acquire));
}

/// FR-5 adjudication mapping: `Submitted → Pending` (any non-absent response) then
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

/// §7.2 scenario c: a `denied` status tombstones the entry as `Dropped` in place.
#[tokio::test]
async fn query_once_denied_tombstones_dropped() {
    let mock = Arc::new(MockRcsClient::new());
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = shared_with(Arc::new(TestClock::new(START)), scenario_a_rules(), true);
    {
        let mut e = scenario_a_entry(golden::tx_c(), 1_000_010, START);
        e.status = BufferStatus::Submitted;
        shared.pool.lock().unwrap().insert(e);
    }

    // Filter must not branch on `reason` (contract §2.5) — only log it.
    mock.register_query_state(golden::TX_C, "denied", Some(START as i64));
    worker::query_once(&shared, &client).await.expect("query ok");

    assert_eq!(
        shared.pool.lock().unwrap().get(&golden::tx_c()).unwrap().status,
        BufferStatus::Dropped
    );
}

/// An absent tx_hash (RCS has never seen it / already swept, contract §2.5) leaves the entry
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

/// §6.4 worked example, end-to-end through the real workers: screen → batch submit (payload
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
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::ReleasePending));
}

/// §7.2 scenario b: a `deny` rule match drops the tx locally and produces **zero** RCS
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

/// §7.2 "节点启动时无法连接 RCS": startup blocks (not ready) and retries while RCS is
/// unavailable, then recovers once RCS returns.
#[tokio::test(start_paused = true)]
async fn startup_blocks_until_rcs_available_then_recovers() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_unavailable(true);
    let clock = Arc::new(TestClock::new(START));
    let h = FilterHandle::spawn(enabled_config(), mock.clone(), clock);

    // While unavailable: never ready, and the load is retried (backoff).
    spin(1500).await;
    assert!(!h.is_ready(), "node must not be ready before rules load");
    assert!(mock.call_count("get_rules") >= 2, "startup load is retried");

    // RCS recovers → startup completes.
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    mock.set_unavailable(false);
    spin(35_000).await; // cover the (capped 30s) backoff
    assert!(h.is_ready(), "filter recovers once RCS is reachable");
}

/// §7.2 "规则热更新": a `content_version` bump reloads rules for **new** transactions, while a
/// transaction already buffered keeps its cached decision (§3.1 — no re-match on hot-reload).
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

    // Hot-swap to an empty rule set.
    mock.set_rules_fixture(&[]);
    mock.bump_rules_version();
    spin(3000).await; // > rules_version_poll_interval (2s)
    assert!(mock.call_count("get_rules") >= 2, "hot-reload pulled the new rules");

    // A fresh tx now sees the empty rule set → allowed.
    let fresh = scenario_a_input(golden::tx_c(), &logs);
    assert_eq!(h.screen_tx(&fresh), Screen::Allow);

    // The already-buffered tx keeps its audit decision (dedup short-circuit, not re-evaluated
    // against the empty rules — it would otherwise become Allow).
    assert_eq!(h.screen_tx(&buffered), Screen::AuditPending);
}

/// §7.2 "protocol_version 不支持" (runtime): a version bump advertising an unsupported
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

/// §2.4 grace period: an audit tx whose RCS is unavailable for 50s (< the 90s outer timeout)
/// must **not** be resolved early; once cumulative time crosses 90s it fails open (its
/// `audit_timeout_action=allow`).
#[tokio::test(start_paused = true)]
async fn grace_period_no_early_timeout_then_fail_open_at_90s() {
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

    // Cross the 90s outer timeout → fail-open release.
    clock.set(START + 91);
    spin(2000).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::ReleasePending));
}

/// §7.2 "NotSubmitted 循环重试后最终恢复": submit fails while RCS is down (tx stays
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
    spin(1500).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));
    assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
}

/// #3: a terminal tombstone is evicted by the timeout task once it is older than
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
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::ReleasePending));
    assert_eq!(h.buffered_len(), 1);

    // Past the 300s retention (measured from the terminal transition at +91) → evicted.
    clock.set(START + 91 + 301);
    spin(2000).await;
    assert_eq!(h.buffer_status(&golden::tx_a()), None, "tombstone evicted");
    assert_eq!(h.buffered_len(), 0, "pool bounded");
}

/// #5 + F1: an unsupported protocol advertised by the lightweight probe is filtered out
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

/// #4: a poisoned pool mutex (a panic while the lock was held) does not permanently brick the
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

/// #4: the rules `RwLock` also recovers from poisoning — `current_rules`/`rules_write` must
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
