//! Rule-side acceptance coverage for call-frame selectors: wire schema, load-time rejections,
//! condition bindings, candidate enumeration, the combined per-transaction budget, and the
//! difference between "observed nothing" and "could not observe".
//!
//! Frame-discrimination behavior (which frames become observations at all) is covered where the
//! inspector lives, in the block builder.

use alloy_primitives::{Address, Log, B256, U256};
use serde_json::{json, Value};

use crate::callstack::{CallObservation, ObservationUnavailable, Observations, SnapshotStamp};
use crate::handle::ScreenInput;
use crate::matching::{try_evaluate, MatchError, MatchErrorKind, MatchOutcome};
use crate::rules::{
    compile_rule, compile_rule_at_protocol, load_rules, RawRule, RuleSet,
    DEFAULT_MAX_EVENT_BINDINGS_PER_TX,
};

/// The protocol version that defines the callstack section.
const PV_CALLSTACK: u32 = crate::config::CALLSTACK_MIN_PROTOCOL_VERSION;

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

fn approved_proxy_code() -> B256 {
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

/// A `deny` rule with one selector on `address` and the given condition.
fn deny_on_selector(id: &str, name: &str, address: Address, condition: Value) -> Value {
    json!({
        "id": id,
        "callstack": { name: { "address": hex_addr(address) } },
        "condition": condition,
        "action": "deny"
    })
}

/// Guarded allowlist deny: "this address matched, and the code it ran is not approved".
fn code_hash_allowlist(name: &str, approved: &[B256]) -> Value {
    let list: Vec<Value> = approved.iter().map(|h| Value::String(hex_hash(*h))).collect();
    json!({ "and": [
        { "==": [ { "var": format!("{name}.matched") }, true ] },
        { "!": { "in": [ { "var": format!("{name}.codeHash") }, list ] } }
    ]})
}

/// Guarded allowlist deny on the direct delegatecall target.
fn impl_code_hash_allowlist(name: &str, approved: &[B256]) -> Value {
    let list: Vec<Value> = approved.iter().map(|h| Value::String(hex_hash(*h))).collect();
    json!({ "and": [
        { "==": [ { "var": format!("{name}.callType") }, "direct_delegatecall" ] },
        { "!": { "in": [ { "var": format!("{name}.implCodeHash") }, list ] } }
    ]})
}

fn rule_set(rules: Vec<Value>) -> RuleSet {
    let set = load_rules(PV_CALLSTACK, 1, rules.into_iter().map(raw).collect());
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

// --------------------------------------------------------------------------------------------
// Functional behavior
// --------------------------------------------------------------------------------------------

#[test]
fn direct_call_with_approved_code_is_allowed() {
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        code_hash_allowlist("entry", &[approved_proxy_code()]),
    )]);
    let items = [CallObservation::call(proxy(), approved_proxy_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Allow));
}

#[test]
fn direct_call_with_unapproved_code_is_denied() {
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        code_hash_allowlist("entry", &[approved_proxy_code()]),
    )]);
    let items = [CallObservation::call(proxy(), rogue_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));
}

#[test]
fn direct_delegatecall_to_approved_implementation_is_allowed() {
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        impl_code_hash_allowlist("entry", &[approved_impl_code()]),
    )]);
    let items = [CallObservation::direct_delegatecall(
        proxy(),
        approved_proxy_code(),
        implementation(),
        approved_impl_code(),
    )];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Allow));
}

#[test]
fn direct_delegatecall_to_unapproved_implementation_is_denied() {
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        impl_code_hash_allowlist("entry", &[approved_impl_code()]),
    )]);
    let items = [CallObservation::direct_delegatecall(
        proxy(),
        approved_proxy_code(),
        implementation(),
        rogue_code(),
    )];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));
}

#[test]
fn observed_values_are_inspectable_without_selector_constraints() {
    // A selector carrying only an address observes every matching frame and exposes the actual
    // values, which is what lets a condition own the allowlist.
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        json!({ "and": [
            { "==": [ { "var": "entry.address" }, hex_addr(proxy()) ] },
            { "==": [ { "var": "entry.implAddress" }, hex_addr(implementation()) ] },
            { "==": [ { "var": "entry.callType" }, "direct_delegatecall" ] }
        ]}),
    )]);
    let items = [CallObservation::direct_delegatecall(
        proxy(),
        approved_proxy_code(),
        implementation(),
        approved_impl_code(),
    )];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));
}

