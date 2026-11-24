//! Rule wire-schema tests: the rule root rejects a key it does not declare, and the
//! `callstack` section rejects every near-miss of its one field.
//!
//! Both gates run at deserialization. Before the rule-root gate existed serde discarded an
//! unrecognised key, a `callStack` typo installed as an event-only rule with the call-frame
//! constraint silently removed, and nothing downstream could tell.

use alloy_primitives::Address;
use rcs_filter::config::*;
use rcs_filter::rules::*;
use rcs_filter::test_support::golden;
use serde_json::{json, Value};

const PROXY: &str = "0x00000000000000000000000000000000000000aa";
const IMPL: &str = "0x00000000000000000000000000000000000000bb";
const CODE_HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

fn transfer_event() -> Value {
    json!({
        "type": "event", "name": "Transfer",
        "inputs": [
            { "name": "from", "type": "address", "indexed": true },
            { "name": "to", "type": "address", "indexed": true },
            { "name": "value", "type": "uint256", "indexed": false }
        ],
        "anonymous": false
    })
}

/// Loads one raw rule and returns the rejection reason, asserting that exactly one rule was
/// rejected and none installed. Only usable for a rule that still deserializes — a key the
/// `callstack` section does not declare fails earlier, during the decode itself.
fn rejection_reason(rule: Value) -> String {
    let raw: RawRule = serde_json::from_value(rule).expect("rule must still deserialize");
    let set = load_rules(rcs_filter::config::MIN_PROTOCOL_VERSION, 1, vec![raw]);
    assert!(set.rules.is_empty(), "no rule may install: {:?}", set.rules.len());
    assert_eq!(set.rejected.len(), 1, "exactly one rejection expected");
    set.rejected[0].reason.clone()
}

/// The decode-time rejection for a key the wire schema does not declare at all.
fn decode_error(rule: Value) -> String {
    serde_json::from_value::<RawRule>(rule)
        .expect_err("an undeclared key must fail the decode")
        .to_string()
}

fn accepts(rule: Value) -> RuleSet {
    let raw: RawRule = serde_json::from_value(rule).expect("rule must deserialize");
    let set = load_rules(rcs_filter::config::MIN_PROTOCOL_VERSION, 1, vec![raw]);
    assert!(set.rejected.is_empty(), "unexpected rejection: {:?}", set.rejected);
    set
}

/// a top-level `callStack` typo on a rule that carries a valid `event_abis` fails the decode,
/// and the error names `callStack`.
#[test]
fn top_level_callstack_typo_is_rejected_by_name() {
    let error = decode_error(json!({
        "id": "typo-callstack",
        "event_abis": { "transfer": transfer_event() },
        "callStack": { "entry": { "address": PROXY } },
        "condition": true,
        "action": "deny"
    }));
    assert!(error.contains("unknown field"), "{error}");
    assert!(error.contains("callStack"), "the error must name the key: {error}");
}

/// The `callstack` section declares exactly one field, so the retired selector form, a
/// near-miss spelling and a speculative sibling are all rejected by name — during the decode,
/// which is earlier than any per-rule check and cannot be reached with the constraint
/// silently absent.
#[test]
fn an_undeclared_key_inside_callstack_is_rejected_by_name() {
    for (section, expected) in [
        // the retired selector form
        (json!({ "entry": { "address": PROXY, "codeHash": CODE_HASH } }), "entry"),
        // near-miss spellings of the one declared field
        (json!({ "direct_delegatecall": { "entry": PROXY } }), "direct_delegatecall"),
        (json!({ "directDelegatecalls": { "entry": PROXY } }), "directDelegatecalls"),
        (json!({ "Direct_DelegateCalls": { "entry": PROXY } }), "Direct_DelegateCalls"),
        // a speculative sibling alongside the correct field
        (
            json!({ "direct_delegatecalls": { "entry": PROXY }, "impl_allowlist": [IMPL] }),
            "impl_allowlist",
        ),
    ] {
        let error = decode_error(json!({
            "id": "undeclared-callstack-key",
            "event_abis": { "transfer": transfer_event() },
            "callstack": section,
            "condition": true,
            "action": "deny"
        }));
        assert!(error.contains("unknown field"), "{error}");
        assert!(error.contains(expected), "the error must name {expected}: {error}");
    }
}

/// any unrecognised safety key at rule level fails the decode, with the key named.
#[test]
fn unrecognised_rule_level_key_is_rejected() {
    let error = decode_error(json!({
        "id": "speculative-field",
        "event_abis": { "transfer": transfer_event() },
        "require_attestation": true,
        "condition": true,
        "action": "deny"
    }));
    assert!(error.contains("unknown field"), "{error}");
    assert!(error.contains("require_attestation"), "the error must name the key: {error}");
}

/// boundary preserved: a `callStack` typo on a rule declaring **no** `event_abis` is
/// still rejected, and the rejection now happens at the decode — which is earlier than the
/// "declares neither" compile-time boundary.
#[test]
fn callstack_typo_without_events_is_rejected_with_unknown_field_reason() {
    let error = decode_error(json!({
        "id": "typo-callstack-no-events",
        "callStack": { "entry": { "address": PROXY } },
        "condition": true,
        "action": "deny"
    }));
    assert!(error.contains("unknown field"), "{error}");
    assert!(error.contains("callStack"), "the error must name the key: {error}");
}

