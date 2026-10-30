//! Rule-side acceptance coverage for the `callstack.direct_delegatecalls` section: wire schema,
//! load-time rejections, condition bindings, candidate enumeration, rule suppression, the
//! combined per-transaction budget, and the difference between "observed nothing" and "could not
//! observe".
//!
//! Frame-discrimination behavior (which frames become observations at all) is covered where the
//! inspector lives, in the block builder.

use alloy_primitives::{Address, Log, B256, U256};
use serde_json::{json, Value};

use crate::callstack::{CallObservation, ObservationUnavailable, Observations, SnapshotStamp};
use crate::handle::ScreenInput;
use crate::matching::{try_evaluate, MatchError, MatchErrorKind, MatchOutcome};
use crate::rules::{compile_rule, load_rules, RawRule, RuleSet, DEFAULT_MAX_EVENT_BINDINGS_PER_TX};

/// The one supported rule protocol version. `callstack` belongs to it, so there is no version
/// gate left to exercise — only the fact that this is the single accepted value.
const PV: u32 = crate::config::MIN_PROTOCOL_VERSION;

fn proxy() -> Address {
    "0x1111111111111111111111111111111111111111".parse().unwrap()
}

fn other_proxy() -> Address {
    "0x2222222222222222222222222222222222222222".parse().unwrap()
}

fn implementation() -> Address {
    "0x3333333333333333333333333333333333333333".parse().unwrap()
}

fn library() -> Address {
    "0x4444444444444444444444444444444444444444".parse().unwrap()
}

fn proxy_code() -> B256 {
    B256::repeat_byte(0xa1)
}

fn approved_impl_code() -> B256 {
    B256::repeat_byte(0xb1)
}

fn rogue_code() -> B256 {
    B256::repeat_byte(0xee)
}

fn hex_addr(a: Address) -> String {
    format!("{a:#x}")
}

fn hex_hash(h: B256) -> String {
    format!("{h:#x}")
}

fn raw(value: Value) -> RawRule {
    serde_json::from_value(value).expect("rule json deserializes")
}

/// A `deny` rule declaring one alias on `address`, with the given condition.
fn deny_on_alias(id: &str, alias: &str, address: Address, condition: Value) -> Value {
    json!({
        "id": id,
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { alias: hex_addr(address) } },
        "condition": condition,
        "action": "deny"
    })
}

/// The producer's own shape: "the code this proxy delegated into is not on the allowlist".
///
/// There is no `matched` guard, and there is deliberately none to add: an alias that produced no
/// observation suppresses the rule, so this condition is never evaluated with an absent value.
fn delegate_code_hash_allowlist(alias: &str, approved: &[B256]) -> Value {
    let list: Vec<Value> = approved.iter().map(|h| Value::String(hex_hash(*h))).collect();
    json!({ "!": { "in": [ { "var": format!("{alias}.delegateCodeHash") }, list ] } })
}

/// The producer's paired shape: an approved implementation is an (address, code hash) pair, so
/// crossing address A with hash B is not approved.
fn paired_allowlist(alias: &str, approved: &[(Address, B256)]) -> Value {
    let pairs: Vec<Value> = approved
        .iter()
        .map(|(address, code_hash)| {
            json!({ "and": [
                { "==": [ { "var": format!("{alias}.delegateTo") }, hex_addr(*address) ] },
                { "==": [ { "var": format!("{alias}.delegateCodeHash") }, hex_hash(*code_hash) ] }
            ]})
        })
        .collect();
    json!({ "!": { "or": pairs } })
}

/// A `Transfer` event declaration, reused by the mixed-rule tests.
fn transfer_abi() -> Value {
    json!({
        "type": "event",
        "name": "Transfer",
        "inputs": [
            {"name": "from", "type": "address", "indexed": true},
            {"name": "to", "type": "address", "indexed": true},
            {"name": "value", "type": "uint256", "indexed": false}
        ],
        "anonymous": false
    })
}

fn rule_set(rules: Vec<Value>) -> RuleSet {
    let set = load_rules(PV, 1, rules.into_iter().map(raw).collect());
    assert!(set.rejected.is_empty(), "test rule set must load cleanly: {:?}", set.rejected);
    set
}

fn screen<'a>(
    rules: &RuleSet,
    logs: &'a [Log],
    observations: Observations<'a>,
) -> Result<MatchOutcome, MatchError> {
    screen_with_limit(rules, logs, observations, DEFAULT_MAX_EVENT_BINDINGS_PER_TX)
}

fn screen_with_limit<'a>(
    rules: &RuleSet,
    logs: &'a [Log],
    observations: Observations<'a>,
    limit: usize,
) -> Result<MatchOutcome, MatchError> {
    let input = ScreenInput {
        tx_hash: B256::repeat_byte(0x77),
        origin: "0x9999999999999999999999999999999999999999".parse().unwrap(),
        tx_to: Some(proxy()),
        nonce: 1,
        value: U256::ZERO,
        block_height: 100,
        logs,
        observations,
    };
    try_evaluate(rules, &input, limit)
}

/// Observations stamped with `rules`'s own identity, i.e. captured under that snapshot.
fn captured<'a>(rules: &RuleSet, items: &'a [CallObservation]) -> Observations<'a> {
    Observations::available(rules.stamp(), items)
}

/// One direct delegatecall of `proxy()` into `impl_address`, which executed `impl_code`.
fn delegated(impl_address: Address, impl_code: B256) -> CallObservation {
    CallObservation::direct_delegatecall(proxy(), proxy_code(), impl_address, impl_code)
}

// --------------------------------------------------------------------------------------------
// Functional behavior
// --------------------------------------------------------------------------------------------

#[test]
fn a_delegatecall_into_an_approved_implementation_is_allowed() {
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "vaultProxy",
        proxy(),
        delegate_code_hash_allowlist("vaultProxy", &[approved_impl_code()]),
    )]);
    let items = [delegated(implementation(), approved_impl_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Allow));
}

#[test]
fn a_delegatecall_into_an_unapproved_implementation_is_denied() {
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "vaultProxy",
        proxy(),
        delegate_code_hash_allowlist("vaultProxy", &[approved_impl_code()]),
    )]);
    let items = [delegated(implementation(), rogue_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));
}

#[test]
fn the_two_bound_variables_are_the_executed_target_and_its_code_hash() {
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        json!({ "and": [
            { "==": [ { "var": "p.delegateTo" }, hex_addr(implementation()) ] },
            { "==": [ { "var": "p.delegateCodeHash" }, hex_hash(approved_impl_code()) ] }
        ]}),
    )]);
    let items = [delegated(implementation(), approved_impl_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));
}

#[test]
fn the_paired_allowlist_rejects_the_crossed_combination() {
    // The shape that inverts into a universal objection if `delegateTo` is not bound: both halves
    // of the pair must come from the same approved implementation.
    let rules = rule_set(vec![deny_on_alias(
        "bridgeProxy",
        "bridgeProxy",
        proxy(),
        paired_allowlist(
            "bridgeProxy",
            &[(implementation(), approved_impl_code()), (library(), rogue_code())],
        ),
    )]);

    // Exactly an approved pair → no objection.
    let approved = [delegated(implementation(), approved_impl_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &approved)), Ok(MatchOutcome::Allow));

    // Approved address crossed with the other pair's hash → objection.
    let crossed = [delegated(implementation(), rogue_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &crossed)), Ok(MatchOutcome::Deny));
}

#[test]
fn each_observation_is_an_independent_alternative() {
    // Several delegatecalls for one alias are alternatives: any one of them satisfying a deny
    // condition denies the transaction, even when an earlier one is approved.
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    )]);
    let approved_only = [
        delegated(implementation(), approved_impl_code()),
        delegated(library(), approved_impl_code()),
    ];
    assert_eq!(screen(&rules, &[], captured(&rules, &approved_only)), Ok(MatchOutcome::Allow));

    let one_rogue =
        [delegated(implementation(), approved_impl_code()), delegated(library(), rogue_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &one_rogue)), Ok(MatchOutcome::Deny));
}

