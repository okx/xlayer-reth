//! Sequencer execution is a direct execution: the deposit is included, executed, receipted and
//! committed in the base order, and an RCS rule observing the same code changes none of it.

use crate::fixtures::*;

/// The sequencer state-order regression: a deposit whose execution a deny-worthy callstack rule
/// would object to produces byte-for-byte the same block facts as the run with no filter at all
/// — inclusion, order, gas, receipts, post-state bundle and the depositor nonce.
#[test]
fn sequencer_deposits_execute_identically_with_and_without_rcs_rules() {
    let approved = code_hash(&impl_code(0x01));
    for proxy_target in [UNAPPROVED_IMPL, APPROVED_IMPL] {
        let proxy_code = delegating_proxy_code(proxy_target, 1);

        let reference = {
            let ctx = ctx_with(None, vec![deposit_to(PROXY)]);
            let mut state = seeded_state(proxy_code.clone());
            run(&ctx, &mut state)
        };
        let filtered = {
            let filter = filter_with(vec![proxy_impl_allowlist_rule(&[approved])], test_config());
            let ctx = ctx_with(Some(filter), vec![deposit_to(PROXY)]);
            let mut state = seeded_state(proxy_code.clone());
            run(&ctx, &mut state)
        };

        assert_eq!(
            filtered.included.len(),
            1,
            "the deposit is still included (proxy target {proxy_target:?})"
        );
        assert_eq!(
            filtered.depositor_nonce, 1,
            "the deposit is executed and committed (proxy target {proxy_target:?})"
        );
        assert_eq!(
            filtered, reference,
            "sequencer execution must not depend on the rule set: inclusion, order, gas, \
             receipts and state must equal the no-filter run (proxy target {proxy_target:?})"
        );
    }
}
