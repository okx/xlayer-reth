use super::*;
use crate::rules::{load_rules, RawRule};
use crate::test_support::{golden, log_builder, TestClock};
use alloy_primitives::{Bytes, LogData};
use std::str::FromStr;

fn handle_with_scenario_a() -> FilterHandle {
    let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()]);
    FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(TestClock::new(1_751_000_000)))
}

fn scenario_a_input<'a>(logs: &'a [Log]) -> ScreenInput<'a> {
    ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: U256::ZERO,
        block_height: 1_000_000,
        logs,
        observations: Observations::NOT_CAPTURED,
    }
}

#[test]
fn audit_tx_enters_pool_as_pending() {
    let h = handle_with_scenario_a();
    let logs = vec![log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];
    assert_eq!(h.screen_tx(&scenario_a_input(&logs)), Screen::AuditPending);
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::NotSubmitted));
    // Second call is deduped (still pending, not re-added).
    assert_eq!(h.screen_tx(&scenario_a_input(&logs)), Screen::AuditPending);
    assert_eq!(h.buffered_len(), 1);
}

// A single audit rule with two anonymous zero-topic events: K matching logs yield K × K
// complete bindings, a precise knob for the configurable binding budget.
fn two_event_rules() -> RuleSet {
    let raw: RawRule = serde_json::from_value(serde_json::json!({
        "id": "binding-budget",
        "event_abis": {
            "a": {"type": "event", "name": "A", "inputs": [], "anonymous": true},
            "b": {"type": "event", "name": "B", "inputs": [], "anonymous": true}
        },
        "audit_types": ["custom"],
        "condition": false,
        "action": "audit"
    }))
    .unwrap();
    load_rules(1, 1, vec![raw])
}

#[test]
fn screen_tx_uses_configured_event_binding_limit() {
    let logs: Vec<Log> = (0..3)
        .map(|_| Log {
            address: golden::token_x(),
            data: LogData::new_unchecked(vec![], Bytes::new()),
        })
        .collect();
    let input = ScreenInput {
        tx_hash: B256::repeat_byte(0x11),
        origin: Address::repeat_byte(0x22),
        tx_to: None,
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };
    // Tight budget of 2: 3 × 3 = 9 bindings > 2 → fail-closed Deny (proves screen_tx reads
    // the configured limit, not a fixed constant).
    let tight = FilterHandle::for_test(
        FilterConfig { max_event_bindings_per_tx: 2, ..FilterConfig::default() },
        two_event_rules(),
        Arc::new(TestClock::new(1)),
    );
    assert_eq!(tight.screen_tx(&input), Screen::Deny);
    // Default budget (10,000): the same rule set does not overflow (condition false → Allow).
    let generous = FilterHandle::for_test(
        FilterConfig::default(),
        two_event_rules(),
        Arc::new(TestClock::new(1)),
    );
    assert_eq!(generous.screen_tx(&input), Screen::Allow);
}

#[test]
fn first_screen_matcher_overflow_emits_error_bounded_fields() {
    use std::io::Write;
    use std::sync::Mutex;

    // Minimal in-memory writer so the test can inspect the emitted diagnostics.
    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl tracing_subscriber::fmt::MakeWriter<'_> for BufWriter {
        type Writer = BufWriter;
        fn make_writer(&self) -> BufWriter {
            self.clone()
        }
    }

    let buf = BufWriter(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_target(true)
        .with_writer(buf.clone())
        .finish();

    // 3 zero-topic logs × two anonymous events = 9 complete bindings, over the tight budget of 2.
    let logs: Vec<Log> = (0..3)
        .map(|_| Log {
            address: golden::token_x(),
            data: LogData::new_unchecked(vec![], Bytes::new()),
        })
        .collect();
    let input = ScreenInput {
        tx_hash: B256::repeat_byte(0xAB),
        origin: Address::repeat_byte(0x22),
        tx_to: None,
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };
    let handle = FilterHandle::for_test(
        FilterConfig { max_event_bindings_per_tx: 2, ..FilterConfig::default() },
        two_event_rules(),
        Arc::new(TestClock::new(1)),
    );

    let decision = tracing::subscriber::with_default(subscriber, || handle.screen_tx(&input));
    assert_eq!(decision, Screen::Deny);

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(out.contains("ERROR"), "expected error-level diagnostic, got: {out}");
    assert!(out.contains("rcs_filter"), "expected target, got: {out}");
    assert!(out.contains("observed_count"), "expected observed_count field, got: {out}");
    assert!(out.contains("limit"), "expected limit field, got: {out}");
    // Bounded fields only: the rule id / binding content must never leak into the log.
    assert!(!out.contains("binding-budget"), "rule id must not be logged: {out}");
}

