use super::*;

fn enabled_args() -> crate::args::RcsFilterArgs {
    let mut args = crate::args::BuilderArgs::default().rcs_filter;
    args.enabled = true;
    args.rcs_base_url = Some("http://rcs.test".to_string());
    args.submit_max_concurrency = 1;
    args
}

#[test]
fn invalid_submit_concurrency_fails_before_workers_start() {
    for invalid in [0, rcs_filter::config::MAX_SUBMIT_CONCURRENCY + 1] {
        let mut args = enabled_args();
        args.submit_max_concurrency = invalid;
        let error = build_rcs_filter_handle(&args).unwrap_err();
        assert!(error.to_string().contains("submit_max_concurrency"));
    }
}

#[test]
fn submit_concurrency_is_mapped_into_filter_config() {
    let default = build_rcs_filter_config(&enabled_args()).unwrap().unwrap();
    assert_eq!(default.submit_max_concurrency, 1);

    let mut overridden = enabled_args();
    overridden.submit_max_concurrency = 4;
    let config = build_rcs_filter_config(&overridden).unwrap().unwrap();
    assert_eq!(config.submit_max_concurrency, 4);
}

#[test]
fn disabled_filter_bypasses_config_and_handle_construction() {
    let mut args = crate::args::BuilderArgs::default().rcs_filter;
    args.submit_max_concurrency = 0;
    assert!(build_rcs_filter_config(&args).unwrap().is_none());
    assert!(build_rcs_filter_handle(&args).unwrap().is_none());
}

#[test]
fn disabled_filter_with_zero_native_budget_is_startup_error() {
    let mut args = crate::args::BuilderArgs::default().rcs_filter;
    args.enabled = false;
    args.max_native_transfers_per_tx = 0;
    assert!(build_rcs_filter_config(&args).is_err());
    assert!(build_rcs_filter_handle(&args).is_err());
}

#[test]
fn disabled_filter_with_zero_event_bindings_is_startup_error() {
    let mut args = crate::args::BuilderArgs::default().rcs_filter;
    args.enabled = false;
    args.max_event_bindings_per_tx = 0;
    assert!(build_rcs_filter_config(&args).is_err());
    assert!(build_rcs_filter_handle(&args).is_err());
}

#[path = "flashblocks__rcs_filter_config_tests__callstack_budget_tests.rs"]
#[cfg(test)]
mod callstack_budget_tests;

#[test]
fn budgets_default_to_10_000_and_map_into_config() {
    let config = build_rcs_filter_config(&enabled_args()).unwrap().unwrap();
    assert_eq!(config.max_native_transfers_per_tx, 10_000);
    assert_eq!(config.max_event_bindings_per_tx, 10_000);
    assert_eq!(config.max_callstack_observations_per_tx, 10_000);

    let mut overridden = enabled_args();
    overridden.max_native_transfers_per_tx = 25;
    overridden.max_event_bindings_per_tx = 50;
    overridden.max_callstack_observations_per_tx = 75;
    let config = build_rcs_filter_config(&overridden).unwrap().unwrap();
    assert_eq!(config.max_native_transfers_per_tx, 25);
    assert_eq!(config.max_event_bindings_per_tx, 50);
    assert_eq!(
        config.max_callstack_observations_per_tx, 75,
        "the capture budget must reach the config, not silently keep its default"
    );
}