#[test]
fn a_plain_call_observation_is_never_a_candidate() {
    // Capture still produces `Call` observations — they are what lets it attribute a delegatecall
    // at all — but no condition variable under this contract can read one, so they must not
    // become candidates. A set holding ONLY a call observation leaves the alias unobserved, which
    // suppresses the rule rather than binding anything.
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    )]);
    let only_a_call = [CallObservation::call(proxy(), rogue_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &only_a_call)), Ok(MatchOutcome::Allow));

    // With both kinds present, the verdict comes from the delegatecall alone.
    let both = [CallObservation::call(proxy(), proxy_code()), delegated(library(), rogue_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &both)), Ok(MatchOutcome::Deny));
}

#[test]
fn an_observation_of_another_address_is_not_a_candidate() {
    // The nested-delegatecall and wrong-proxy cases at the rule layer: an observation whose
    // execution identity is a different address never satisfies this alias.
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    )]);
    let elsewhere = [CallObservation::direct_delegatecall(
        other_proxy(),
        proxy_code(),
        library(),
        rogue_code(),
    )];
    assert_eq!(screen(&rules, &[], captured(&rules, &elsewhere)), Ok(MatchOutcome::Allow));
}

#[test]
fn two_aliases_keep_independent_bindings() {
    let rules = rule_set(vec![json!({
        "id": "two-aliases",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": {
            "a": hex_addr(proxy()),
            "b": hex_addr(other_proxy())
        }},
        // Denies only when `a` delegated into unapproved code; `b`'s own allowlist is separate.
        "condition": { "and": [
            { "!": { "in": [ { "var": "a.delegateCodeHash" }, [hex_hash(approved_impl_code())] ] } },
            { "in": [ { "var": "b.delegateCodeHash" }, [hex_hash(approved_impl_code())] ] }
        ]},
        "action": "deny"
    })]);

    let both_fine = [
        delegated(implementation(), approved_impl_code()),
        CallObservation::direct_delegatecall(
            other_proxy(),
            proxy_code(),
            implementation(),
            approved_impl_code(),
        ),
    ];
    assert_eq!(screen(&rules, &[], captured(&rules, &both_fine)), Ok(MatchOutcome::Allow));

    let a_rogue = [
        delegated(implementation(), rogue_code()),
        CallObservation::direct_delegatecall(
            other_proxy(),
            proxy_code(),
            implementation(),
            approved_impl_code(),
        ),
    ];
    assert_eq!(screen(&rules, &[], captured(&rules, &a_rogue)), Ok(MatchOutcome::Deny));
}

#[test]
fn an_unobserved_alias_suppresses_the_whole_rule() {
    // The hazard this contract removes at its source. The producer's condition is a NEGATED
    // membership test, and the evaluator treats a one-sided null as "not equal", so binding an
    // absent value as null would make this rule object to every transaction that never touched
    // the proxy. Suppression means the condition is never evaluated at all.
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    )]);

    // Capture ran and genuinely observed nothing.
    assert_eq!(screen(&rules, &[], captured(&rules, &[])), Ok(MatchOutcome::Allow));
    // Capture ran and observed an unrelated address.
    let elsewhere = [CallObservation::direct_delegatecall(
        other_proxy(),
        proxy_code(),
        library(),
        rogue_code(),
    )];
    assert_eq!(screen(&rules, &[], captured(&rules, &elsewhere)), Ok(MatchOutcome::Allow));
}

#[test]
fn a_mixed_rule_requires_both_an_event_binding_and_an_observation() {
    use crate::test_support::log_builder::erc20_transfer;
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    let rules = rule_set(vec![json!({
        "id": "combined",
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": { "and": [
            { "==": [ { "var": "t.from" }, hex_addr(from) ] },
            { "!": { "in": [ { "var": "p.delegateCodeHash" }, [hex_hash(approved_impl_code())] ] } }
        ]},
        "action": "deny"
    })]);

    let logs = vec![erc20_transfer(token, from, to, U256::from(5u64))];
    let rogue = [delegated(library(), rogue_code())];
    let approved = [delegated(implementation(), approved_impl_code())];

    // Both present and both satisfied.
    assert_eq!(screen(&rules, &logs, captured(&rules, &rogue)), Ok(MatchOutcome::Deny));
    // The event alone is not enough.
    assert_eq!(screen(&rules, &logs, captured(&rules, &approved)), Ok(MatchOutcome::Allow));
    // The frame alone is not enough: with no physical event binding the rule is suppressed, so
    // the callstack dimension cannot fire a mixed rule on its own.
    assert_eq!(screen(&rules, &[], captured(&rules, &rogue)), Ok(MatchOutcome::Allow));
    // And neither is present.
    assert_eq!(screen(&rules, &[], captured(&rules, &[])), Ok(MatchOutcome::Allow));
}

/// A `TransferSingle` declaration, used where a second, distinct declared event is needed.
fn transfer_single_abi() -> Value {
    json!({
        "type": "event",
        "name": "TransferSingle",
        "inputs": [
            {"name": "operator", "type": "address", "indexed": true},
            {"name": "from", "type": "address", "indexed": true},
            {"name": "to", "type": "address", "indexed": true},
            {"name": "id", "type": "uint256", "indexed": false},
            {"name": "value", "type": "uint256", "indexed": false}
        ],
        "anonymous": false
    })
}

/// A mixed rule: one declared event plus one declared alias on `proxy()`, carrying `condition`.
fn mixed_rule(id: &str, condition: Value) -> Value {
    json!({
        "id": id,
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": condition,
        "action": "deny"
    })
}

#[test]
fn a_mixed_rule_with_a_missing_event_is_suppressed_even_when_its_condition_holds() {
    use crate::test_support::log_builder::erc20_transfer;
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    // `a_mixed_rule_requires_both_an_event_binding_and_an_observation` above cannot distinguish
    // suppression from a condition that merely did not hold: its `t.from` comparison is false
    // against a null binding whatever the rule class does. Here the condition is satisfied by the
    // callstack side alone, so an evaluated rule denies and only suppression allows.
    let conditions = [
        // The degenerate case: nothing but the rule's declared shape decides the outcome.
        json!(true),
        // The producer's own shape, against an implementation that is not on the allowlist.
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    ];
    for condition in conditions {
        let rules = rule_set(vec![mixed_rule("combined", condition.clone())]);
        let rogue = [delegated(library(), rogue_code())];

        assert_eq!(
            screen(&rules, &[], captured(&rules, &rogue)),
            Ok(MatchOutcome::Allow),
            "a mixed rule whose declared event produced no physical binding must not be \
             evaluated, even though its alias was observed and its condition holds \
             (condition = {condition})"
        );

        // Positive control on the same rule set: with the event physically present the deny
        // takes effect normally, so suppression is not "this rule can never fire".
        let logs = vec![erc20_transfer(token, from, to, U256::from(5u64))];
        assert_eq!(
            screen(&rules, &logs, captured(&rules, &rogue)),
            Ok(MatchOutcome::Deny),
            "with the declared event present the same rule still denies (condition = {condition})"
        );
    }
}

