//! Strict wire-schema tests: an unrecognized key anywhere in the rule wire format must reject
//! the rule, and therefore the whole snapshot update, with a reason that names the key.
//!
//! Before this behaviour existed serde discarded the key, every selector constraint field being
//! `Option<String>` deserialized to `None`, and the rule installed with the protection silently
//! removed.

use super::*;
use crate::test_support::golden;
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
    let set = load_rules(crate::config::MIN_PROTOCOL_VERSION, 1, vec![raw]);
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
    let set = load_rules(crate::config::MIN_PROTOCOL_VERSION, 1, vec![raw]);
    assert!(set.rejected.is_empty(), "unexpected rejection: {:?}", set.rejected);
    set
}

/// a top-level `callStack` typo on a rule that carries a valid `event_abis` (so it
/// survives the "declares neither" boundary) rejects the whole snapshot, naming `callStack`.
#[test]
fn top_level_callstack_typo_is_rejected_by_name() {
    let reason = rejection_reason(json!({
        "id": "typo-callstack",
        "event_abis": { "transfer": transfer_event() },
        "callStack": { "entry": { "address": PROXY } },
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(reason, "unknown field(s) in rule: callStack");
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

/// any unrecognised safety key at rule level is rejected with the key named.
#[test]
fn unrecognised_rule_level_key_is_rejected() {
    let reason = rejection_reason(json!({
        "id": "speculative-field",
        "event_abis": { "transfer": transfer_event() },
        "require_attestation": true,
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(reason, "unknown field(s) in rule: require_attestation");
}

/// an unknown key inside `event_abis.<name>` is rejected.
#[test]
fn unknown_key_inside_event_is_rejected() {
    let mut event = transfer_event();
    event["Anonymous"] = json!(true);
    let reason = rejection_reason(json!({
        "id": "typo-event",
        "event_abis": { "transfer": event },
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(reason, "unknown field(s) in event 'transfer': Anonymous");
}

/// an unknown key inside an ABI input is rejected, naming the input index.
#[test]
fn unknown_key_inside_abi_input_is_rejected() {
    let mut event = transfer_event();
    event["inputs"][1]["Indexed"] = json!(false);
    let reason = rejection_reason(json!({
        "id": "typo-abi-input",
        "event_abis": { "transfer": event },
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(reason, "unknown field(s) in event 'transfer' input #1: Indexed");
}

/// boundary preserved: a `callStack` typo on a rule declaring **no** `event_abis` is
/// still rejected. The "declares neither event_abis nor callstack selectors" boundary already
/// caught this input; the unknown-field
/// traversal runs first, so the **reason** now names `callStack` instead. The rejection does
/// not change; only the diagnostic improves.
#[test]
fn callstack_typo_without_events_is_rejected_with_unknown_field_reason() {
    let reason = rejection_reason(json!({
        "id": "typo-callstack-no-events",
        "callStack": { "entry": { "address": PROXY } },
        "condition": true,
        "action": "deny"
    }));
    assert_eq!(reason, "unknown field(s) in rule: callStack");
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

    let snapshot: Value = serde_json::from_str(include_str!("data/rules-snapshot.json"))
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

/// the key echo is bounded on both axes: at most 8 keys are named, each at most 64
/// bytes, and the remainder is summarised as `(+N more)`. A hostile snapshot must not be
/// able to inflate an operator log line.
#[test]
fn key_echo_is_bounded() {
    let mut rule = json!({
        "id": "many-unknown-keys",
        "event_abis": { "transfer": transfer_event() },
        "condition": true,
        "action": "deny"
    });
    // Sorted order is `k00..k49`, so the first 8 reported keys are deterministic. Each name
    // is far longer than the per-key byte bound.
    for index in 0..50 {
        let key = format!("k{index:02}{}", "x".repeat(200));
        rule[key] = json!(1);
    }
    let reason = rejection_reason(rule);

    assert!(reason.starts_with("unknown field(s) in rule: "), "{reason}");
    assert!(reason.ends_with(" (+42 more)"), "{reason}");
    let listed =
        reason.trim_start_matches("unknown field(s) in rule: ").trim_end_matches(" (+42 more)");
    let keys: Vec<&str> = listed.split(", ").collect();
    assert_eq!(keys.len(), MAX_UNKNOWN_KEYS_REPORTED);
    for key in &keys {
        assert!(key.len() <= MAX_UNKNOWN_KEY_BYTES, "key too long: {} bytes", key.len());
        assert!(key.ends_with("..."), "a truncated key must be marked: {key}");
    }
    assert!(keys[0].starts_with("k00"), "keys are reported in sorted order: {keys:?}");
    assert!(keys[7].starts_with("k07"), "keys are reported in sorted order: {keys:?}");
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

/// The traversal order is fixed (rule root → events → inputs), so an input carrying unknown
/// keys at several levels always reports the outermost one. Without that the reported reason
/// would depend on which unrelated check happened to run first.
#[test]
fn traversal_order_is_fixed_outermost_first() {
    let mut event = transfer_event();
    event["Anonymous"] = json!(true);
    let reason = rejection_reason(json!({
        "id": "unknown-at-every-level",
        "event_abis": { "transfer": event },
        "callstack": { "direct_delegatecalls": { "entry": PROXY } },
        "speculative": 1,
        "condition": { "==": [ { "var": "entry.delegateTo" }, IMPL ] },
        "action": "deny"
    }));
    assert_eq!(reason, "unknown field(s) in rule: speculative");
}

/// Unknown-field rejection is an ordinary per-rule rejection, so one typo'd rule among
/// valid siblings still lands in `rejected` and leaves the batch incomplete — which is what
/// `load_and_install` turns into "snapshot refused, last-known-good retained".
#[test]
fn one_typod_rule_among_valid_siblings_is_rejected_individually() {
    let valid: RawRule = serde_json::from_str(golden::RULE_SCENARIO_A).unwrap();
    let typod: RawRule = serde_json::from_value(json!({
        "id": "typo-sibling",
        "event_abis": { "transfer": transfer_event() },
        "callStack": {},
        "condition": true,
        "action": "deny"
    }))
    .unwrap();
    let set = load_rules(1, 1, vec![valid, typod]);
    assert_eq!(set.rules.len(), 1);
    assert_eq!(set.rejected.len(), 1);
    assert_eq!(set.rejected[0].id, "typo-sibling");
    assert_eq!(set.rejected[0].reason, "unknown field(s) in rule: callStack");
}

/// A key echoed at exactly the byte bound is reproduced whole; one byte over is truncated
/// and marked. Pins the boundary rather than only the obviously-too-long case.
#[test]
fn key_echo_boundary_is_exact() {
    let exact = "e".repeat(MAX_UNKNOWN_KEY_BYTES);
    assert_eq!(bound_key(&exact), exact);
    let over = "o".repeat(MAX_UNKNOWN_KEY_BYTES + 1);
    let bounded = bound_key(&over);
    assert_eq!(bounded.len(), MAX_UNKNOWN_KEY_BYTES);
    assert!(bounded.ends_with("..."));
}

/// Truncation never splits a multi-byte character.
#[test]
fn key_echo_truncation_respects_char_boundaries() {
    let key = "ä".repeat(100);
    let bounded = bound_key(&key);
    assert!(bounded.len() <= MAX_UNKNOWN_KEY_BYTES);
    assert!(bounded.ends_with("..."));
    assert!(bounded.trim_end_matches("...").chars().all(|c| c == 'ä'));
}