#[test]
fn max_native_transfers_accessor_returns_config_value() {
    let handle = FilterHandle::for_test(
        FilterConfig { max_native_transfers_per_tx: 7, ..FilterConfig::default() },
        RuleSet::default(),
        Arc::new(TestClock::new(1)),
    );
    assert_eq!(handle.max_native_transfers_per_tx(), 7);
}

#[test]
fn record_local_deny_creates_no_buffer_entry() {
    let handle = FilterHandle::for_test(
        FilterConfig::default(),
        RuleSet::default(),
        Arc::new(TestClock::new(1)),
    );
    handle.record_local_deny(LocalDenyReason::NativeTransferLimit);
    handle.record_local_deny(LocalDenyReason::ObservationInvariant);
    // A local deny records metrics only; it must never enter the pending buffer.
    assert_eq!(handle.buffered_len(), 0);
}

#[test]
fn deny_tx_returns_deny_without_buffering() {
    let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_B).unwrap()]);
    let h = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(TestClock::new(1)));
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
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(h.screen_tx(&input), Screen::Deny);
    assert!(h.buffer_status(&golden::tx_b()).is_none());
}

#[test]
fn excessive_complete_bindings_deny_without_buffering() {
    let raw: RawRule = serde_json::from_str(
        r#"{"id":"excessive","event_abis":{"a":{"type":"event","name":"A","inputs":[],"anonymous":true},"b":{"type":"event","name":"B","inputs":[],"anonymous":true}},"audit_types":["custom"],"condition":true,"action":"audit"}"#,
    )
    .unwrap();
    let rules = load_rules(1, 1, vec![raw]);
    // Pin the binding budget to 4,096 so the 65 × 65 = 4,225 bindings below overflow it
    // (the scenario predates the configurable budget, whose default is now 10,000).
    let h = FilterHandle::for_test(
        FilterConfig { max_event_bindings_per_tx: 4_096, ..FilterConfig::default() },
        rules,
        Arc::new(TestClock::new(1)),
    );
    let logs = (0..65)
        .map(|_| Log {
            address: golden::token_x(),
            data: LogData::new_unchecked(vec![], Bytes::new()),
        })
        .collect::<Vec<_>>();
    let input = scenario_a_input(&logs);

    assert_eq!(h.screen_tx(&input), Screen::Deny);
    assert!(h.buffer_status(&input.tx_hash).is_none());
}

#[test]
fn unmatched_tx_is_allowed() {
    let h = handle_with_scenario_a();
    assert_eq!(h.screen_tx(&scenario_a_input(&[])), Screen::Allow);
}

// ---- Builder-visible terminal tombstone semantics ------------------------------------

fn handle_and_clock(start: u64) -> (FilterHandle, std::sync::Arc<TestClock>) {
    let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()]);
    let clock = std::sync::Arc::new(TestClock::new(start));
    (FilterHandle::for_test(FilterConfig::default(), rules, clock.clone()), clock)
}

fn audit_input<'a>(
    tx_hash: B256,
    nonce: u64,
    block_height: u64,
    logs: &'a [Log],
) -> ScreenInput<'a> {
    ScreenInput {
        tx_hash,
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce,
        value: U256::ZERO,
        block_height,
        logs,
        observations: Observations::NOT_CAPTURED,
    }
}

fn transfer_log(amount: &str) -> Log {
    log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        U256::from_str(amount).unwrap(),
    )
}

