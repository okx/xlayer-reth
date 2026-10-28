//! Acceptance tests for how direct-delegatecall observations drive rule evaluation: candidate
//! suppression, the three binding classes, the combined budget, and fail-closed behaviour.
//!
//! These run the real matcher over real compiled rules; the observations are supplied directly so
//! each decision can be pinned independently of EVM execution.

use alloy_primitives::{Address, Bytes, Log, LogData, B256, U256};

use crate::callstack::{
    DirectDelegatecallObservation, Observations, ObservationsUnavailable, SnapshotStamp,
};
use crate::handle::ScreenInput;
use crate::matching::{try_evaluate, MatchError, MatchOutcome};
use crate::rules::{load_rules_from_raw, RuleSet, DEFAULT_MAX_EVENT_BINDINGS_PER_TX};

const PROXY1: &str = "0x1111111111111111111111111111111111111111";
const PROXY2: &str = "0x2222222222222222222222222222222222222222";
const UNSELECTED: &str = "0x3333333333333333333333333333333333333333";

/// An approved implementation's runtime code hash.
fn approved_hash() -> B256 {
    B256::repeat_byte(0xaa)
}

/// An unapproved implementation's runtime code hash.
fn unapproved_hash() -> B256 {
    B256::repeat_byte(0xbb)
}

fn hash_text(hash: B256) -> String {
    crate::callstack::lower_hash(hash)
}

fn address_of(s: &str) -> Address {
    s.parse().expect("valid address")
}

fn compile(rules: &[String]) -> RuleSet {
    let raw: Vec<Box<serde_json::value::RawValue>> =
        rules.iter().map(|r| serde_json::from_str(r).expect("syntactically valid rule")).collect();
    let set = load_rules_from_raw(1, 0, &raw);
    assert!(set.rejected.is_empty(), "fixture rules must be valid: {:?}", set.rejected);
    set
}

/// The canonical deny shape: deny unless the executed delegate code hash is in the allowlist.
///
/// This is the shape that makes candidate suppression load-bearing — comparing a value with null
/// yields false, so the negation would be TRUE for every transaction that never touched the proxy.
fn deny_unless_allowlisted(id: &str, alias: &str, address: &str, allowed: &[B256]) -> String {
    let list: Vec<String> = allowed.iter().map(|h| format!("\"{}\"", hash_text(*h))).collect();
    format!(
        r#"{{"id":"{id}","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"{alias}":"{address}"}}}},"condition":{{"!":{{"in":[{{"var":"{alias}.delegateCodeHash"}},[{}]]}}}},"action":"deny"}}"#,
        list.join(",")
    )
}

fn observation(
    context: &str,
    delegate: &str,
    delegate_hash: B256,
) -> DirectDelegatecallObservation {
    DirectDelegatecallObservation {
        context_address: address_of(context),
        context_code_hash: B256::repeat_byte(0xc0),
        delegate_address: address_of(delegate),
        delegate_code_hash: delegate_hash,
    }
}

/// An `Available` set correctly stamped for `rules`.
fn available(rules: &RuleSet, items: Vec<DirectDelegatecallObservation>) -> Observations {
    Observations::Available { stamp: rules.snapshot_stamp(), items }
}

fn input<'a>(logs: &'a [Log], observations: &'a Observations) -> ScreenInput<'a> {
    ScreenInput {
        tx_hash: B256::repeat_byte(0x11),
        origin: address_of("0x0404040404040404040404040404040404040404"),
        tx_to: Some(address_of(PROXY1)),
        nonce: 1,
        value: U256::ZERO,
        block_height: 1_000,
        logs,
        direct_delegatecalls: observations,
    }
}

fn evaluate(rules: &RuleSet, input: &ScreenInput) -> Result<MatchOutcome, MatchError> {
    try_evaluate(rules, input, DEFAULT_MAX_EVENT_BINDINGS_PER_TX)
}

// =============================================================================================
// the core allow/deny behaviour on executed delegate code identity.
// =============================================================================================

#[test]
fn proxy_delegatecalling_an_approved_implementation_allows() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    let obs = available(&rules, vec![observation(PROXY1, UNSELECTED, approved_hash())]);
    assert_eq!(evaluate(&rules, &input(&[], &obs)).unwrap(), MatchOutcome::Allow);
}

#[test]
fn proxy_delegatecalling_an_unapproved_implementation_denies() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    let obs = available(&rules, vec![observation(PROXY1, UNSELECTED, unapproved_hash())]);
    assert_eq!(evaluate(&rules, &input(&[], &obs)).unwrap(), MatchOutcome::Deny);
}