#[test]
fn each_observation_is_an_independent_alternative() {
    // Several frames for one selector are alternatives: any one of them satisfying a deny
    // condition denies the transaction, even when an earlier one is approved.
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        code_hash_allowlist("entry", &[approved_proxy_code()]),
    )]);
    let approved_only = [
        CallObservation::call(proxy(), approved_proxy_code()),
        CallObservation::call(proxy(), approved_proxy_code()),
    ];
    assert_eq!(screen(&rules, &[], captured(&rules, &approved_only)), Ok(MatchOutcome::Allow));

    let one_rogue = [
        CallObservation::call(proxy(), approved_proxy_code()),
        CallObservation::call(proxy(), rogue_code()),
    ];
    assert_eq!(screen(&rules, &[], captured(&rules, &one_rogue)), Ok(MatchOutcome::Deny));
}

#[test]
fn a_standard_proxy_invocation_produces_both_observation_kinds() {
    // The pair a proxy call legitimately yields: the proxy running its own code, and that code's
    // direct delegatecall. A rule may reason about either.
    let own_code_rule = rule_set(vec![deny_on_selector(
        "own",
        "entry",
        proxy(),
        code_hash_allowlist("entry", &[approved_proxy_code()]),
    )]);
    let impl_rule = rule_set(vec![deny_on_selector(
        "impl",
        "entry",
        proxy(),
        impl_code_hash_allowlist("entry", &[approved_impl_code()]),
    )]);
    let items = [
        CallObservation::call(proxy(), approved_proxy_code()),
        CallObservation::direct_delegatecall(
            proxy(),
            approved_proxy_code(),
            implementation(),
            rogue_code(),
        ),
    ];
    // The proxy's own code is approved, so the own-code rule does not fire...
    assert_eq!(
        screen(&own_code_rule, &[], captured(&own_code_rule, &items)),
        Ok(MatchOutcome::Allow)
    );
    // ...while the unapproved implementation is still caught by the impl rule.
    assert_eq!(screen(&impl_rule, &[], captured(&impl_rule, &items)), Ok(MatchOutcome::Deny));
}

#[test]
fn two_selectors_keep_independent_bindings() {
    let rules = rule_set(vec![json!({
        "id": "two-selectors",
        "callstack": {
            "a": { "address": hex_addr(proxy()) },
            "b": { "address": hex_addr(other_proxy()) }
        },
        // Denies only when `a` ran unapproved code; `b`'s own allowlist is separate and satisfied.
        "condition": { "and": [
            { "==": [ { "var": "a.matched" }, true ] },
            { "!": { "in": [ { "var": "a.codeHash" }, [hex_hash(approved_proxy_code())] ] } },
            { "==": [ { "var": "b.matched" }, true ] },
            { "in": [ { "var": "b.codeHash" }, [hex_hash(approved_impl_code())] ] }
        ]},
        "action": "deny"
    })]);

    // `a` approved → no deny, and `b`'s binding did not leak into `a`'s.
    let both_fine = [
        CallObservation::call(proxy(), approved_proxy_code()),
        CallObservation::call(other_proxy(), approved_impl_code()),
    ];
    assert_eq!(screen(&rules, &[], captured(&rules, &both_fine)), Ok(MatchOutcome::Allow));

    let a_rogue = [
        CallObservation::call(proxy(), rogue_code()),
        CallObservation::call(other_proxy(), approved_impl_code()),
    ];
    assert_eq!(screen(&rules, &[], captured(&rules, &a_rogue)), Ok(MatchOutcome::Deny));
}

#[test]
fn no_matching_observation_binds_matched_false_and_five_nulls() {
    let rules = rule_set(vec![json!({
        "id": "shape",
        "callstack": { "entry": { "address": hex_addr(proxy()) } },
        // Asserts the exact no-match shape required of the bindings.
        "condition": { "and": [
            { "==": [ { "var": "entry.matched" }, false ] },
            { "==": [ { "var": "entry.callType" }, null ] },
            { "==": [ { "var": "entry.address" }, null ] },
            { "==": [ { "var": "entry.codeHash" }, null ] },
            { "==": [ { "var": "entry.implAddress" }, null ] },
            { "==": [ { "var": "entry.implCodeHash" }, null ] }
        ]},
        "action": "deny"
    })]);
    // Capture ran and saw a frame for a different address: the selector has no match.
    let elsewhere = [CallObservation::call(other_proxy(), approved_proxy_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &elsewhere)), Ok(MatchOutcome::Deny));
}

