//! Timed-out-allow local re-check: a fail-open timeout releases an audit transaction only while
//! the local matcher still decides allow/audit on the current execution. A local deny — or a
//! matcher outcome that cannot be decided — must drop it instead, without applying the
//! approved-path quota-consistency gate, resetting the retry clock, or re-submitting.
//!
//! These scenarios use only pre-existing crate APIs, so the whole module compiles against the
//! pre-fix base for the regression proof. The tests that exercise the new pool method and metric
//! live in the sibling `timed_out_allow_recheck_api_tests` module.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, Log, B256, U256};

use crate::callstack::{CallObservation, Observations, SnapshotStamp};
use crate::config::{FilterConfig, MIN_PROTOCOL_VERSION};
use crate::handle::{FilterHandle, PreScreen, Screen, ScreenInput};
use crate::pool::BufferStatus;
use crate::rules::{load_rules, RawRule, RuleSet};
use crate::test_support::{golden, log_builder, MockRcsClient, TestClock};

const START: u64 = 1_751_000_000;

/// The proxy named by the whitelist rule's `direct_delegatecalls` alias.
const P: Address = Address::repeat_byte(0x51);
/// The proxy's own executed code hash.
const H_P: B256 = B256::repeat_byte(0x5b);
/// An approved implementation code hash.
const H_OK: B256 = B256::repeat_byte(0x0a);
/// A non-whitelisted implementation code hash.
const H_ROGUE: B256 = B256::repeat_byte(0xee);

fn hex_hash(hash: B256) -> String {
    format!("{hash:#x}")
}

/// The whitelist deny rule: the code the proxy delegated into must be approved.
fn deny_rule_value() -> serde_json::Value {
    serde_json::json!({
        "id": "impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "p": format!("{P:#x}") } },
        "condition": { "!": { "in": [ { "var": "p.delegateCodeHash" }, [hex_hash(H_OK)] ] } },
        "action": "deny"
    })
}

fn deny_rule_json() -> String {
    serde_json::to_string(&deny_rule_value()).expect("the deny rule serializes")
}

fn scenario_a_raw() -> RawRule {
    serde_json::from_str(golden::RULE_SCENARIO_A).expect("the audit fixture deserializes")
}

fn deny_raw() -> RawRule {
    serde_json::from_value(deny_rule_value()).expect("the deny rule deserializes")
}

/// The audit fixture plus the whitelist deny rule, loaded as one snapshot.
fn timed_out_allow_rules() -> RuleSet {
    let rules = load_rules(MIN_PROTOCOL_VERSION, 1, vec![scenario_a_raw(), deny_raw()]);
    assert!(rules.rejected.is_empty(), "the fixture rules must load cleanly: {:?}", rules.rejected);
    rules
}

fn enabled_config() -> FilterConfig {
    FilterConfig { enabled: true, rcs_base_url: "http://mock".into(), ..Default::default() }
}

fn transfer_log(amount: &str) -> Log {
    log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        U256::from_str(amount).unwrap(),
    )
}

fn screen_input<'a>(logs: &'a [Log], observations: Observations<'a>) -> ScreenInput<'a> {
    ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: U256::ZERO,
        block_height: 1_000_000,
        logs,
        observations,
    }
}

/// One direct delegatecall observation for alias `p`, at the given implementation hash.
fn direct_delegatecall(impl_code_hash: B256) -> [CallObservation; 1] {
    [CallObservation::direct_delegatecall(P, H_P, P, impl_code_hash)]
}

/// A handle whose only lifecycle reached the terminal `TimedOutAllow` state through the real
/// timeout transition (`check_timeouts`, the same state-machine call the timeout worker performs).
fn handle_at_timed_out_allow(cfg: FilterConfig) -> FilterHandle {
    let rules = timed_out_allow_rules();
    let stamp = rules.stamp();
    let clock = Arc::new(TestClock::new(START));
    let handle = FilterHandle::for_test(cfg.clone(), rules, clock);
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let none: Vec<CallObservation> = Vec::new();
    let entry = screen_input(&logs, Observations::available(stamp, &none));
    assert_eq!(handle.screen_tx(&entry), Screen::AuditPending, "the audit rule buffers the tx");
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::NotSubmitted));
    handle.with_pool(|pool| {
        let resolved = pool.check_timeouts(&cfg, START + 91);
        assert_eq!(resolved.len(), 1, "the cumulative deadline resolves the entry");
    });
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::TimedOutAllow));
    assert_eq!(handle.pre_screen(&golden::tx_a()), PreScreen::Execute);
    handle
}