#[test]
fn two_allowed_delegate_hashes_both_allow() {
    let second = B256::repeat_byte(0xcc);
    let rules =
        compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash(), second])]);
    let obs = available(
        &rules,
        vec![
            observation(PROXY1, UNSELECTED, approved_hash()),
            observation(PROXY1, UNSELECTED, second),
        ],
    );
    assert_eq!(evaluate(&rules, &input(&[], &obs)).unwrap(), MatchOutcome::Allow);
}

#[test]
fn and_t8_any_single_unallowed_delegate_hash_denies() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    // Several delegatecalls, only one of them unapproved: one denying candidate is enough.
    let obs = available(
        &rules,
        vec![
            observation(PROXY1, UNSELECTED, approved_hash()),
            observation(PROXY1, UNSELECTED, approved_hash()),
            observation(PROXY1, UNSELECTED, unapproved_hash()),
        ],
    );
    assert_eq!(evaluate(&rules, &input(&[], &obs)).unwrap(), MatchOutcome::Deny);
}

#[test]
fn an_executed_then_reverted_observation_still_denies() {
    // Observations are execution facts and are never truncated on revert, which is the whole
    // point of the upgrade → execute → rollback threat: storage reads approved code at both
    // observation points while unapproved code actually ran.
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    let obs = available(&rules, vec![observation(PROXY1, UNSELECTED, unapproved_hash())]);
    assert_eq!(evaluate(&rules, &input(&[], &obs)).unwrap(), MatchOutcome::Deny);
}

#[test]
fn the_context_code_hash_can_also_be_constrained() {
    let proxy_code = B256::repeat_byte(0xc0);
    let rule = format!(
        r#"{{"id":"paired","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"condition":{{"and":[{{"==":[{{"var":"proxy1.contextCodeHash"}},"{}"]}},{{"!":{{"in":[{{"var":"proxy1.delegateCodeHash"}},["{}"]]}}}}]}},"action":"deny"}}"#,
        hash_text(proxy_code),
        hash_text(approved_hash())
    );
    let rules = compile(&[rule]);
    // Matching proxy code + unapproved delegate → deny.
    let deny = available(&rules, vec![observation(PROXY1, UNSELECTED, unapproved_hash())]);
    assert_eq!(evaluate(&rules, &input(&[], &deny)).unwrap(), MatchOutcome::Deny);
    // Different proxy code → the `and` fails, so no deny.
    let mut other = observation(PROXY1, UNSELECTED, unapproved_hash());
    other.context_code_hash = B256::repeat_byte(0x0d);
    let allow = available(&rules, vec![other]);
    assert_eq!(evaluate(&rules, &input(&[], &allow)).unwrap(), MatchOutcome::Allow);
}

#[test]
fn the_four_variables_are_bound_to_the_real_executed_values() {
    let rule = format!(
        r#"{{"id":"all-four","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"condition":{{"and":[{{"==":[{{"var":"proxy1.contextAddress"}},"{PROXY1}"]}},{{"==":[{{"var":"proxy1.delegateAddress"}},"{UNSELECTED}"]}},{{"==":[{{"var":"proxy1.contextCodeHash"}},"{}"]}},{{"==":[{{"var":"proxy1.delegateCodeHash"}},"{}"]}}]}},"action":"deny"}}"#,
        hash_text(B256::repeat_byte(0xc0)),
        hash_text(unapproved_hash())
    );
    let rules = compile(&[rule]);
    let obs = available(&rules, vec![observation(PROXY1, UNSELECTED, unapproved_hash())]);
    assert_eq!(
        evaluate(&rules, &input(&[], &obs)).unwrap(),
        MatchOutcome::Deny,
        "all four variables must bind to the observation's real values"
    );
}

// =============================================================================================
// candidate suppression. These are the anti-goal A1 guards: a rule with no
// matching direct delegatecall must NOT deny unrelated transactions.
// =============================================================================================

#[test]
fn no_delegatecall_at_all_does_not_evaluate_the_rule_and_allows() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    // Capture RAN and produced nothing — a clean no-match, not an unavailable set.
    let obs = available(&rules, vec![]);
    assert_eq!(
        evaluate(&rules, &input(&[], &obs)).unwrap(),
        MatchOutcome::Allow,
        "a negated allowlist must not become a universal deny"
    );
}