#[test]
fn no_match_does_not_deny_an_unrelated_transaction() {
    // The hazard the mandatory `matched` guard exists for: a negated allowlist over a null hash
    // evaluates true on its own, because the evaluator treats a one-sided null as not equal. A
    // guarded rule must leave an unrelated transaction alone.
    let guarded = rule_set(vec![deny_on_selector(
        "guarded",
        "entry",
        proxy(),
        code_hash_allowlist("entry", &[approved_proxy_code()]),
    )]);
    assert_eq!(screen(&guarded, &[], captured(&guarded, &[])), Ok(MatchOutcome::Allow));

    // And the unguarded form really is the trap it is documented to be, so the guard is load-
    // bearing rather than decorative.
    let unguarded = rule_set(vec![deny_on_selector(
        "unguarded",
        "entry",
        proxy(),
        json!({ "!": { "in": [ { "var": "entry.codeHash" }, [hex_hash(approved_proxy_code())] ] } }),
    )]);
    assert_eq!(screen(&unguarded, &[], captured(&unguarded, &[])), Ok(MatchOutcome::Deny));
}

#[test]
fn selector_exact_values_narrow_which_observations_match() {
    // A fully pinned selector matches only that exact path.
    let rules = rule_set(vec![json!({
        "id": "pinned",
        "callstack": { "entry": {
            "address": hex_addr(proxy()),
            "codeHash": hex_hash(approved_proxy_code()),
            "implAddress": hex_addr(implementation()),
            "implCodeHash": hex_hash(approved_impl_code())
        }},
        "condition": { "==": [ { "var": "entry.matched" }, true ] },
        "action": "deny"
    })]);

    let exact = [CallObservation::direct_delegatecall(
        proxy(),
        approved_proxy_code(),
        implementation(),
        approved_impl_code(),
    )];
    assert_eq!(screen(&rules, &[], captured(&rules, &exact)), Ok(MatchOutcome::Deny));

    // A different implementation code hash is a different path and does not match.
    let different = [CallObservation::direct_delegatecall(
        proxy(),
        approved_proxy_code(),
        implementation(),
        rogue_code(),
    )];
    assert_eq!(screen(&rules, &[], captured(&rules, &different)), Ok(MatchOutcome::Allow));

    // Neither does a plain call observation, which carries no impl values at all.
    let plain = [CallObservation::call(proxy(), approved_proxy_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &plain)), Ok(MatchOutcome::Allow));
}

#[test]
fn event_and_callstack_conditions_combine_in_one_rule() {
    use crate::test_support::log_builder::erc20_transfer;
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    let rules = rule_set(vec![json!({
        "id": "combined",
        "event_abis": { "t": {
            "type": "event",
            "name": "Transfer",
            "inputs": [
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ],
            "anonymous": false
        }},
        "callstack": { "entry": { "address": hex_addr(proxy()) } },
        // Both families must be satisfied for the deny to fire.
        "condition": { "and": [
            { "==": [ { "var": "t.from" }, hex_addr(from) ] },
            { "==": [ { "var": "entry.matched" }, true ] },
            { "!": { "in": [ { "var": "entry.codeHash" }, [hex_hash(approved_proxy_code())] ] } }
        ]},
        "action": "deny"
    })]);

    let logs = vec![erc20_transfer(token, from, to, U256::from(5u64))];
    let rogue = [CallObservation::call(proxy(), rogue_code())];
    assert_eq!(screen(&rules, &logs, captured(&rules, &rogue)), Ok(MatchOutcome::Deny));

    // The event alone is not enough.
    let approved = [CallObservation::call(proxy(), approved_proxy_code())];
    assert_eq!(screen(&rules, &logs, captured(&rules, &approved)), Ok(MatchOutcome::Allow));

    // Nor is the frame alone, without the matching event.
    assert_eq!(screen(&rules, &[], captured(&rules, &rogue)), Ok(MatchOutcome::Allow));
}