#[test]
fn a_mixed_rule_missing_exactly_one_of_several_events_is_suppressed() {
    use crate::test_support::log_builder::{erc1155_transfer_single, erc20_transfer};
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();
    let operator: Address = "0x8888888888888888888888888888888888888888".parse().unwrap();

    // Two declared events, one declared alias. One missing event is enough to suppress: the whole
    // rule requires both binding families complete, not "at least one event".
    let rules = rule_set(vec![json!({
        "id": "two-events",
        "event_abis": { "t": transfer_abi(), "s": transfer_single_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": true,
        "action": "deny"
    })]);
    let rogue = [delegated(library(), rogue_code())];

    let transfer = erc20_transfer(token, from, to, U256::from(5u64));
    let single =
        erc1155_transfer_single(token, operator, from, to, U256::from(1u64), U256::from(2u64));

    // Only the first declared event fired.
    assert_eq!(
        screen(&rules, std::slice::from_ref(&transfer), captured(&rules, &rogue)),
        Ok(MatchOutcome::Allow),
        "one of two declared events missing suppresses the rule"
    );
    // Only the second declared event fired.
    assert_eq!(
        screen(&rules, std::slice::from_ref(&single), captured(&rules, &rogue)),
        Ok(MatchOutcome::Allow),
        "the other one missing suppresses it just the same"
    );
    // Both fired: the positive control.
    let both = vec![transfer, single];
    assert_eq!(
        screen(&rules, &both, captured(&rules, &rogue)),
        Ok(MatchOutcome::Deny),
        "with every declared event present the rule denies"
    );
}

#[test]
fn a_suppressed_mixed_rule_is_charged_nothing_even_at_a_zero_budget() {
    // Suppression happens before the budget is charged, so the rule costs zero rather than the
    // one candidate a null-bound missing event would have cost. A budget of zero is the sharpest
    // possible assertion of that: any charge at all would exceed it.
    let rules = rule_set(vec![mixed_rule("combined", json!(true))]);
    let rogue = [delegated(library(), rogue_code())];
    assert_eq!(
        screen_with_limit(&rules, &[], captured(&rules, &rogue), 0),
        Ok(MatchOutcome::Allow),
        "a suppressed mixed rule must not raise BindingBudgetExceeded"
    );
}

#[test]
fn a_mixed_rule_with_a_missing_event_still_fails_closed_on_an_unadjudicable_set() {
    // Suppression must not swallow the fail-closed cases: "capture did not run" and "capture ran
    // under a different snapshot" are decided before the missing-event check, so a mixed rule
    // with no log still reports them rather than being quietly allowed.
    let rules = rule_set(vec![mixed_rule("combined", json!(true))]);

    for reason in [ObservationUnavailable::NotCaptured, ObservationUnavailable::BudgetExceeded] {
        assert_eq!(
            screen(&rules, &[], Observations::Unavailable { reason }),
            Err(MatchError::ObservationsUnavailable { rule_id: "combined".to_string(), reason }),
            "{reason:?} must still fail closed for a mixed rule with no log"
        );
    }

    let own = rules.stamp();
    let foreign = SnapshotStamp { content_version: own.content_version + 1, ..own };
    let rogue = [delegated(library(), rogue_code())];
    assert_eq!(
        screen(&rules, &[], Observations::available(foreign, &rogue)),
        Err(MatchError::ObservationSnapshotMismatch { rule_id: "combined".to_string() }),
        "a foreign-stamped set must still fail closed for a mixed rule with no log"
    );
}

#[test]
fn a_mixed_rule_evaluates_every_event_times_alias_combination() {
    use crate::test_support::log_builder::erc20_transfer;
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let other: Address = "0x8888888888888888888888888888888888888888".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    let rules = rule_set(vec![json!({
        "id": "combined",
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": { "and": [
            { "==": [ { "var": "t.from" }, hex_addr(from) ] },
            { "==": [ { "var": "p.delegateCodeHash" }, hex_hash(rogue_code()) ] }
        ]},
        "action": "deny"
    })]);

    // The satisfying pair is the SECOND event with the SECOND observation, so only a full product
    // walk finds it.
    let logs = vec![
        erc20_transfer(token, other, to, U256::from(1u64)),
        erc20_transfer(token, from, to, U256::from(2u64)),
    ];
    let items =
        [delegated(implementation(), approved_impl_code()), delegated(library(), rogue_code())];
    assert_eq!(screen(&rules, &logs, captured(&rules, &items)), Ok(MatchOutcome::Deny));
}

#[test]
fn an_upgraded_and_rolled_back_implementation_is_still_caught() {
    // The motivating case: within one transaction the proxy is pointed at unapproved code, that
    // code executes, and the pointer is restored. Only the execution-time observation records
    // what ran, so the rule still fires — nothing here is derived from the final pointer.
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    )]);
    let items = [
        // pointer set to the rogue implementation, then executed
        delegated(library(), rogue_code()),
        // pointer restored and the approved implementation executed afterwards
        delegated(implementation(), approved_impl_code()),
    ];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));
}

// --------------------------------------------------------------------------------------------
// Load-time validation
// --------------------------------------------------------------------------------------------

fn reject_reason(value: Value) -> String {
    compile_rule(raw(value)).expect_err("rule must be rejected").to_string()
}

#[test]
fn an_unknown_key_inside_callstack_is_rejected_at_deserialization() {
    // The retired selector form, a near-miss spelling and a speculative sibling all land here.
    for section in [
        json!({ "vaultProxy": { "address": hex_addr(proxy()) } }),
        json!({ "direct_delegatecall": { "p": hex_addr(proxy()) } }),
        json!({ "directDelegatecalls": { "p": hex_addr(proxy()) } }),
        json!({ "Direct_DelegateCalls": { "p": hex_addr(proxy()) } }),
        json!({ "direct_delegatecalls": { "p": hex_addr(proxy()) }, "extra": {} }),
    ] {
        let rule = json!({
            "id": "r",
            "event_abis": {},
            "callstack": section,
            "condition": true,
            "action": "deny"
        });
        let error = serde_json::from_value::<RawRule>(rule)
            .expect_err("an unknown key inside callstack must be rejected");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }
}

#[test]
fn a_misspelled_callstack_sibling_is_rejected_by_the_rule_root_gate() {
    // The rule root keeps its own unknown-field gate, so a misspelled sibling is a named
    // rejection rather than a rule that quietly degrades into an event-only one.
    let reason = reject_reason(json!({
        "id": "r",
        "event_abis": { "t": transfer_abi() },
        "callStack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": true,
        "action": "deny"
    }));
    assert!(reason.contains("unknown field(s) in rule"), "{reason}");
    assert!(reason.contains("callStack"), "{reason}");
}

#[test]
fn an_explicitly_null_callstack_is_rejected() {
    let reason = reject_reason(json!({
        "id": "r",
        "event_abis": { "t": transfer_abi() },
        "callstack": Value::Null,
        "condition": true,
        "action": "deny"
    }));
    assert!(reason.contains("explicitly null"), "{reason}");

    // And it is NOT reported as an absent section, even with no events declared.
    let reason = reject_reason(json!({
        "id": "r",
        "event_abis": {},
        "callstack": Value::Null,
        "condition": true,
        "action": "deny"
    }));
    assert!(reason.contains("explicitly null"), "{reason}");
}