async fn wait_ready(handle: &FilterHandle) {
    for _ in 0..100 {
        if handle.is_ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!("the filter never installed the fixture rules");
}

/// F1 — a timed-out-allow transaction whose current execution matches the local deny must be
/// dropped, and the builder must see the terminal `Drop` (not `AuditApproved`).
#[tokio::test(start_paused = true)]
async fn timed_out_allow_deny_recheck_drops_after_the_real_timeout_entry() {
    let mock = Arc::new(MockRcsClient::new());
    let deny = deny_rule_json();
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A, deny.as_str()]);
    let clock = Arc::new(TestClock::new(START));
    let handle = FilterHandle::spawn(enabled_config(), mock.clone(), clock.clone());
    wait_ready(&handle).await;

    let stamp = handle.rule_snapshot().stamp();
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let none: Vec<CallObservation> = Vec::new();
    let entry = screen_input(&logs, Observations::available(stamp, &none));
    assert_eq!(handle.screen_tx(&entry), Screen::AuditPending);

    // RCS never adjudicates: the cumulative deadline releases it fail-open.
    clock.set(START + 91);
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::TimedOutAllow));

    // The upgrade executed before packaging: the implementation is not on the whitelist.
    let rogue = direct_delegatecall(H_ROGUE);
    let revisit = screen_input(&logs, Observations::available(stamp, &rogue));
    assert_eq!(handle.screen_tx(&revisit), Screen::Drop);
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
    assert_eq!(handle.pre_screen(&golden::tx_a()), PreScreen::Drop);
}

/// F2 — a legitimate whitelist entry stays released and remains releasable on every re-screen.
#[test]
fn timed_out_allow_keeps_a_whitelisted_implementation_released() {
    let handle = handle_at_timed_out_allow(enabled_config());
    let stamp = handle.rule_snapshot().stamp();
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let ok = direct_delegatecall(H_OK);
    let input = screen_input(&logs, Observations::available(stamp, &ok));
    assert_eq!(handle.screen_tx(&input), Screen::AuditApproved);
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::TimedOutAllow));
    assert_eq!(handle.screen_tx(&input), Screen::AuditApproved, "repeatable");
}

/// F3 — a plain quota audit transaction (alias observed nothing) still fails open.
#[test]
fn timed_out_allow_still_releases_a_plain_quota_transaction() {
    let handle = handle_at_timed_out_allow(enabled_config());
    let stamp = handle.rule_snapshot().stamp();
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let none: Vec<CallObservation> = Vec::new();
    let input = screen_input(&logs, Observations::available(stamp, &none));
    assert_eq!(handle.screen_tx(&input), Screen::AuditApproved);
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::TimedOutAllow));
}

/// F4 — the approved-path quota-consistency gate is not applied to the timeout branch.
#[test]
fn timed_out_allow_is_not_gated_by_quota_consistency() {
    let handle = handle_at_timed_out_allow(enabled_config());
    let stamp = handle.rule_snapshot().stamp();
    let logs = vec![transfer_log(golden::TWO_TOKENS)];
    let ok = direct_delegatecall(H_OK);
    let input = screen_input(&logs, Observations::available(stamp, &ok));
    assert_eq!(handle.screen_tx(&input), Screen::AuditApproved);
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::TimedOutAllow));
}

/// F5 — logs that no longer match any audit rule (`Ok(Allow)`) still release.
#[test]
fn timed_out_allow_releases_when_the_current_execution_matches_nothing() {
    let handle = handle_at_timed_out_allow(enabled_config());
    let stamp = handle.rule_snapshot().stamp();
    let none: Vec<CallObservation> = Vec::new();
    let input = screen_input(&[], Observations::available(stamp, &none));
    assert_eq!(handle.screen_tx(&input), Screen::AuditApproved);
}