#[test]
fn an_upgraded_and_rolled_back_implementation_is_still_caught() {
    // The motivating case: within one transaction the proxy is pointed at unapproved code, that
    // code executes, and the pointer is restored. Only the execution-time observation records
    // what ran, so the rule still fires — nothing here is derived from the final pointer.
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        impl_code_hash_allowlist("entry", &[approved_impl_code()]),
    )]);
    let items = [
        // pointer set to the rogue implementation, then executed
        CallObservation::direct_delegatecall(
            proxy(),
            approved_proxy_code(),
            library(),
            rogue_code(),
        ),
        // pointer restored and the approved implementation executed afterwards
        CallObservation::direct_delegatecall(
            proxy(),
            approved_proxy_code(),
            implementation(),
            approved_impl_code(),
        ),
    ];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));
}

// --------------------------------------------------------------------------------------------
// Load-time validation
// --------------------------------------------------------------------------------------------

fn reject_reason(value: Value) -> String {
    compile_rule_at_protocol(raw(value), PV_CALLSTACK)
        .expect_err("rule must be rejected")
        .to_string()
}

#[test]
fn selector_without_an_address_is_rejected() {
    let reason = reject_reason(json!({
        "id": "r",
        "callstack": { "entry": { "codeHash": hex_hash(approved_proxy_code()) } },
        "condition": true,
        "action": "deny"
    }));
    assert!(reason.contains("no address"), "{reason}");
}

#[test]
fn impl_address_and_impl_code_hash_must_be_supplied_together() {
    for selector in [
        json!({ "address": hex_addr(proxy()), "implAddress": hex_addr(implementation()) }),
        json!({ "address": hex_addr(proxy()), "implCodeHash": hex_hash(approved_impl_code()) }),
    ] {
        let reason = reject_reason(json!({
            "id": "r",
            "callstack": { "entry": selector },
            "condition": true,
            "action": "deny"
        }));
        assert!(reason.contains("together or not at all"), "{reason}");
    }

    // Both together is accepted.
    assert!(compile_rule_at_protocol(
        raw(json!({
            "id": "r",
            "callstack": { "entry": {
                "address": hex_addr(proxy()),
                "implAddress": hex_addr(implementation()),
                "implCodeHash": hex_hash(approved_impl_code())
            }},
            "condition": true,
            "action": "deny"
        })),
        PV_CALLSTACK
    )
    .is_ok());
}

#[test]
fn malformed_encodings_are_rejected() {
    let cases: Vec<(Value, &str)> = vec![
        // missing 0x prefix
        (json!({ "address": "1111111111111111111111111111111111111111" }), "0x-prefixed"),
        // wrong length
        (json!({ "address": "0x1111" }), "exactly 20 bytes"),
        // non-hex digit
        (json!({ "address": "0x111111111111111111111111111111111111111g" }), "hex digits only"),
        // uppercase is rejected rather than normalized: observed values are lowercase, so a
        // mixed-case selector would look active while comparing against a different string form
        (json!({ "address": "0x1111111111111111111111111111111111111A11" }), "lowercase hex"),
        // code hash with the wrong width
        (json!({ "address": hex_addr(proxy()), "codeHash": "0xdeadbeef" }), "exactly 32 bytes"),
    ];
    for (selector, expected) in cases {
        let reason = reject_reason(json!({
            "id": "r",
            "callstack": { "entry": selector },
            "condition": true,
            "action": "deny"
        }));
        assert!(reason.contains(expected), "expected {expected:?} in {reason:?}");
    }
}

#[test]
fn a_rejected_rule_refuses_the_whole_snapshot() {
    // Per-rule validation stays per-rule, but an installer that requires all-or-nothing can see
    // the rejection, so a snapshot is never silently installed missing a risk rule.
    let set = load_rules(
        PV_CALLSTACK,
        1,
        vec![
            raw(deny_on_selector("good", "entry", proxy(), json!(true))),
            raw(json!({
                "id": "bad",
                "callstack": { "entry": { "codeHash": hex_hash(rogue_code()) } },
                "condition": true,
                "action": "deny"
            })),
        ],
    );
    assert_eq!(set.rules.len(), 1);
    assert_eq!(set.rejected.len(), 1);
    assert_eq!(set.rejected[0].id, "bad");
}