#[test]
fn an_empty_direct_delegatecalls_map_is_rejected() {
    let reason = reject_reason(json!({
        "id": "r",
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": {} },
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(reason, "direct_delegatecalls is empty");
}

#[test]
fn a_wrong_json_type_is_rejected_naming_the_schema_element() {
    // Each case names the element it failed on: the `callstack` section's own type, the alias map,
    // or the individual alias whose value is not an address string.
    let cases: [(Value, &str); 5] = [
        (json!([]), "RawCallstack"),
        (json!(7), "RawCallstack"),
        (json!({ "direct_delegatecalls": [] }), "direct_delegatecalls"),
        (json!({ "direct_delegatecalls": "x" }), "direct_delegatecalls"),
        (json!({ "direct_delegatecalls": { "p": 7 } }), "direct_delegatecalls alias 'p'"),
    ];
    for (section, expected) in cases {
        let rule = json!({
            "id": "r",
            "event_abis": {},
            "callstack": section,
            "condition": true,
            "action": "deny"
        });
        let error = serde_json::from_value::<RawRule>(rule)
            .expect_err("a wrong JSON type must be rejected");
        let text = error.to_string();
        assert!(text.contains(expected), "expected {expected:?} in {text:?}");
    }

    // A `callstack` object that declares nothing at all is the missing-field case, which also
    // names the one admissible field.
    let error = serde_json::from_value::<RawRule>(json!({
        "id": "r",
        "event_abis": {},
        "callstack": {},
        "condition": true,
        "action": "deny"
    }))
    .expect_err("an empty callstack object is missing its only field");
    assert!(error.to_string().contains("missing field `direct_delegatecalls`"), "{error}");
}

#[test]
fn a_non_lowercase_or_malformed_alias_address_is_rejected() {
    for bad in [
        "1111111111111111111111111111111111111111",
        "0x1111",
        "0x111111111111111111111111111111111111111g",
        // A checksummed address is a rejection, not a normalisation: observed values are emitted
        // lowercase, so accepting it would compare against a different string form.
        "0x1111111111111111111111111111111111111A11",
    ] {
        let reason = reject_reason(json!({
            "id": "r",
            "event_abis": {},
            "callstack": { "direct_delegatecalls": { "p": bad } },
            "condition": true,
            "action": "deny"
        }));
        assert_eq!(
            reason, "direct_delegatecalls alias 'p' is not a lowercase 0x 20-byte address",
            "input {bad}"
        );
    }
}

#[test]
fn more_than_eight_aliases_in_one_rule_is_rejected() {
    let mut aliases = serde_json::Map::new();
    for i in 0..=crate::rules::MAX_DIRECT_DELEGATECALL_ALIASES {
        aliases.insert(format!("p{i}"), Value::String(hex_addr(Address::repeat_byte(i as u8 + 1))));
    }
    let reason = reject_reason(json!({
        "id": "r",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": Value::Object(aliases) },
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(reason, "direct_delegatecalls declares 9 aliases; at most 8 are allowed");

    // Exactly eight stays valid.
    let mut aliases = serde_json::Map::new();
    for i in 0..crate::rules::MAX_DIRECT_DELEGATECALL_ALIASES {
        aliases.insert(format!("p{i}"), Value::String(hex_addr(Address::repeat_byte(i as u8 + 1))));
    }
    assert!(
        compile_rule(raw(json!({
            "id": "r",
            "event_abis": {},
            "callstack": { "direct_delegatecalls": Value::Object(aliases) },
            "condition": true,
            "action": "deny"
        })))
        .is_ok(),
        "eight aliases must remain valid"
    );
}

#[test]
fn an_alias_colliding_with_an_event_alias_is_rejected() {
    // Condition variables share one flat namespace, so a collision would silently shadow one of
    // the two families instead of being visible to the author.
    let reason = reject_reason(json!({
        "id": "r",
        "event_abis": { "p": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": true,
        "action": "deny"
    }));
    assert!(reason.contains("collides with an event_abis alias"), "{reason}");
    assert!(reason.contains("direct_delegatecalls alias 'p'"), "{reason}");
}

#[test]
fn a_callstack_only_audit_rule_is_rejected_and_a_mixed_one_is_accepted() {
    let reason = reject_reason(json!({
        "id": "r",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": true,
        "action": "audit",
        "audit_types": ["delegatecall_review"]
    }));
    assert!(reason.contains("no event payload content"), "{reason}");

    // With a declared event the audit payload has content, so the rule is valid — including with
    // an audit type that is not the quota one, which carries no fixed event shape constraint.
    assert!(compile_rule(raw(json!({
        "id": "r",
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": true,
        "action": "audit",
        "audit_types": ["delegatecall_review"]
    })))
    .is_ok());
}

#[test]
fn an_empty_alias_name_is_rejected_but_a_non_ascii_one_is_accepted() {
    let reason = reject_reason(json!({
        "id": "r",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "": hex_addr(proxy()) } },
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(reason, "direct_delegatecalls declares an empty alias name");

    // A non-empty non-ASCII alias is a legitimate rule-local identifier: it loads, binds and
    // evaluates normally, and must never be reported with the empty-alias reason.
    let rules = rule_set(vec![deny_on_alias(
        "non-ascii",
        "代理1",
        proxy(),
        delegate_code_hash_allowlist("代理1", &[approved_impl_code()]),
    )]);
    let rogue = [delegated(library(), rogue_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &rogue)), Ok(MatchOutcome::Deny));
    let approved = [delegated(implementation(), approved_impl_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &approved)), Ok(MatchOutcome::Allow));
}

#[test]
fn a_duplicate_alias_is_rejected_at_deserialization_in_both_orders() {
    // A plain map would keep the last occurrence and silently drop the first, so the duplicate is
    // surfaced instead of collapsing an author-visible alias. Same value, different values and
    // swapped order must all behave identically.
    let a = "0x1111111111111111111111111111111111111111";
    let b = "0x2222222222222222222222222222222222222222";
    for (first, second) in [(a, a), (a, b), (b, a)] {
        let json = format!(
            r#"{{
                "id": "r",
                "event_abis": {{}},
                "callstack": {{ "direct_delegatecalls": {{ "p": "{first}", "p": "{second}" }} }},
                "condition": true,
                "action": "deny"
            }}"#
        );
        let error =
            serde_json::from_str::<RawRule>(&json).expect_err("a duplicate alias must be rejected");
        assert_eq!(
            error.to_string().split(" at line").next().unwrap(),
            "direct_delegatecalls declares alias 'p' more than once",
            "{first} then {second}"
        );
    }

    // The legal control is accepted, which proves the payload reached this decode path rather
    // than failing for an unrelated schema reason.
    let control = format!(
        r#"{{
            "id": "r",
            "event_abis": {{}},
            "callstack": {{ "direct_delegatecalls": {{ "p": "{a}" }} }},
            "condition": true,
            "action": "deny"
        }}"#
    );
    let parsed = serde_json::from_str::<RawRule>(&control).expect("the legal control is accepted");
    assert!(compile_rule(parsed).is_ok());
}

#[test]
fn a_duplicate_modelled_rule_root_key_is_rejected_in_both_orders() {
    // Struct-field deserialization carries its own duplicate guard, and the rule path reaches
    // `RawRule` straight from the bytes with no intermediate map, so the duplicate evidence is
    // still intact when that guard runs. This is an existing property to preserve, not a new one.
    let proxy_hex = hex_addr(proxy());
    let other_hex = hex_addr(other_proxy());
    let cases = [
        r#""action": "deny", "action": "deny""#.to_string(),
        r#""action": "deny", "action": "allow""#.to_string(),
        r#""action": "allow", "action": "deny""#.to_string(),
        format!(
            r#""callstack": {{ "direct_delegatecalls": {{ "p": "{proxy_hex}" }} }}, "callstack": {{ "direct_delegatecalls": {{ "p": "{other_hex}" }} }}, "action": "deny""#
        ),
        format!(
            r#""callstack": {{ "direct_delegatecalls": {{ "p": "{other_hex}" }} }}, "callstack": {{ "direct_delegatecalls": {{ "p": "{proxy_hex}" }} }}, "action": "deny""#
        ),
    ];
    for tail in cases {
        let json = format!(
            r#"{{ "id": "r", "event_abis": {{}}, "callstack": {{ "direct_delegatecalls": {{ "p": "{proxy_hex}" }} }}, "condition": true, {tail} }}"#
        );
        let error = serde_json::from_str::<RawRule>(&json)
            .expect_err("a duplicate modelled key must be rejected");
        assert!(error.to_string().contains("duplicate field"), "{error}");
    }
}

#[test]
fn a_rule_with_neither_selector_family_is_rejected() {
    let reason = reject_reason(json!({ "id": "r", "condition": true, "action": "deny" }));
    assert_eq!(reason, "rule declares neither event_abis nor callstack");

    let reason =
        reject_reason(json!({ "id": "r", "event_abis": {}, "condition": true, "action": "deny" }));
    assert_eq!(reason, "rule declares neither event_abis nor callstack");
}

#[test]
fn a_callstack_only_rule_may_omit_event_abis() {
    let compiled = compile_rule(raw(deny_on_alias("callstack-only", "p", proxy(), json!(true))))
        .expect("a rule selecting purely on call frames is valid");
    assert!(compiled.events.is_empty());
    assert_eq!(compiled.callstack.len(), 1);
    assert_eq!(compiled.callstack[0].alias, "p");
    assert_eq!(compiled.callstack[0].address, proxy());
}

#[test]
fn a_rejected_rule_refuses_the_whole_snapshot() {
    // Per-rule validation stays per-rule, and the diagnostic set keeps the valid sibling, so an
    // operator can see which rule failed and why. The install gate is what refuses the batch.
    let set = load_rules(
        PV,
        1,
        vec![
            raw(deny_on_alias("good", "p", proxy(), json!(true))),
            raw(json!({
                "id": "bad",
                "event_abis": {},
                "callstack": { "direct_delegatecalls": { "p": "0xNOPE" } },
                "condition": true,
                "action": "deny"
            })),
        ],
    );
    assert_eq!(set.rules.len(), 1, "the valid sibling is visible in the diagnostic set");
    assert_eq!(set.rules[0].id, "good");
    assert_eq!(set.rejected.len(), 1);
    assert_eq!(set.rejected[0].id, "bad");
}

// --------------------------------------------------------------------------------------------
// Condition-variable validation
// --------------------------------------------------------------------------------------------

#[test]
fn a_condition_reading_an_unprovided_alias_variable_is_rejected() {
    // Every one of these resolves to `null` at evaluation time, and inside the producer's own
    // negated `or` that inverts the rule into an unconditional objection. A load-time rejection
    // naming the variable is the only safe outcome.
    for field in [
        "delegateAddress",
        "contextAddress",
        "contextCodeHash",
        "matched",
        "codeHash",
        "callType",
        "implAddress",
        "implCodeHash",
        "delegateto",
        "delegateCodehash",
    ] {
        let reason = reject_reason(deny_on_alias(
            "r",
            "p",
            proxy(),
            json!({ "==": [ { "var": format!("p.{field}") }, hex_addr(implementation()) ] }),
        ));
        assert_eq!(
            reason,
            format!(
                "condition references unknown variable 'p.{field}'; direct_delegatecalls provides delegateTo and delegateCodeHash"
            )
        );
    }
}

#[test]
fn the_guard_fires_on_the_producer_shaped_negated_or() {
    // The exact shape of the paired producer rule, with one half misspelled. Rejecting it at load
    // is what prevents the inversion; the counterexample below records what the inversion is.
    let reason = reject_reason(deny_on_alias(
        "r",
        "bridgeProxy",
        proxy(),
        json!({ "!": { "or": [
            { "and": [
                { "==": [ { "var": "bridgeProxy.delegateAddress" }, hex_addr(implementation()) ] },
                { "==": [ { "var": "bridgeProxy.delegateCodeHash" }, hex_hash(approved_impl_code()) ] }
            ]}
        ]}}),
    ));
    assert!(reason.contains("'bridgeProxy.delegateAddress'"), "{reason}");
}

#[test]
fn the_unprovided_variable_would_otherwise_invert_into_a_universal_objection() {
    // The recorded counterexample for the rejection above, driven through the evaluator directly
    // because the rule can no longer be compiled. With the misspelled variable absent, both `and`
    // branches are false, the `or` is false, and the `!` is TRUE — so the rule would object to
    // every observed delegatecall, including an explicitly approved implementation.
    let condition = json!({ "!": { "or": [
        { "and": [
            { "==": [ { "var": "bridgeProxy.delegateAddress" }, hex_addr(implementation()) ] },
            { "==": [ { "var": "bridgeProxy.delegateCodeHash" }, hex_hash(approved_impl_code()) ] }
        ]}
    ]}});
    let mut bindings = crate::rules::Bindings::new();
    bindings.insert("bridgeProxy.delegateTo".to_string(), json!(hex_addr(implementation())));
    bindings
        .insert("bridgeProxy.delegateCodeHash".to_string(), json!(hex_hash(approved_impl_code())));
    assert!(
        crate::rules::truthy(&condition, &bindings),
        "an unbound variable inverts the negated allowlist, which is why it is a load-time error"
    );

    // The correctly spelled rule does not fire for that same approved implementation.
    let correct = json!({ "!": { "or": [
        { "and": [
            { "==": [ { "var": "bridgeProxy.delegateTo" }, hex_addr(implementation()) ] },
            { "==": [ { "var": "bridgeProxy.delegateCodeHash" }, hex_hash(approved_impl_code()) ] }
        ]}
    ]}});
    assert!(!crate::rules::truthy(&correct, &bindings));
}

#[test]
fn the_guard_leaves_event_and_transaction_variables_alone() {
    // Scope is a declared alias prefix of the same rule, and nothing else.
    assert!(
        compile_rule(raw(deny_on_alias(
            "r",
            "p",
            proxy(),
            json!({ "and": [
                { "==": [ { "var": "p.delegateTo" }, hex_addr(implementation()) ] },
                { "==": [ { "var": "p.delegateCodeHash" }, hex_hash(approved_impl_code()) ] }
            ]})
        )))
        .is_ok(),
        "the two provided variables are accepted"
    );

    // An event-only rule using an event variable plus every transaction-level variable.
    assert!(
        compile_rule(raw(json!({
            "id": "event-only",
            "event_abis": { "transfer": transfer_abi() },
            "condition": { "and": [
                { "==": [ { "var": "transfer.from" }, hex_addr(implementation()) ] },
                { "==": [ { "var": "origin" }, hex_addr(implementation()) ] },
                { "==": [ { "var": "contract_address" }, hex_addr(proxy()) ] },
                { "==": [ { "var": "value" }, "0" ] },
                { "==": [ { "var": "nonce" }, 0 ] }
            ]},
            "action": "deny"
        })))
        .is_ok(),
        "no event or transaction variable may be affected"
    );

    // A mixed rule: the event alias's own fields are untouched by the alias guard.
    assert!(compile_rule(raw(json!({
        "id": "mixed",
        "event_abis": { "transfer": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": { "and": [
            { "==": [ { "var": "transfer.whatever" }, null ] },
            { "==": [ { "var": "p.delegateTo" }, hex_addr(implementation()) ] }
        ]},
        "action": "deny"
    })))
    .is_ok());

    // And a variable whose prefix merely resembles the alias is not a match.
    assert!(compile_rule(raw(deny_on_alias(
        "r",
        "p",
        proxy(),
        json!({ "==": [ { "var": "px.delegateAddress" }, null ] })
    )))
    .is_ok());
}

// --------------------------------------------------------------------------------------------
// Protocol version
// --------------------------------------------------------------------------------------------

#[test]
fn exactly_one_protocol_version_is_supported() {
    assert!(crate::config::is_supported_protocol(PV));
    assert!(!crate::config::is_supported_protocol(0));
    assert!(!crate::config::is_supported_protocol(PV + 1));
}

#[test]
fn callstack_is_accepted_at_the_only_supported_version() {
    let rule = deny_on_alias("r", "p", proxy(), json!(true));
    // Both entry points agree: the section needs no version argument at all.
    assert!(compile_rule(raw(rule.clone())).is_ok());
    let set = load_rules(PV, 9, vec![raw(rule)]);
    assert!(set.rejected.is_empty(), "{:?}", set.rejected);
    assert_eq!(set.rules.len(), 1);
    assert!(set.has_callstack_rules());
}

#[test]
fn event_only_rules_are_byte_identical_under_the_narrowed_version_set() {
    let event_rule = json!({
        "id": "event-only",
        "event_abis": { "t": transfer_abi() },
        "condition": true,
        "action": "deny"
    });
    let set = load_rules(PV, 1, vec![raw(event_rule)]);
    assert!(set.rejected.is_empty(), "{:?}", set.rejected);
    assert_eq!(set.rules.len(), 1);
    assert!(set.rules[0].callstack.is_empty());
    assert!(!set.has_callstack_rules());
    assert!(set.callstack_selectors().is_empty());
}

#[test]
fn an_event_only_rule_with_a_missing_event_is_still_evaluated() {
    use crate::test_support::log_builder::erc20_transfer;
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    // Emergency Deny-All semantics: an event-only rule contributes one null candidate for a
    // missing event and the condition still runs, so a transaction with no matching log is
    // adjudicated. Suppression applies to aliases only and must not have leaked here.
    let rules = rule_set(vec![json!({
        "id": "event-only",
        "event_abis": { "t": transfer_abi() },
        "condition": { "==": [ { "var": "t.from" }, null ] },
        "action": "deny"
    })]);
    assert_eq!(screen(&rules, &[], Observations::NOT_CAPTURED), Ok(MatchOutcome::Deny));

    // With the event physically present the same rule does not fire.
    let logs = vec![erc20_transfer(token, from, to, U256::from(1u64))];
    assert_eq!(screen(&rules, &logs, Observations::NOT_CAPTURED), Ok(MatchOutcome::Allow));
}

#[test]
fn rules_without_callstack_are_unaffected_by_unavailable_observations() {
    // The compatibility guarantee: an execution path that captures no frames changes nothing for
    // a rule that does not ask about frames.
    let rules = rule_set(vec![json!({
        "id": "event-only",
        "event_abis": { "t": transfer_abi() },
        "condition": { "==": [ { "var": "t.matched_nothing" }, null ] },
        "action": "deny"
    })]);
    for observations in [
        Observations::NOT_CAPTURED,
        Observations::Unavailable { reason: ObservationUnavailable::BudgetExceeded },
        Observations::Unavailable { reason: ObservationUnavailable::Incomplete },
        Observations::Unavailable { reason: ObservationUnavailable::SnapshotMismatch },
    ] {
        assert_eq!(
            screen(&rules, &[], observations),
            Ok(MatchOutcome::Deny),
            "an event-only rule must evaluate identically regardless of observation availability"
        );
    }
}

// --------------------------------------------------------------------------------------------
// Unavailable vs. suppressed, and snapshot identity
// --------------------------------------------------------------------------------------------

#[test]
fn a_callstack_rule_under_unavailable_observations_never_allows() {
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    )]);
    for reason in [
        ObservationUnavailable::NotCaptured,
        ObservationUnavailable::Incomplete,
        ObservationUnavailable::BudgetExceeded,
        ObservationUnavailable::SnapshotMismatch,
    ] {
        let result = screen(&rules, &[], Observations::Unavailable { reason });
        assert_eq!(
            result,
            Err(MatchError::ObservationsUnavailable { rule_id: "guard".to_string(), reason }),
            "{reason:?} must not produce a verdict"
        );
        assert_ne!(result, Ok(MatchOutcome::Allow));
        assert_eq!(
            result.unwrap_err().kind(),
            MatchErrorKind::ObservationsUnavailable,
            "the cause must stay distinguishable from a budget overflow"
        );
    }
}