/// positive control: a fully correct `callstack` rule, in the producer's own shape, loads
/// unchanged and compiles to exactly one alias.
#[test]
fn a_correct_callstack_rule_loads() {
    let set = accepts(json!({
        "id": "correct-callstack",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": PROXY } },
        "condition": { "==": [ { "var": "entry.delegateCodeHash" }, CODE_HASH ] },
        "action": "deny"
    }));
    assert_eq!(set.rules.len(), 1);
    let alias = &set.rules[0].callstack[0];
    assert_eq!(alias.alias, "entry");
    assert_eq!(alias.address, PROXY.parse::<Address>().unwrap());
    assert_eq!(set.callstack_selectors(), [PROXY.parse::<Address>().unwrap()]);
}

/// positive control: a correct rule with no `callstack` loads unchanged.
#[test]
fn a_correct_event_only_rule_loads() {
    let set = accepts(json!({
        "id": "correct-event-only",
        "contract_address": null,
        "origin": null,
        "event_abis": { "transfer": transfer_event() },
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(set.rules.len(), 1);
    assert!(set.rules[0].callstack.is_empty());
}

/// positive control: the existing golden rule fixtures and the official RCS snapshot
/// still load with `rejected` empty.
#[test]
fn golden_fixtures_still_load_clean() {
    let raw: Vec<RawRule> = [golden::RULE_SCENARIO_A, golden::RULE_SCENARIO_B]
        .iter()
        .map(|text| serde_json::from_str(text).expect("golden fixture must deserialize"))
        .collect();
    let set = load_rules(1, 1, raw);
    assert!(set.rejected.is_empty(), "golden fixtures rejected: {:?}", set.rejected);
    assert_eq!(set.rules.len(), 2);

    let snapshot: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/rules-snapshot.json"
    )))
    .expect("official snapshot must deserialize");
    let raw: Vec<RawRule> = snapshot["rules"]
        .as_array()
        .expect("rules array")
        .iter()
        .map(|rule| serde_json::from_value(rule.clone()).expect("snapshot rule"))
        .collect();
    let set = load_rules(
        snapshot["protocol_version"].as_u64().unwrap() as u32,
        snapshot["content_version"].as_u64().unwrap(),
        raw,
    );
    assert!(set.rejected.is_empty(), "official snapshot rejected: {:?}", set.rejected);
}

/// control: a rule genuinely declaring neither `event_abis` nor `callstack`, with no
/// unknown keys, is still rejected with the **original** "declares neither" reason.
#[test]
fn declares_neither_keeps_original_reason() {
    let reason = rejection_reason(json!({
        "id": "dead-rule",
        "event_abis": {},
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(reason, "rule declares neither event_abis nor callstack");
}

/// control: a declared but malformed alias address, with no unknown keys anywhere, is still
/// rejected by the encoding gate's own reason.
#[test]
fn a_malformed_alias_address_keeps_its_own_reason() {
    let reason = rejection_reason(json!({
        "id": "malformed-alias-address",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": "0xAABB" } },
        "condition": { "==": [ { "var": "entry.delegateTo" }, IMPL ] },
        "action": "deny"
    }));
    assert_eq!(reason, "direct_delegatecalls alias 'entry' is not a lowercase 0x 20-byte address");
}

/// A typo'd rule fails its OWN decode, while valid siblings still decode; a whole document
/// containing it therefore fails to decode, which `load_and_install` turns into "snapshot
/// refused, last-known-good retained".
#[test]
fn one_typod_rule_fails_decode_while_valid_siblings_decode() {
    let valid: RawRule = serde_json::from_str(golden::RULE_SCENARIO_A).unwrap();

    let typod = json!({
        "id": "typo-sibling",
        "event_abis": { "transfer": transfer_event() },
        "callStack": {},
        "condition": true,
        "action": "deny"
    });
    let error = serde_json::from_value::<RawRule>(typod.clone())
        .expect_err("the typod sibling must fail its own decode");
    assert!(error.to_string().contains("callStack"), "{error}");

    // The valid sibling is unaffected.
    let set = load_rules(1, 1, vec![valid]);
    assert_eq!(set.rules.len(), 1);
    assert!(set.rejected.is_empty());

    // A document carrying both fails as a whole, before any per-rule validation runs.
    serde_json::from_value::<Vec<RawRule>>(json!([
        json!({
            "id": "valid-sibling",
            "event_abis": { "transfer": transfer_event() },
            "condition": true,
            "action": "deny"
        }),
        typod
    ]))
    .expect_err("a document containing a typod rule must fail the decode");
}

// ---------------------------------------------------------------- selector families ---

/// Fixture copied verbatim from `validation.rs`'s pre-existing `tests` module.
fn parse(json: &str) -> RawRule {
    serde_json::from_str(json).expect("valid rule json")
}

// Selector families: a rule must declare at least one event or call-frame selector.

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

// ---------------------------------------------------------------- config gate ---------

// Call-frame selector configuration: `callstack` needs no protocol-version gate, because it
// belongs to the one supported schema.

#[test]
fn callstack_needs_no_version_gate() {
    // `callstack` belongs to the one supported schema, so acceptance is decided by the rule
    // compiler alone and the only supported version is the minimum one.
    assert_eq!(MIN_PROTOCOL_VERSION, 1);
    assert!(is_supported_protocol(MIN_PROTOCOL_VERSION));
}