/// F6 — an unavailable observation set must not be read as "nothing matched".
#[test]
fn timed_out_allow_drops_when_observations_are_unavailable() {
    let handle = handle_at_timed_out_allow(enabled_config());
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input = screen_input(&logs, Observations::NOT_CAPTURED);
    assert_eq!(handle.screen_tx(&input), Screen::Drop);
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
}

/// F7 — an observation set stamped with a foreign snapshot cannot be adjudicated.
#[test]
fn timed_out_allow_drops_on_a_foreign_snapshot_stamp() {
    let handle = handle_at_timed_out_allow(enabled_config());
    let own = handle.rule_snapshot().stamp();
    let foreign = SnapshotStamp { content_version: own.content_version + 1, ..own };
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let ok = direct_delegatecall(H_OK);
    let input = screen_input(&logs, Observations::available(foreign, &ok));
    assert_eq!(handle.screen_tx(&input), Screen::Drop);
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
}

/// F8 — a re-check that exceeds the configured binding budget fails closed.
#[test]
fn timed_out_allow_drops_on_a_recheck_binding_budget_overflow() {
    // Budget 2: the buffered screen charges one binding; the re-check sees two matching logs
    // plus the callstack candidate, i.e. three.
    let cfg = FilterConfig { max_event_bindings_per_tx: 2, ..enabled_config() };
    let handle = handle_at_timed_out_allow(cfg);
    let stamp = handle.rule_snapshot().stamp();
    let logs = vec![transfer_log(golden::ONE_TOKEN), transfer_log(golden::TWO_TOKENS)];
    let rogue = direct_delegatecall(H_ROGUE);
    let input = screen_input(&logs, Observations::available(stamp, &rogue));
    assert_eq!(handle.screen_tx(&input), Screen::Drop);
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
}

/// F9 — neither a release nor a deny re-check resets the retry clock or re-submits. The outcome
/// itself is pinned by F1–F8; this test pins the invariants around it.
#[tokio::test(start_paused = true)]
async fn timed_out_allow_recheck_preserves_the_clock_and_never_resubmits() {
    let mock = Arc::new(MockRcsClient::new());
    let deny = deny_rule_json();
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A, deny.as_str()]);
    let clock = Arc::new(TestClock::new(START));
    let handle = FilterHandle::spawn(enabled_config(), mock.clone(), clock.clone());
    wait_ready(&handle).await;

    let stamp = handle.rule_snapshot().stamp();
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let none: Vec<CallObservation> = Vec::new();
    let entry = screen_input(&logs, Observations::available(stamp, &none));
    assert_eq!(handle.screen_tx(&entry), Screen::AuditPending);

    // Keep RCS dark so no submission is ever recorded, then let the deadline elapse.
    mock.set_unavailable(true);
    clock.set(START + 91);
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    assert_eq!(handle.buffer_status(&golden::tx_a()), Some(BufferStatus::TimedOutAllow));

    let rogue = direct_delegatecall(H_ROGUE);
    let revisit = screen_input(&logs, Observations::available(stamp, &rogue));
    let decision = handle.screen_tx(&revisit);
    assert!(
        matches!(decision, Screen::Drop | Screen::AuditApproved),
        "the re-check must reach a terminal decision, got {decision:?}"
    );

    assert!(mock.submitted_requests().is_empty(), "the re-check never re-submits");
    handle.with_pool(|pool| {
        let entry = pool.get(&golden::tx_a()).expect("the tombstone stays in place");
        assert_eq!(entry.first_not_submitted_at, START, "the retry clock is untouched");
        assert!(entry.status.is_terminal(), "the lifecycle stays terminal: {:?}", entry.status);
        assert!(pool.not_submitted().is_empty(), "never re-buffered for submission");
        assert!(pool.in_flight_hashes().is_empty(), "never re-queried");
    });
}

// F13 — the existing terminal-allow and timeout behaviours are unchanged; they are covered by
// the module and integration suites in this crate and are deliberately not duplicated here.