#[test]
fn unavailable_is_not_the_same_as_suppressed() {
    // The distinction that matters most on this path. "Capture ran and this proxy was untouched"
    // suppresses the rule and produces no verdict; "capture could not run" must fail closed.
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    )]);
    assert_eq!(screen(&rules, &[], captured(&rules, &[])), Ok(MatchOutcome::Allow));
    assert!(screen(&rules, &[], Observations::NOT_CAPTURED).is_err());
}

#[test]
fn a_snapshot_stamp_mismatch_fails_closed_and_never_reads_as_suppression() {
    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    )]);
    let own = rules.stamp();

    // Same alias addresses, different rule content. The addresses alone cannot prove the
    // conditions are unchanged, so full rule identity — not just the digest — must be equal.
    let same_addresses_newer_content =
        SnapshotStamp { content_version: own.content_version + 1, ..own };
    assert_eq!(same_addresses_newer_content.selector_digest, own.selector_digest);
    let observations = Observations::available(same_addresses_newer_content, &[]);
    let result = screen(&rules, &[], observations);
    assert_eq!(
        result,
        Err(MatchError::ObservationSnapshotMismatch { rule_id: "guard".to_string() }),
        "a set captured under other rules must not be adjudicated"
    );
    assert_ne!(result, Ok(MatchOutcome::Allow), "and must never read as a clean no-match");
    assert_eq!(result.unwrap_err().kind(), MatchErrorKind::ObservationSnapshotMismatch);

    // A different protocol version and a different alias set are likewise mismatches.
    for stamp in [
        SnapshotStamp { protocol_version: own.protocol_version + 1, ..own },
        SnapshotStamp { selector_digest: B256::repeat_byte(0x01), ..own },
    ] {
        assert!(screen(&rules, &[], Observations::available(stamp, &[])).is_err());
    }

    // The snapshot's own stamp is accepted.
    assert_eq!(screen(&rules, &[], Observations::available(own, &[])), Ok(MatchOutcome::Allow));
}

