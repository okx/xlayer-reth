//! Selector families: a rule must declare at least one event or call-frame selector.

use super::*;

#[test]
fn a_rule_with_no_selector_of_either_kind_is_rejected() {
    // An empty `event_abis` is no longer rejected on its own — a rule may select purely on
    // call frames. What must stay rejected is a rule with NEITHER selector family, which
    // could never match anything.
    let json = r#"{"id":"r","event_abis":{},"condition":true,"action":"deny"}"#;
    let reason = compile_rule(parse(json)).expect_err("a rule that can never match");
    assert!(reason.contains("neither event_abis nor callstack"), "{reason}");

    // The same rule with a `callstack` section is valid, and needs no version argument.
    let with_callstack = r#"{"id":"r","event_abis":{},"callstack":{"direct_delegatecalls":{"entry":"0x1111111111111111111111111111111111111111"}},"condition":true,"action":"deny"}"#;
    assert!(compile_rule(parse(with_callstack)).is_ok());
}
