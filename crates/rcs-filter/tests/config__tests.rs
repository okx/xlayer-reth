use super::*;

#[test]
fn defaults_match_expected_values() {
    let c = FilterConfig::default();
    assert!(!c.enabled);
    assert_eq!(c.batch_window, Duration::from_millis(200));
    assert_eq!(c.submit_max_concurrency, 1);
    assert_eq!(c.connect_timeout, Duration::from_secs(1));
    assert_eq!(c.request_timeout, Duration::from_secs(3));
    assert_eq!(c.retry_initial_backoff, Duration::from_millis(200));
    assert_eq!(c.retry_max_backoff, Duration::from_secs(5));
    assert_eq!(c.submitted_confirmation_timeout, Duration::from_secs(8));
    assert_eq!(c.risk_module_unresponsive_timeout, Duration::from_secs(20));
    assert_eq!(c.total_retry_timeout, Duration::from_secs(90));
    assert_eq!(c.rules_version_poll_interval, Duration::from_millis(2000));
    assert_eq!(c.terminal_entry_retention, Duration::from_secs(300));
}

#[test]
fn retention_must_exceed_total_retry_timeout() {
    let base =
        FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };
    // Equal → rejected.
    let eq = FilterConfig { terminal_entry_retention: Duration::from_secs(90), ..base.clone() };
    assert!(eq.validate().is_err());
    // Strictly less → rejected.
    let lt = FilterConfig { terminal_entry_retention: Duration::from_secs(30), ..base.clone() };
    assert!(lt.validate().is_err());
    // Strictly greater → accepted.
    let gt = FilterConfig { terminal_entry_retention: Duration::from_secs(91), ..base.clone() };
    assert!(gt.validate().is_ok());
    // Disabled → the check is skipped even with an otherwise-invalid retention.
    let disabled =
        FilterConfig { enabled: false, terminal_entry_retention: Duration::from_secs(1), ..base };
    assert!(disabled.validate().is_ok());
}

#[test]
fn protocol_version_is_exact_match() {
    // Exactly one version is accepted. Anything outside the set is refused outright, which
    // is what stops a rule authored against a different `callstack` contract from being
    // adjudicated under this one.
    assert_eq!(SUPPORTED_PROTOCOL_VERSIONS, &[1]);
    assert!(is_supported_protocol(1));
    assert!(!is_supported_protocol(0));
    assert!(!is_supported_protocol(2));
    assert!(!is_supported_protocol(3));
}

#[path = "config__tests__callstack_config_tests.rs"]
#[cfg(test)]
mod callstack_config_tests;

#[test]
fn enabled_requires_base_url() {
    let c = FilterConfig { enabled: true, ..Default::default() };
    assert!(c.validate().is_err());
    let c = FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };
    assert!(c.validate().is_ok());
}

#[test]
fn network_timeout_and_backoff_bounds_are_validated() {
    let base =
        FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };
    assert!(FilterConfig { connect_timeout: Duration::ZERO, ..base.clone() }.validate().is_err());
    assert!(FilterConfig {
        connect_timeout: Duration::from_secs(4),
        request_timeout: Duration::from_secs(3),
        ..base.clone()
    }
    .validate()
    .is_err());
    assert!(FilterConfig {
        retry_initial_backoff: Duration::from_secs(6),
        retry_max_backoff: Duration::from_secs(5),
        ..base
    }
    .validate()
    .is_err());
}

#[test]
fn scheduling_and_state_durations_must_be_non_zero() {
    let base =
        FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };
    for invalid in [
        FilterConfig { batch_window: Duration::ZERO, ..base.clone() },
        FilterConfig { submitted_confirmation_timeout: Duration::ZERO, ..base.clone() },
        FilterConfig { risk_module_unresponsive_timeout: Duration::ZERO, ..base.clone() },
        FilterConfig { total_retry_timeout: Duration::ZERO, ..base.clone() },
        FilterConfig { rules_version_poll_interval: Duration::ZERO, ..base.clone() },
        FilterConfig { terminal_entry_retention: Duration::ZERO, ..base.clone() },
    ] {
        assert!(invalid.validate().is_err());
    }
}

#[test]
fn submit_concurrency_boundaries_are_validated_when_enabled() {
    let base =
        FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };

    assert!(FilterConfig { submit_max_concurrency: 0, ..base.clone() }.validate().is_err());
    assert!(FilterConfig { submit_max_concurrency: 1, ..base.clone() }.validate().is_ok());
    assert!(FilterConfig { submit_max_concurrency: 4, ..base.clone() }.validate().is_ok());
    assert!(FilterConfig { submit_max_concurrency: MAX_SUBMIT_CONCURRENCY, ..base.clone() }
        .validate()
        .is_ok());
    assert!(FilterConfig { submit_max_concurrency: MAX_SUBMIT_CONCURRENCY + 1, ..base }
        .validate()
        .is_err());
}

#[test]
fn disabled_filter_skips_submit_concurrency_validation() {
    let config = FilterConfig { submit_max_concurrency: 0, ..Default::default() };
    assert!(config.validate().is_ok());
}

#[test]
fn default_config_budgets_are_10_000() {
    let c = FilterConfig::default();
    assert_eq!(c.max_native_transfers_per_tx, 10_000);
    assert_eq!(c.max_event_bindings_per_tx, 10_000);
}

#[test]
fn validate_rejects_zero_native_budget() {
    let c = FilterConfig { max_native_transfers_per_tx: 0, ..Default::default() };
    assert!(c.validate().is_err());
}

#[test]
fn validate_rejects_zero_event_bindings_budget() {
    let c = FilterConfig { max_event_bindings_per_tx: 0, ..Default::default() };
    assert!(c.validate().is_err());
}

#[test]
fn budgets_are_validated_even_when_disabled() {
    // Disabled filter still rejects a zero budget (validate-always semantics).
    let native =
        FilterConfig { enabled: false, max_native_transfers_per_tx: 0, ..Default::default() };
    assert!(native.validate().is_err());
    let bindings =
        FilterConfig { enabled: false, max_event_bindings_per_tx: 0, ..Default::default() };
    assert!(bindings.validate().is_err());
}