/// A denied decision tombstones the tx as `Dropped`; it is never
/// packaged or re-submitted, and re-screening requests tx-pool eviction.
#[test]
fn scenario_c_denied_drops_and_never_resubmits() {
    let (h, _clock) = handle_and_clock(1_751_000_000);
    let logs = vec![transfer_log(golden::THREE_TOKENS)];
    let input = audit_input(golden::tx_c(), 2, 1_000_010, &logs);

    assert_eq!(h.screen_tx(&input), Screen::AuditPending);
    assert_eq!(h.buffer_status(&golden::tx_c()), Some(BufferStatus::NotSubmitted));

    let now = 1_751_000_000;
    h.with_pool(|p| {
        p.apply_submit_response_current(&[format!("{:#x}", golden::tx_c())], now);
        p.apply_query_status_current(&golden::tx_c(), "denied", now);
    });
    assert_eq!(h.buffer_status(&golden::tx_c()), Some(BufferStatus::Dropped));

    // Re-screen: still dropped; not re-admitted, not re-submitted, not re-queried.
    assert_eq!(h.screen_tx(&input), Screen::Drop);
    h.with_pool(|p| {
        assert!(p.not_submitted().is_empty());
        assert!(p.in_flight_hashes().is_empty());
    });
}

/// For a pending → approved → outdated flow, the first `outdated`
/// observation drops the tx; re-screening never returns `AuditApproved` and the
/// consistency check is never triggered (it went straight to `Dropped`).
#[test]
fn scenario_d_outdated_drops_without_consistency_check() {
    let (h, _clock) = handle_and_clock(1_751_000_000);
    let logs = vec![transfer_log(golden::TWO_TOKENS)];
    let input = audit_input(golden::tx_d(), 3, 1_000_020, &logs);

    assert_eq!(h.screen_tx(&input), Screen::AuditPending);

    let now = 1_751_000_000;
    h.with_pool(|p| {
        p.apply_submit_response_current(&[format!("{:#x}", golden::tx_d())], now);
        p.apply_query_status_current(&golden::tx_d(), "pending", now);
        p.apply_query_status_current(&golden::tx_d(), "approved", now);
        p.apply_query_status_current(&golden::tx_d(), "outdated", now);
    });
    assert_eq!(h.buffer_status(&golden::tx_d()), Some(BufferStatus::Dropped));
    assert_eq!(h.screen_tx(&input), Screen::Drop);
}

/// Fail-open behavior: an audit tx whose `audit_timeout_action=allow` exceeds the 90s
/// outer timeout is released into the block via `TimedOutAllow → AuditApproved`,
/// deterministically on every subsequent round (no clock reset, no re-buffer).
#[test]
fn timed_out_allow_is_terminal_allow() {
    let (h, clock) = handle_and_clock(1_751_000_000);
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);

    assert_eq!(h.screen_tx(&input), Screen::AuditPending);

    clock.advance(91);
    let now = clock.now_unix();
    h.with_pool(|p| {
        let resolved = p.check_timeouts(&FilterConfig::default(), now);
        assert_eq!(resolved.len(), 1);
    });
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::TimedOutAllow));
    assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
    // Deterministic on re-entry.
    assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
}

#[test]
fn cancelled_attempt_rechecks_changed_actions() {
    let (h, _clock) = handle_and_clock(1_751_000_000);
    let original_logs = vec![transfer_log(golden::ONE_TOKEN)];
    let original = audit_input(golden::tx_a(), 1, 1_000_000, &original_logs);
    assert_eq!(h.screen_tx(&original), Screen::AuditPending);
    h.with_pool(|p| {
        p.apply_submit_response_current(&[format!("{:#x}", golden::tx_a())], 1_751_000_000);
        p.apply_query_status_current(&golden::tx_a(), "approved", 1_751_000_000);
    });
    assert_eq!(h.screen_tx(&original), Screen::AuditApproved);
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));

    let changed_logs = vec![transfer_log(golden::TWO_TOKENS)];
    let changed = audit_input(golden::tx_a(), 1, 1_000_000, &changed_logs);
    assert_eq!(h.screen_tx(&changed), Screen::Drop);
}

#[test]
fn pre_screen_defers_all_inflight_states() {
    for target in ["not_submitted", "submitted", "pending"] {
        let (h, _) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::ONE_TOKEN)];
        let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);
        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
        h.with_pool(|pool| {
            if target != "not_submitted" {
                pool.apply_submit_response_current(
                    &[format!("{:#x}", golden::tx_a())],
                    1_751_000_000,
                );
            }
            if target == "pending" {
                pool.apply_query_status_current(&golden::tx_a(), "pending", 1_751_000_000);
            }
        });
        assert_eq!(h.pre_screen(&golden::tx_a()), PreScreen::Defer);
    }
}