#[test]
fn callstack_only_audit_is_rejected_and_event_backed_audit_is_accepted() {
    let reason = reject_reason(json!({
        "id": "r",
        "callstack": { "entry": { "address": hex_addr(proxy()) } },
        "condition": true,
        "action": "audit"
    }));
    assert!(reason.contains("no event payload content"), "{reason}");

    // With declared events the audit payload has content, so the rule is valid.
    assert!(compile_rule_at_protocol(
        raw(json!({
            "id": "r",
            "event_abis": { "t": {
                "type": "event",
                "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to", "type": "address", "indexed": true},
                    {"name": "value", "type": "uint256", "indexed": false}
                ],
                "anonymous": false
            }},
            "callstack": { "entry": { "address": hex_addr(proxy()) } },
            "condition": true,
            "action": "audit"
        })),
        PV_CALLSTACK
    )
    .is_ok());
}

#[test]
fn a_rule_with_neither_selector_family_is_rejected() {
    let reason = reject_reason(json!({ "id": "r", "condition": true, "action": "deny" }));
    assert!(reason.contains("neither event_abis nor callstack"), "{reason}");

    let reason = reject_reason(json!({
        "id": "r", "event_abis": {}, "callstack": {}, "condition": true, "action": "deny"
    }));
    assert!(reason.contains("neither event_abis nor callstack"), "{reason}");
}

#[test]
fn a_callstack_only_rule_may_omit_event_abis() {
    let compiled = compile_rule_at_protocol(
        raw(deny_on_selector("callstack-only", "entry", proxy(), json!(true))),
        PV_CALLSTACK,
    )
    .expect("a rule selecting purely on call frames is valid");
    assert!(compiled.events.is_empty());
    assert_eq!(compiled.callstack.len(), 1);
    assert_eq!(compiled.callstack[0].address, proxy());
}

#[test]
fn too_many_selectors_in_one_rule_is_rejected() {
    let mut selectors = serde_json::Map::new();
    for i in 0..=crate::rules::MAX_CALLSTACK_SELECTORS_PER_RULE {
        selectors.insert(
            format!("s{i}"),
            json!({ "address": hex_addr(Address::repeat_byte(i as u8 + 1)) }),
        );
    }
    let reason = reject_reason(json!({
        "id": "r",
        "callstack": Value::Object(selectors),
        "condition": true,
        "action": "deny"
    }));
    assert!(reason.contains("selectors; at most"), "{reason}");
}

#[test]
fn a_selector_name_colliding_with_an_event_alias_is_rejected() {
    // Condition variables share one flat namespace, so a collision would silently shadow one of
    // the two families instead of being visible to the author.
    let reason = reject_reason(json!({
        "id": "r",
        "event_abis": { "entry": {
            "type": "event",
            "name": "Transfer",
            "inputs": [
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ],
            "anonymous": false
        }},
        "callstack": { "entry": { "address": hex_addr(proxy()) } },
        "condition": true,
        "action": "deny"
    }));
    assert!(reason.contains("collides with an event_abis alias"), "{reason}");
}

#[test]
fn a_duplicate_selector_name_is_rejected_at_deserialization() {
    // A plain map would keep the last occurrence and silently drop the first, so the duplicate is
    // surfaced instead of collapsing an author-visible selector.
    let json = r#"{
        "id": "r",
        "callstack": {
            "entry": {"address": "0x1111111111111111111111111111111111111111"},
            "entry": {"address": "0x2222222222222222222222222222222222222222"}
        },
        "condition": true,
        "action": "deny"
    }"#;
    let error = serde_json::from_str::<RawRule>(json).expect_err("duplicate name must be rejected");
    assert!(error.to_string().contains("duplicate callstack selector name"), "{error}");
}

// --------------------------------------------------------------------------------------------
// Protocol version and rollout
// --------------------------------------------------------------------------------------------

#[test]
fn both_protocol_versions_are_supported() {
    assert!(crate::config::is_supported_protocol(1));
    assert!(crate::config::is_supported_protocol(PV_CALLSTACK));
    assert!(!crate::config::is_supported_protocol(PV_CALLSTACK + 1));
}

