//! Startup validation of the call-frame observation budget while the filter is disabled.

use super::*;

#[test]
fn disabled_filter_with_zero_callstack_observation_budget_is_startup_error() {
    // Validated even with the master switch off, like the other budgets: a zero budget is a
    // startup misconfiguration whether or not screening is enabled.
    let mut args = crate::args::BuilderArgs::default().rcs_filter;
    args.enabled = false;
    args.max_callstack_observations_per_tx = 0;
    assert!(build_rcs_filter_config(&args).is_err());
    assert!(build_rcs_filter_handle(&args).is_err());
}