#[test]
fn pre_screen_executes_approved_and_timeout_allow() {
    for target in ["approved", "timed_out_allow"] {
        let (h, _) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::ONE_TOKEN)];
        let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);
        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
        h.with_pool(|pool| {
            if target == "approved" {
                pool.apply_submit_response_current(
                    &[format!("{:#x}", golden::tx_a())],
                    1_751_000_000,
                );
                pool.apply_query_status_current(&golden::tx_a(), "approved", 1_751_000_000);
            } else {
                pool.check_timeouts(&FilterConfig::default(), 1_751_000_091);
            }
        });
        assert_eq!(h.pre_screen(&golden::tx_a()), PreScreen::Execute);
    }
}

#[test]
fn approved_can_become_outdated_before_canonical_cleanup() {
    let (h, _) = handle_and_clock(1_751_000_000);
    let logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);
    assert_eq!(h.screen_tx(&input), Screen::AuditPending);
    h.with_pool(|pool| {
        pool.apply_submit_response_current(&[format!("{:#x}", golden::tx_a())], 1_751_000_000);
        pool.apply_query_status_current(&golden::tx_a(), "approved", 1_751_000_000);
    });
    assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
    h.with_pool(|pool| {
        pool.apply_query_status_current(&golden::tx_a(), "outdated", 1_751_000_001);
    });
    assert_eq!(h.pre_screen(&golden::tx_a()), PreScreen::Drop);
}

/// Consistency mismatch: approved but the pre-package re-simulation yields
/// a different quota → hard drop, no RCS cancel, never re-admitted.
#[test]
fn approved_consistency_mismatch_hard_drops() {
    let (h, _clock) = handle_and_clock(1_751_000_000);
    let submit_logs = vec![transfer_log(golden::ONE_TOKEN)];
    let input_submit = audit_input(golden::tx_a(), 1, 1_000_000, &submit_logs);

    assert_eq!(h.screen_tx(&input_submit), Screen::AuditPending);
    let now = 1_751_000_000;
    h.with_pool(|p| {
        p.apply_submit_response_current(&[format!("{:#x}", golden::tx_a())], now);
        p.apply_query_status_current(&golden::tx_a(), "approved", now);
    });

    // Re-simulate with a different amount → quota hash mismatch → drop.
    let repackage_logs = vec![transfer_log(golden::TWO_TOKENS)];
    let input_repackage = audit_input(golden::tx_a(), 1, 1_000_000, &repackage_logs);
    assert_eq!(h.screen_tx(&input_repackage), Screen::Drop);
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
    // Never re-admitted / never released.
    assert_eq!(h.screen_tx(&input_repackage), Screen::Drop);
}

#[test]
fn approved_mismatch_after_total_timeout_still_drops() {
    let (h, clock) = handle_and_clock(1_751_000_000);
    let original_logs = vec![transfer_log(golden::ONE_TOKEN)];
    let original = audit_input(golden::tx_a(), 1, 1_000_000, &original_logs);
    assert_eq!(h.screen_tx(&original), Screen::AuditPending);
    h.with_pool(|pool| {
        pool.apply_submit_response_current(&[format!("{:#x}", golden::tx_a())], 1_751_000_000);
        pool.apply_query_status_current(&golden::tx_a(), "approved", 1_751_000_001);
    });

    clock.set(1_751_000_200);
    let changed_logs = vec![transfer_log(golden::TWO_TOKENS)];
    let changed = audit_input(golden::tx_a(), 1, 1_000_000, &changed_logs);
    assert_eq!(h.screen_tx(&changed), Screen::Drop);
    assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
}

#[test]
fn rules_state_updates_on_install_and_reload() {
    let (h, _) = handle_and_clock(1_751_000_000);
    assert!(h.is_ready());
    assert_eq!(h.shared.current_rules().content_version, 1);

    let replacement =
        load_rules(1, 2, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()]);
    h.install_rules(replacement);
    assert!(h.is_ready());
    assert_eq!(h.shared.current_rules().content_version, 2);
}