#[test]
fn callstack_under_the_older_protocol_version_is_rejected_not_ignored() {
    let rule = deny_on_selector("r", "entry", proxy(), json!(true));
    let reason = compile_rule_at_protocol(raw(rule.clone()), 1)
        .expect_err("callstack is not defined at protocol version 1")
        .to_string();
    assert!(reason.contains("protocol_version"), "{reason}");

    // The whole snapshot is refused, so an older node keeps its last known good rules rather than
    // running a rule whose risk condition it cannot evaluate.
    let set = load_rules(1, 9, vec![raw(rule)]);
    assert!(set.rules.is_empty());
    assert_eq!(set.rejected.len(), 1);

    // `compile_rule`'s standalone entry point defaults to the oldest version for the same reason.
    assert!(compile_rule(raw(deny_on_selector("r", "entry", proxy(), json!(true)))).is_err());
}

#[test]
fn event_only_rules_are_unaffected_by_the_new_protocol_version() {
    let event_rule = json!({
        "id": "event-only",
        "event_abis": { "t": {
            "type": "event",
            "name": "Transfer",
            "inputs": [
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ],
            "anonymous": false
        }},
        "condition": true,
        "action": "deny"
    });
    for pv in [1, PV_CALLSTACK] {
        let set = load_rules(pv, 1, vec![raw(event_rule.clone())]);
        assert!(set.rejected.is_empty(), "pv {pv}: {:?}", set.rejected);
        assert_eq!(set.rules.len(), 1);
        assert!(set.rules[0].callstack.is_empty());
        assert!(!set.has_callstack_rules());
        assert!(set.callstack_selectors().is_empty());
    }
}