#[test]
fn a_rule_declaring_two_aliases_needs_an_observation_for_each() {
    let rule = format!(
        r#"{{"id":"pair","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}","proxy2":"{PROXY2}"}}}},"condition":{{"!":{{"in":[{{"var":"proxy1.delegateCodeHash"}},["{}"]]}}}},"action":"deny"}}"#,
        hash_text(approved_hash())
    );
    let rules = compile(&[rule]);

    // Only proxy1 delegatecalled, with an UNAPPROVED hash that would otherwise deny.
    let partial = available(&rules, vec![observation(PROXY1, UNSELECTED, unapproved_hash())]);
    assert_eq!(
        evaluate(&rules, &input(&[], &partial)).unwrap(),
        MatchOutcome::Allow,
        "all declared aliases must have an observation; independent monitoring needs separate rules"
    );

    // With both present the rule is evaluated and denies.
    let both = available(
        &rules,
        vec![
            observation(PROXY1, UNSELECTED, unapproved_hash()),
            observation(PROXY2, UNSELECTED, approved_hash()),
        ],
    );
    assert_eq!(evaluate(&rules, &input(&[], &both)).unwrap(), MatchOutcome::Deny);
}

#[test]
fn an_unselected_contracts_delegatecall_is_not_bound_to_any_alias() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    // The only observation belongs to a contract the rule never selected.
    let obs = available(&rules, vec![observation(UNSELECTED, PROXY2, unapproved_hash())]);
    assert_eq!(
        evaluate(&rules, &input(&[], &obs)).unwrap(),
        MatchOutcome::Allow,
        "an unselected contract's delegatecall must not satisfy a declared alias"
    );
}

#[test]
fn a_suppressed_rule_does_not_block_a_later_rule_from_denying() {
    let suppressed = deny_unless_allowlisted("a", "proxy1", PROXY1, &[approved_hash()]);
    let firing = deny_unless_allowlisted("b", "proxy2", PROXY2, &[approved_hash()]);
    let rules = compile(&[suppressed, firing]);
    let obs = available(&rules, vec![observation(PROXY2, UNSELECTED, unapproved_hash())]);
    assert_eq!(evaluate(&rules, &input(&[], &obs)).unwrap(), MatchOutcome::Deny);
}

// =============================================================================================
// fail-closed on an untrustworthy observation set.
// =============================================================================================

#[test]
fn every_unavailable_reason_fails_closed_when_a_rule_declares_an_alias() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    for reason in [
        ObservationsUnavailable::BudgetExceeded { attempted: 10_001, limit: 10_000 },
        ObservationsUnavailable::IncompleteFrames,
        ObservationsUnavailable::NotCaptured,
        ObservationsUnavailable::SnapshotMismatch,
        ObservationsUnavailable::OwnCodeIndeterminate,
        ObservationsUnavailable::ResultAlreadyTaken,
        ObservationsUnavailable::EngineError,
    ] {
        let obs = Observations::Unavailable(reason);
        let error = evaluate(&rules, &input(&[], &obs))
            .expect_err("an unavailable set must never resolve to a verdict");
        assert_eq!(
            error,
            MatchError::ObservationsUnavailable { reason },
            "reason must be preserved, not collapsed"
        );
    }
}

#[test]
fn a_stamp_mismatch_fails_closed_rather_than_being_treated_as_empty() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    // A plausible but WRONG stamp: right digest, different content version.
    let stale = SnapshotStamp { content_version: 99, ..rules.snapshot_stamp() };
    let obs = Observations::Available {
        stamp: stale,
        items: vec![observation(PROXY1, UNSELECTED, approved_hash())],
    };
    assert_eq!(
        evaluate(&rules, &input(&[], &obs)).expect_err("must fail closed"),
        MatchError::ObservationsUnavailable { reason: ObservationsUnavailable::SnapshotMismatch },
        "a configuration error must never become a clean allow"
    );
}

#[test]
fn a_digest_mismatch_alone_also_fails_closed() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    let wrong =
        SnapshotStamp { alias_set_digest: B256::repeat_byte(0xfe), ..rules.snapshot_stamp() };
    let obs = Observations::Available { stamp: wrong, items: vec![] };
    assert!(evaluate(&rules, &input(&[], &obs)).is_err());
}