#[test]
fn the_alias_address_set_is_the_sorted_union_over_active_rules() {
    let rules = rule_set(vec![
        deny_on_alias("r1", "a", other_proxy(), json!(true)),
        deny_on_alias("r2", "b", proxy(), json!(true)),
        // A repeat of an address already named by another rule contributes once.
        deny_on_alias("r3", "c", proxy(), json!(true)),
    ]);
    assert_eq!(rules.callstack_selectors(), [proxy(), other_proxy()]);
    assert!(rules.has_callstack_rules());

    // The digest depends on set membership only, never on declaration order.
    let reordered = rule_set(vec![
        deny_on_alias("r3", "c", proxy(), json!(true)),
        deny_on_alias("r2", "b", proxy(), json!(true)),
        deny_on_alias("r1", "a", other_proxy(), json!(true)),
    ]);
    assert_eq!(reordered.stamp().selector_digest, rules.stamp().selector_digest);
}

/// The alias-bearing RULE count is not the alias ADDRESS count: the two diverge in both
/// directions, and an event-only rule contributes to neither.
#[test]
fn the_alias_bearing_rule_count_counts_rules_not_addresses() {
    // Two rules, different ids, naming ONE address.
    let two_rules_one_address = rule_set(vec![
        deny_on_alias("r1", "a", proxy(), json!(true)),
        deny_on_alias("r2", "b", proxy(), json!(true)),
    ]);
    assert_eq!(two_rules_one_address.callstack_selectors().len(), 1, "one deduplicated address");
    assert_eq!(two_rules_one_address.selector_bearing_rule_count(), 2, "two rules carry it");

    // ONE rule naming SEVERAL addresses.
    let one_rule_two_addresses = rule_set(vec![json!({
        "id": "r1",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": {
            "a": hex_addr(proxy()),
            "b": hex_addr(other_proxy())
        }},
        "condition": true,
        "action": "deny"
    })]);
    assert_eq!(one_rule_two_addresses.callstack_selectors().len(), 2, "two addresses");
    assert_eq!(one_rule_two_addresses.selector_bearing_rule_count(), 1, "declared by one rule");

    // An event-only rule carries no alias and is counted by neither.
    let mixed = rule_set(vec![
        deny_on_alias("r1", "a", proxy(), json!(true)),
        event_and_alias_rule("r2"),
        json!({
            "id": "r3",
            "event_abis": { "t": { "type": "event", "name": "T",
                "inputs": [{ "name": "a", "type": "address", "indexed": false }],
                "anonymous": false } },
            "condition": true,
            "action": "deny"
        }),
    ]);
    assert_eq!(mixed.selector_bearing_rule_count(), 2, "the event-only rule is not counted");
    assert_eq!(mixed.rules.len(), 3, "while all three rules are active");
}

// --------------------------------------------------------------------------------------------
// Combined per-transaction budget
// --------------------------------------------------------------------------------------------

/// A rule with one event alias and one direct-delegatecall alias, so a test can drive both
/// candidate families.
fn event_and_alias_rule(id: &str) -> Value {
    json!({
        "id": id,
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": false,
        "action": "deny"
    })
}