#[test]
fn rules_without_callstack_are_unaffected_by_unavailable_observations() {
    // The compatibility guarantee: an execution path that captures no frames changes nothing for
    // a rule that does not ask about frames.
    let rules = rule_set(vec![json!({
        "id": "event-only",
        "event_abis": { "t": {
            "type": "event",
            "name": "Transfer",
            "inputs": [
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ],
            "anonymous": false
        }},
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
// Unavailable vs. no-match, and snapshot identity
// --------------------------------------------------------------------------------------------

#[test]
fn a_callstack_rule_under_unavailable_observations_never_allows() {
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        code_hash_allowlist("entry", &[approved_proxy_code()]),
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
fn an_empty_captured_set_is_a_genuine_no_match_not_an_error() {
    // The distinction that matters: capture ran and saw nothing, which a guarded rule correctly
    // treats as "does not apply".
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        code_hash_allowlist("entry", &[approved_proxy_code()]),
    )]);
    assert_eq!(screen(&rules, &[], captured(&rules, &[])), Ok(MatchOutcome::Allow));
}

#[test]
fn a_snapshot_stamp_mismatch_fails_closed_and_never_binds_matched_false() {
    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        code_hash_allowlist("entry", &[approved_proxy_code()]),
    )]);
    let own = rules.stamp();

    // Same selector addresses, different rule content. The addresses alone cannot prove the
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

    // A different protocol version and a different selector set are likewise mismatches.
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
fn the_selector_set_is_the_sorted_union_over_active_rules() {
    let rules = rule_set(vec![
        deny_on_selector("r1", "a", other_proxy(), json!(true)),
        deny_on_selector("r2", "b", proxy(), json!(true)),
        // A repeat of an address already named by another rule contributes once.
        deny_on_selector("r3", "c", proxy(), json!(true)),
    ]);
    assert_eq!(rules.callstack_selectors(), [proxy(), other_proxy()]);
    assert!(rules.has_callstack_rules());

    // The digest depends on set membership only, never on declaration order.
    let reordered = rule_set(vec![
        deny_on_selector("r3", "c", proxy(), json!(true)),
        deny_on_selector("r2", "b", proxy(), json!(true)),
        deny_on_selector("r1", "a", other_proxy(), json!(true)),
    ]);
    assert_eq!(reordered.stamp().selector_digest, rules.stamp().selector_digest);
}

// --------------------------------------------------------------------------------------------
// Combined per-transaction budget
// --------------------------------------------------------------------------------------------

/// A rule with one event alias and one selector, so a test can drive both candidate families.
fn event_and_selector_rule(id: &str) -> Value {
    json!({
        "id": id,
        "event_abis": { "t": {
            "type": "event",
            "name": "Transfer",
            "inputs": [
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ],
            "anonymous": false
        }},
        "callstack": { "entry": { "address": hex_addr(proxy()) } },
        "condition": false,
        "action": "deny"
    })
}

#[test]
fn the_budget_counts_the_event_times_callstack_product() {
    use crate::test_support::log_builder::erc20_transfer;
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    let rules = rule_set(vec![event_and_selector_rule("product")]);
    // 3 decoded events x 4 observations = 12 combinations for this one rule.
    let logs: Vec<Log> =
        (0..3).map(|i| erc20_transfer(token, from, to, U256::from(i as u64))).collect();
    let items: Vec<CallObservation> =
        (0..4).map(|i| CallObservation::call(proxy(), B256::repeat_byte(i))).collect();

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
fn a_no_match_selector_contributes_exactly_one_candidate() {
    use crate::test_support::log_builder::erc20_transfer;
    let token: Address = "0x5555555555555555555555555555555555555555".parse().unwrap();
    let from: Address = "0x6666666666666666666666666666666666666666".parse().unwrap();
    let to: Address = "0x7777777777777777777777777777777777777777".parse().unwrap();

    let rules = rule_set(vec![event_and_selector_rule("one-null")]);
    let logs = vec![erc20_transfer(token, from, to, U256::from(1u64))];
    // One event candidate x one null selector candidate = exactly one combination, so a no-match
    // neither multiplies the count to zero nor skips evaluation.
    assert_eq!(screen_with_limit(&rules, &logs, captured(&rules, &[]), 1), Ok(MatchOutcome::Allow));
    assert!(screen_with_limit(&rules, &logs, captured(&rules, &[]), 0).is_err());
}

#[test]
fn per_rule_counts_accumulate_across_rules() {
    let rules = rule_set(vec![
        deny_on_selector("r1", "a", proxy(), json!(false)),
        deny_on_selector("r2", "b", proxy(), json!(false)),
    ]);
    let items = [
        CallObservation::call(proxy(), approved_proxy_code()),
        CallObservation::call(proxy(), rogue_code()),
    ];
    // 2 + 2 across the two rules.
    assert_eq!(
        screen_with_limit(&rules, &[], captured(&rules, &items), 4),
        Ok(MatchOutcome::Allow)
    );
    assert!(screen_with_limit(&rules, &[], captured(&rules, &items), 3).is_err());
}

#[test]
fn candidate_count_overflow_fails_closed() {
    // Eight selectors, each matching enough observations that the product overflows `usize`
    // rather than merely exceeding the configured limit. Overflow is itself a budget failure.
    let mut selectors = serde_json::Map::new();
    for i in 0..crate::rules::MAX_CALLSTACK_SELECTORS_PER_RULE {
        selectors.insert(format!("s{i}"), json!({ "address": hex_addr(proxy()) }));
    }
    let rules = rule_set(vec![json!({
        "id": "overflow",
        "callstack": Value::Object(selectors),
        "condition": false,
        "action": "deny"
    })]);
    // 2^8 selectors each holding `n` candidates: with n = 2^9 the product is 2^72, well past
    // `usize::MAX` on a 64-bit target.
    let items: Vec<CallObservation> =
        (0..512).map(|i| CallObservation::call(proxy(), B256::repeat_byte(i as u8))).collect();
    let error = screen(&rules, &[], captured(&rules, &items)).expect_err("must fail closed");
    assert_eq!(error, MatchError::BindingCountOverflow { rule_id: "overflow".to_string() });
    assert_eq!(error.kind(), MatchErrorKind::BindingBudget);
}

// --------------------------------------------------------------------------------------------
// Snapshot lifetime: hot reload, pinning, and per-simulation re-capture
// --------------------------------------------------------------------------------------------

#[test]
fn a_hot_reload_recomputes_the_selector_set_and_changes_the_stamp() {
    use crate::clock::SystemClock;
    use crate::config::FilterConfig;
    use crate::handle::FilterHandle;
    use std::sync::Arc;

    let first = rule_set(vec![deny_on_selector("r1", "entry", proxy(), json!(true))]);
    let first_stamp = first.stamp();
    let handle = FilterHandle::for_test(FilterConfig::default(), first, Arc::new(SystemClock));

    // A snapshot acquired once is a stable `Arc`: installing a new rule set cannot retroactively
    // change what an already-pinned block is screening against.
    let pinned = handle.rule_snapshot();
    assert_eq!(pinned.callstack_selectors(), [proxy()]);

    let second = load_rules(
        PV_CALLSTACK,
        2,
        vec![raw(deny_on_selector("r2", "entry", other_proxy(), json!(true)))],
    );
    handle.install_rules(second);

    assert_eq!(
        pinned.callstack_selectors(),
        [proxy()],
        "the pinned snapshot is unaffected by a later install"
    );
    assert_eq!(pinned.stamp(), first_stamp);

    // A newly acquired snapshot sees the reload, with a recomputed selector set and a new stamp.
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

    let rules = rule_set(vec![deny_on_selector(
        "guard",
        "entry",
        proxy(),
        code_hash_allowlist("entry", &[approved_proxy_code()]),
    )]);
    let handle = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();

    // Capture configured from `pinned` and screened against the same `Arc`.
    let rogue = [CallObservation::call(proxy(), rogue_code())];
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
        PV_CALLSTACK,
        99,
        vec![raw(deny_on_selector("other", "entry", other_proxy(), json!(true)))],
    ));
    let input = ScreenInput { tx_hash: B256::repeat_byte(0x02), ..input };
    assert_eq!(
        handle.screen_tx_with_snapshot(&input, &pinned),
        Screen::Deny,
        "a mid-block reload must not reinterpret a transaction already captured"
    );

    // Screening the same observations against the *new* snapshot is a stamp mismatch, which fails
    // closed rather than silently binding `matched = false`.
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

    // The convenience entry point stays valid for callers that capture nothing: an event-only
    // rule set behaves exactly as before.
    let rules = rule_set(vec![deny_on_selector("guard", "entry", proxy(), json!(true))]);
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
        "event_abis": { "t": {
            "type": "event",
            "name": "Transfer",
            "inputs": [
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ],
            "anonymous": false
        }},
        "callstack": { "entry": { "address": hex_addr(proxy()) } },
        "condition": { "==": [ { "var": "entry.matched" }, true ] },
        "action": "audit"
    });
    let rules = rule_set(vec![audit_rule]);
    let handle = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();

    let logs = vec![erc20_transfer(token, from, to, U256::from(3u64))];
    let observed = [CallObservation::call(proxy(), approved_proxy_code())];
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
        PV_CALLSTACK,
        77,
        vec![raw(deny_on_selector("unrelated", "entry", other_proxy(), json!(true)))],
    ));
    handle.advance_to_approved_for_test(&tx_hash);
    assert_eq!(handle.buffer_status(&tx_hash), Some(BufferStatus::Approved));

    // The re-simulation re-captures its own observations and they are stamped with the SAVED
    // snapshot, not the newly installed one, so the consistency check succeeds.
    let recaptured = [CallObservation::call(proxy(), approved_proxy_code())];
    let reinput =
        ScreenInput { observations: Observations::available(pinned.stamp(), &recaptured), ..input };
    assert_eq!(
        handle.screen_tx(&reinput),
        Screen::AuditApproved,
        "a buffered transaction is never reinterpreted under a newer rule set"
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

    // The audit payload is gated on the proxy running approved code. Observations are facts about
    // one specific simulation and are never reused, so a re-simulation that executed different
    // code must be seen as different rather than inheriting the earlier verdict.
    let rules = rule_set(vec![json!({
        "id": "gated-audit",
        "event_abis": { "t": {
            "type": "event",
            "name": "Transfer",
            "inputs": [
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ],
            "anonymous": false
        }},
        "callstack": { "entry": { "address": hex_addr(proxy()) } },
        "condition": { "in": [ { "var": "entry.codeHash" }, [hex_hash(approved_proxy_code())] ] },
        "action": "audit"
    })]);
    let handle = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock));
    let pinned = handle.rule_snapshot();

    let logs = vec![erc20_transfer(token, from, to, U256::from(3u64))];
    let first = [CallObservation::call(proxy(), approved_proxy_code())];
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
    let changed = [CallObservation::call(proxy(), rogue_code())];
    let reinput =
        ScreenInput { observations: Observations::available(pinned.stamp(), &changed), ..input };
    assert_eq!(
        handle.screen_tx(&reinput),
        Screen::Drop,
        "a changed executed code hash must not pass the consistency check"
    );
}