#[test]
fn an_unavailable_set_is_ignored_when_no_rule_declares_an_alias() {
    // An event-only deployment keeps today's exact behaviour and pays nothing — this is the
    // boundary that keeps the feature free for deployments that do not use it.
    let event_abi = r#"{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":false}],"anonymous":false}"#;
    let rules = compile(&[format!(
        r#"{{"id":"event-only","event_abis":{{"e":{event_abi}}},"condition":true,"action":"deny"}}"#
    )]);
    assert!(!rules.declares_any_alias());
    for reason in [ObservationsUnavailable::NotCaptured, ObservationsUnavailable::EngineError] {
        let obs = Observations::Unavailable(reason);
        // `condition: true` with a null-bound missing event still denies, exactly as before.
        assert_eq!(evaluate(&rules, &input(&[], &obs)).unwrap(), MatchOutcome::Deny);
    }
}

// =============================================================================================
// event-only behaviour is unchanged (Emergency Deny-All preserved).
// =============================================================================================

#[test]
fn an_event_only_rule_with_a_missing_event_is_still_evaluated() {
    let event_abi = r#"{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":false}],"anonymous":false}"#;
    let rules = compile(&[format!(
        r#"{{"id":"deny-all","event_abis":{{"e":{event_abi}}},"condition":true,"action":"deny"}}"#
    )]);
    let obs = available(&rules, vec![]);
    assert_eq!(
        evaluate(&rules, &input(&[], &obs)).unwrap(),
        MatchOutcome::Deny,
        "a missing event contributes a null candidate and the rule is STILL evaluated"
    );
}

// =============================================================================================
// mixed rules require BOTH binding kinds.
// =============================================================================================

fn transfer_log(from: Address, to: Address, value: u64) -> Log {
    let topic0 = alloy_primitives::keccak256("Transfer(address,address,uint256)");
    let topics = vec![topic0, from.into_word(), to.into_word()];
    let data = Bytes::from(U256::from(value).to_be_bytes::<32>().to_vec());
    Log { address: address_of(PROXY2), data: LogData::new_unchecked(topics, data) }
}

const TRANSFER_ABI: &str = r#"{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":false}"#;

/// A mixed rule whose condition inspects ONLY the callstack side. Before mixed rules required
/// both binding kinds, this denied with no event present at all.
fn mixed_rule_inspecting_only_the_callstack() -> String {
    format!(
        r#"{{"id":"mixed","event_abis":{{"transfer":{TRANSFER_ABI}}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"condition":{{"!":{{"in":[{{"var":"proxy1.delegateCodeHash"}},["{}"]]}}}},"action":"deny"}}"#,
        hash_text(approved_hash())
    )
}

#[test]
fn a_mixed_rule_with_no_event_is_not_evaluated_even_with_a_denying_observation() {
    let rules = compile(&[mixed_rule_inspecting_only_the_callstack()]);
    let obs = available(&rules, vec![observation(PROXY1, UNSELECTED, unapproved_hash())]);
    assert_eq!(
        evaluate(&rules, &input(&[], &obs)).unwrap(),
        MatchOutcome::Allow,
        "a mixed rule requires BOTH binding kinds to be physically present"
    );
}

#[test]
fn a_mixed_rule_with_no_alias_observation_is_not_evaluated_and_fires_when_both_are_present() {
    let rules = compile(&[mixed_rule_inspecting_only_the_callstack()]);
    let logs = vec![transfer_log(address_of(PROXY1), address_of(PROXY2), 5)];

    // Event present, no alias observation → suppressed.
    let no_alias = available(&rules, vec![]);
    assert_eq!(evaluate(&rules, &input(&logs, &no_alias)).unwrap(), MatchOutcome::Allow);

    // Both present → evaluated, and denies.
    let both = available(&rules, vec![observation(PROXY1, UNSELECTED, unapproved_hash())]);
    assert_eq!(evaluate(&rules, &input(&logs, &both)).unwrap(), MatchOutcome::Deny);
}

#[test]
fn a_mixed_rule_visits_the_full_event_times_alias_product() {
    // Two events × two observations: the denying combination is the LAST one, so it is only found
    // if the whole product is walked rather than just the first pairing.
    let rule = format!(
        r#"{{"id":"product","event_abis":{{"transfer":{TRANSFER_ABI}}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"condition":{{"and":[{{"==":[{{"var":"transfer.value"}},"7"]}},{{"==":[{{"var":"proxy1.delegateCodeHash"}},"{}"]}}]}},"action":"deny"}}"#,
        hash_text(unapproved_hash())
    );
    let rules = compile(&[rule]);
    let logs = vec![
        transfer_log(address_of(PROXY1), address_of(PROXY2), 5),
        transfer_log(address_of(PROXY1), address_of(PROXY2), 7),
    ];
    let obs = available(
        &rules,
        vec![
            observation(PROXY1, UNSELECTED, approved_hash()),
            observation(PROXY1, UNSELECTED, unapproved_hash()),
        ],
    );
    assert_eq!(evaluate(&rules, &input(&logs, &obs)).unwrap(), MatchOutcome::Deny);
}