#[test]
fn the_budget_counts_the_event_times_alias_product() {
    use crate::test_support::log_builder::erc20_transfer;
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    let rules = rule_set(vec![event_and_alias_rule("product")]);
    // 3 decoded events x 4 observations = 12 combinations for this one rule.
    let logs: Vec<Log> =
        (0..3).map(|i| erc20_transfer(token, from, to, U256::from(i as u64))).collect();
    let items: Vec<CallObservation> =
        (0..4).map(|i| delegated(implementation(), B256::repeat_byte(i))).collect();

    // Bounding each family separately would have admitted this; the product must not.
    assert_eq!(
        screen_with_limit(&rules, &logs, captured(&rules, &items), 12),
        Ok(MatchOutcome::Allow),
        "the limit-th combination is accepted"
    );
    assert_eq!(
        screen_with_limit(&rules, &logs, captured(&rules, &items), 11),
        Err(MatchError::BindingBudgetExceeded {
            rule_id: "product".to_string(),
            attempted: 12,
            limit: 11,
        }),
        "the (limit+1)-th combination exceeds the budget"
    );
}

#[test]
fn a_suppressed_rule_contributes_zero_to_the_budget() {
    use crate::test_support::log_builder::erc20_transfer;
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    let rules = rule_set(vec![event_and_alias_rule("suppressed")]);
    let logs = vec![erc20_transfer(token, from, to, U256::from(1u64))];
    // The alias produced no observation, so the rule is skipped before the budget is charged:
    // even a budget of zero is not exceeded, which is strictly less than the one candidate a
    // null-binding model would have cost.
    assert_eq!(screen_with_limit(&rules, &logs, captured(&rules, &[]), 0), Ok(MatchOutcome::Allow));
}

#[test]
fn call_kind_observations_do_not_enter_the_binding_count() {
    // The disclosed consequence of leaving capture untouched, measured rather than assumed:
    // `Call` observations are still committed and still consume the capture budget, but they are
    // not candidates, so they add nothing to the matcher's own binding count.
    let rules = rule_set(vec![deny_on_alias("guard", "p", proxy(), json!(false))]);
    let one_delegatecall_three_calls = [
        CallObservation::call(proxy(), proxy_code()),
        delegated(implementation(), approved_impl_code()),
        CallObservation::call(proxy(), proxy_code()),
        CallObservation::call(proxy(), proxy_code()),
    ];
    assert_eq!(
        screen_with_limit(&rules, &[], captured(&rules, &one_delegatecall_three_calls), 1),
        Ok(MatchOutcome::Allow),
        "four observations, one candidate"
    );
    assert!(
        screen_with_limit(&rules, &[], captured(&rules, &one_delegatecall_three_calls), 0).is_err(),
        "and that one candidate is still charged"
    );
}

#[test]
fn per_rule_counts_accumulate_across_rules() {
    let rules = rule_set(vec![
        deny_on_alias("r1", "a", proxy(), json!(false)),
        deny_on_alias("r2", "b", proxy(), json!(false)),
    ]);
    let items =
        [delegated(implementation(), approved_impl_code()), delegated(library(), rogue_code())];
    // 2 + 2 across the two rules.
    assert_eq!(
        screen_with_limit(&rules, &[], captured(&rules, &items), 4),
        Ok(MatchOutcome::Allow)
    );
    assert!(screen_with_limit(&rules, &[], captured(&rules, &items), 3).is_err());
}

#[test]
fn candidate_count_overflow_fails_closed() {
    // Eight aliases, each matching enough observations that the product overflows `usize` rather
    // than merely exceeding the configured limit. Overflow is itself a budget failure.
    let mut aliases = serde_json::Map::new();
    for i in 0..crate::rules::MAX_DIRECT_DELEGATECALL_ALIASES {
        aliases.insert(format!("p{i}"), Value::String(hex_addr(proxy())));
    }
    let rules = rule_set(vec![json!({
        "id": "overflow",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": Value::Object(aliases) },
        "condition": false,
        "action": "deny"
    })]);
    // Eight aliases each holding `n` candidates: with n = 2^9 the product is 2^72, well past
    // `usize::MAX` on a 64-bit target.
    let items: Vec<CallObservation> =
        (0..512).map(|i| delegated(implementation(), B256::repeat_byte(i as u8))).collect();
    let error = screen(&rules, &[], captured(&rules, &items)).expect_err("must fail closed");
    assert_eq!(error, MatchError::BindingCountOverflow { rule_id: "overflow".to_string() });
    assert_eq!(error.kind(), MatchErrorKind::BindingBudget);
}

// --------------------------------------------------------------------------------------------
// Snapshot lifetime: hot reload, pinning, and per-simulation re-capture
// --------------------------------------------------------------------------------------------

#[test]
fn a_hot_reload_recomputes_the_alias_set_and_changes_the_stamp() {
    use crate::clock::SystemClock;
    use crate::config::FilterConfig;
    use crate::handle::FilterHandle;
    use std::sync::Arc;

    let first = rule_set(vec![deny_on_alias("r1", "p", proxy(), json!(true))]);
    let first_stamp = first.stamp();
    let handle = FilterHandle::for_test(FilterConfig::default(), first, Arc::new(SystemClock));

    // A snapshot acquired once is a stable `Arc`: installing a new rule set cannot retroactively
    // change what an already-pinned block is screening against.
    let pinned = handle.rule_snapshot();
    assert_eq!(pinned.callstack_selectors(), [proxy()]);

    let second = load_rules(PV, 2, vec![raw(deny_on_alias("r2", "p", other_proxy(), json!(true)))]);
    handle.install_rules(second);

    assert_eq!(
        pinned.callstack_selectors(),
        [proxy()],
        "the pinned snapshot is unaffected by a later install"
    );
    assert_eq!(pinned.stamp(), first_stamp);

    // A newly acquired snapshot sees the reload, with a recomputed alias set and a new stamp.
    let refreshed = handle.rule_snapshot();
    assert_eq!(refreshed.callstack_selectors(), [other_proxy()]);
    assert_ne!(refreshed.stamp(), first_stamp);
    assert_ne!(refreshed.stamp().selector_digest, first_stamp.selector_digest);
}

#[test]
fn observations_captured_under_the_pinned_snapshot_are_adjudicated_by_it() {
    use crate::clock::SystemClock;
    use crate::config::FilterConfig;
    use crate::handle::{FilterHandle, Screen};
    use std::sync::Arc;

    let rules = rule_set(vec![deny_on_alias(
        "guard",
        "p",
        proxy(),
        delegate_code_hash_allowlist("p", &[approved_impl_code()]),
    )]);
    let handle = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();

    // Capture configured from `pinned` and screened against the same `Arc`.
    let rogue = [delegated(library(), rogue_code())];
    let input = ScreenInput {
        tx_hash: B256::repeat_byte(0x01),
        origin: "0x9999999999999999999999999999999999999999".parse().unwrap(),
        tx_to: Some(proxy()),
        nonce: 0,
        value: U256::ZERO,
        block_height: 1,
        logs: &[],
        observations: Observations::available(pinned.stamp(), &rogue),
    };
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::Deny);

    // A reload lands, but the block in flight keeps screening against its pinned snapshot, so a
    // transaction captured under it is still adjudicated under it.
    handle.install_rules(load_rules(
        PV,
        99,
        vec![raw(deny_on_alias("other", "p", other_proxy(), json!(true)))],
    ));
    let input = ScreenInput { tx_hash: B256::repeat_byte(0x02), ..input };
    assert_eq!(
        handle.screen_tx_with_snapshot(&input, &pinned),
        Screen::Deny,
        "a mid-block reload must not reinterpret a transaction already captured"
    );

    // Screening the same observations against the *new* snapshot is a stamp mismatch, which fails
    // closed rather than silently suppressing the rule.
    let refreshed = handle.rule_snapshot();
    let input = ScreenInput { tx_hash: B256::repeat_byte(0x03), ..input };
    assert_eq!(
        handle.screen_tx_with_snapshot(&input, &refreshed),
        Screen::Deny,
        "observations from another snapshot must never be read as a clean no-match"
    );
}

