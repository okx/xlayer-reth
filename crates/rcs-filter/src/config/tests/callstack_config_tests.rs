//! Call-frame selector configuration: no protocol-version gate, and a call-frame observation
//! budget of at least one.

use super::*;

#[test]
fn callstack_needs_no_version_gate() {
    // `callstack` belongs to the one supported schema, so acceptance is decided by the rule
    // compiler alone and the only supported version is the minimum one.
    assert_eq!(MIN_PROTOCOL_VERSION, 1);
    assert!(is_supported_protocol(MIN_PROTOCOL_VERSION));
}

#[test]
fn callstack_observation_budget_must_be_at_least_one() {
    let zero = FilterConfig { max_callstack_observations_per_tx: 0, ..Default::default() };
    // Validated even while disabled: a zero budget is a startup misconfiguration either way.
    assert!(!zero.enabled);
    assert!(zero.validate().is_err());
    assert!(FilterConfig { max_callstack_observations_per_tx: 1, ..Default::default() }
        .validate()
        .is_ok());
    assert_eq!(FilterConfig::default().max_callstack_observations_per_tx, 10_000);
}