// =============================================================================================
// the combined budget bounds the full event × alias product.
// =============================================================================================

#[test]
fn the_alias_dimension_counts_toward_the_binding_budget() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    let items: Vec<_> = (0..5).map(|_| observation(PROXY1, UNSELECTED, approved_hash())).collect();
    let obs = available(&rules, items);

    // Exactly at budget: fine.
    assert!(try_evaluate(&rules, &input(&[], &obs), 5).is_ok());
    // One below: the alias dimension alone overflows it.
    let error = try_evaluate(&rules, &input(&[], &obs), 4).expect_err("must fail closed");
    assert!(
        matches!(error, MatchError::BindingBudgetExceeded { attempted: 5, limit: 4, .. }),
        "unexpected error: {error:?}"
    );
}

#[test]
fn the_budget_accumulates_across_rules() {
    let a = deny_unless_allowlisted("a", "proxy1", PROXY1, &[approved_hash()]);
    let b = deny_unless_allowlisted("b", "proxy2", PROXY2, &[approved_hash()]);
    let rules = compile(&[a, b]);
    let obs = available(
        &rules,
        vec![
            observation(PROXY1, UNSELECTED, approved_hash()),
            observation(PROXY1, UNSELECTED, approved_hash()),
            observation(PROXY2, UNSELECTED, approved_hash()),
            observation(PROXY2, UNSELECTED, approved_hash()),
        ],
    );
    // 2 + 2 = 4 across the two rules.
    assert!(try_evaluate(&rules, &input(&[], &obs), 4).is_ok());
    assert!(try_evaluate(&rules, &input(&[], &obs), 3).is_err());
}

#[test]
fn a_suppressed_rule_contributes_zero_to_the_budget_not_one() {
    // `a` is suppressed (no observation for PROXY1); `b` has one observation. If a suppressed
    // rule contributed 1, the total would be 2 and a budget of 1 would overflow.
    let a = deny_unless_allowlisted("a", "proxy1", PROXY1, &[approved_hash()]);
    let b = deny_unless_allowlisted("b", "proxy2", PROXY2, &[approved_hash()]);
    let rules = compile(&[a, b]);
    let obs = available(&rules, vec![observation(PROXY2, UNSELECTED, approved_hash())]);
    assert!(
        try_evaluate(&rules, &input(&[], &obs), 1).is_ok(),
        "a rule that is never evaluated must not consume budget"
    );
}

#[test]
fn an_executed_then_reverted_observation_still_participates_in_the_product() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    // Three observations, the reverted-but-executed one last and unapproved.
    let obs = available(
        &rules,
        vec![
            observation(PROXY1, UNSELECTED, approved_hash()),
            observation(PROXY1, UNSELECTED, approved_hash()),
            observation(PROXY1, UNSELECTED, unapproved_hash()),
        ],
    );
    assert_eq!(evaluate(&rules, &input(&[], &obs)).unwrap(), MatchOutcome::Deny);
}

// =============================================================================================
// Observation order and identical observations.
// =============================================================================================

#[test]
fn two_identical_observations_are_both_retained_as_distinct_execution_facts() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    let one = observation(PROXY1, UNSELECTED, approved_hash());
    let obs = available(&rules, vec![one.clone(), one]);
    // Two identical candidates → the budget sees 2, proving neither was deduplicated away.
    assert!(try_evaluate(&rules, &input(&[], &obs), 2).is_ok());
    assert!(try_evaluate(&rules, &input(&[], &obs), 1).is_err());
}

#[test]
fn observations_are_bound_in_execution_order() {
    let rules = compile(&[deny_unless_allowlisted("g", "proxy1", PROXY1, &[approved_hash()])]);
    let first = observation(PROXY1, PROXY2, approved_hash());
    let second = observation(PROXY1, UNSELECTED, approved_hash());
    let obs = available(&rules, vec![first.clone(), second.clone()]);
    match &obs {
        Observations::Available { items, .. } => {
            let for_proxy1 = obs.items_for(address_of(PROXY1));
            assert_eq!(for_proxy1.len(), 2);
            assert_eq!(for_proxy1[0], &first);
            assert_eq!(for_proxy1[1], &second);
            assert_eq!(items.len(), 2);
        }
        Observations::Unavailable(_) => panic!("expected an available set"),
    }
}