#[test]
fn screen_tx_without_an_explicit_snapshot_uses_the_current_one() {
    use crate::clock::SystemClock;
    use crate::config::FilterConfig;
    use crate::handle::{FilterHandle, Screen};
    use std::sync::Arc;

    let rules = rule_set(vec![deny_on_alias("guard", "p", proxy(), json!(true))]);
    let handle = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let input = ScreenInput {
        tx_hash: B256::repeat_byte(0x04),
        origin: "0x9999999999999999999999999999999999999999".parse().unwrap(),
        tx_to: Some(proxy()),
        nonce: 0,
        value: U256::ZERO,
        block_height: 1,
        logs: &[],
        observations: Observations::NOT_CAPTURED,
    };
    // A callstack rule with no observations available fails closed, never allows.
    assert_eq!(handle.screen_tx(&input), Screen::Deny);
}

/// Unit-level check of the handle contract only: it hands the re-simulation observations that
/// already carry the saved snapshot's stamp, so it pins what `screen_existing` evaluates against
/// and nothing about how a caller obtains that stamp.
///
/// The end-to-end counterpart — a candidate captured and screened through the real builder chain
/// across a cross-block reload — lives where the capture is wired, in
/// `xlayer-builder`'s `approved_resimulation_after_a_cross_block_reload_uses_the_saved_snapshot`.
/// Keep both: this one would still pass if the builder configured capture from the wrong snapshot.
#[test]
fn an_approved_transaction_re_evaluates_under_its_saved_snapshot() {
    use crate::clock::SystemClock;
    use crate::config::FilterConfig;
    use crate::handle::{FilterHandle, Screen};
    use crate::pool::BufferStatus;
    use crate::test_support::log_builder::erc20_transfer;
    use std::sync::Arc;

    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    // An audit rule gated on a call-frame observation: the event supplies the action payload
    // while the frame acts as a condition.
    let audit_rule = json!({
        "id": "gated-audit",
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": { "==": [ { "var": "p.delegateTo" }, hex_addr(implementation()) ] },
        "action": "audit",
        "audit_types": ["delegatecall_review"]
    });
    let rules = rule_set(vec![audit_rule]);
    let handle = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();

    let logs = vec![erc20_transfer(token, from, to, U256::from(3u64))];
    let observed = [delegated(implementation(), approved_impl_code())];
    let tx_hash = B256::repeat_byte(0x05);
    let input = ScreenInput {
        tx_hash,
        origin: from,
        tx_to: Some(proxy()),
        nonce: 0,
        value: U256::ZERO,
        block_height: 1,
        logs: &logs,
        observations: Observations::available(pinned.stamp(), &observed),
    };
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::AuditPending);

    // A reload lands while the transaction is buffered, then it is approved and re-simulated.
    handle.install_rules(load_rules(
        PV,
        77,
        vec![raw(deny_on_alias("unrelated", "p", other_proxy(), json!(true)))],
    ));
    handle.advance_to_approved_for_test(&tx_hash);
    assert_eq!(handle.buffer_status(&tx_hash), Some(BufferStatus::Approved));

    // The re-simulation re-captures its own observations and they are stamped with the SAVED
    // snapshot, not the newly installed one, so the consistency check succeeds.
    let recaptured = [delegated(implementation(), approved_impl_code())];
    let reinput =
        ScreenInput { observations: Observations::available(pinned.stamp(), &recaptured), ..input };
    assert_eq!(
        handle.screen_tx(&reinput),
        Screen::AuditApproved,
        "a buffered transaction is never reinterpreted under a newer rule set"
    );
}

/// The accessor a capturing caller uses to pick a buffered candidate's snapshot keeps naming the
/// SAVED snapshot across a reload, and reports `None` for a transaction with no entry so the
/// caller falls back to its own pinned snapshot.
#[test]
fn the_saved_snapshot_of_a_buffered_transaction_survives_a_reload() {
    use crate::clock::SystemClock;
    use crate::config::FilterConfig;
    use crate::handle::{FilterHandle, Screen};
    use crate::test_support::log_builder::erc20_transfer;
    use std::sync::Arc;

    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    let rules = rule_set(vec![json!({
        "id": "gated-audit",
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": { "==": [ { "var": "p.delegateTo" }, hex_addr(implementation()) ] },
        "action": "audit",
        "audit_types": ["delegatecall_review"]
    })]);
    let handle = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();

    let tx_hash = B256::repeat_byte(0x07);
    assert!(
        handle.saved_rule_snapshot(&tx_hash).is_none(),
        "an unbuffered transaction has no saved snapshot, so the caller's pinned one applies"
    );

    let logs = vec![erc20_transfer(token, from, to, U256::from(3u64))];
    let observed = [delegated(implementation(), approved_impl_code())];
    let input = ScreenInput {
        tx_hash,
        origin: from,
        tx_to: Some(proxy()),
        nonce: 0,
        value: U256::ZERO,
        block_height: 1,
        logs: &logs,
        observations: Observations::available(pinned.stamp(), &observed),
    };
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::AuditPending);

    // A reload changes only the content version; the buffered entry must keep its own snapshot.
    handle.install_rules(load_rules(
        PV,
        99,
        vec![raw(deny_on_alias("unrelated", "p", other_proxy(), json!(true)))],
    ));
    assert_ne!(handle.rule_snapshot().stamp(), pinned.stamp(), "the live snapshot did move");
    assert_eq!(
        handle.saved_rule_snapshot(&tx_hash).map(|rules| rules.stamp()),
        Some(pinned.stamp()),
        "the buffered candidate's capture configuration must come from its saved snapshot"
    );
}

#[test]
fn a_re_simulation_whose_executed_code_changed_is_not_silently_consistent() {
    use crate::clock::SystemClock;
    use crate::config::FilterConfig;
    use crate::handle::{FilterHandle, Screen};
    use crate::pool::BufferStatus;
    use crate::test_support::log_builder::erc20_transfer;
    use std::sync::Arc;

    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    // The audit payload is gated on the proxy delegating into approved code. Observations are
    // facts about one specific simulation and are never reused, so a re-simulation that executed
    // different code must be seen as different rather than inheriting the earlier verdict.
    let rules = rule_set(vec![json!({
        "id": "gated-audit",
        "event_abis": { "t": transfer_abi() },
        "callstack": { "direct_delegatecalls": { "p": hex_addr(proxy()) } },
        "condition": { "in": [ { "var": "p.delegateCodeHash" }, [hex_hash(approved_impl_code())] ] },
        "action": "audit",
        "audit_types": ["delegatecall_review"]
    })]);
    let handle = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();

    let logs = vec![erc20_transfer(token, from, to, U256::from(3u64))];
    let first = [delegated(implementation(), approved_impl_code())];
    let tx_hash = B256::repeat_byte(0x06);
    let input = ScreenInput {
        tx_hash,
        origin: from,
        tx_to: Some(proxy()),
        nonce: 0,
        value: U256::ZERO,
        block_height: 1,
        logs: &logs,
        observations: Observations::available(pinned.stamp(), &first),
    };
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::AuditPending);

    handle.advance_to_approved_for_test(&tx_hash);
    assert_eq!(handle.buffer_status(&tx_hash), Some(BufferStatus::Approved));

    // The re-simulation executed DIFFERENT code, so the gating condition no longer holds and the
    // recomputed action payload differs from the submitted one.
    let changed = [delegated(implementation(), rogue_code())];
    let reinput =
        ScreenInput { observations: Observations::available(pinned.stamp(), &changed), ..input };
    assert_eq!(
        handle.screen_tx(&reinput),
        Screen::Drop,
        "a changed executed code hash must not pass the consistency check"
    );
}
