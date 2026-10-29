//! Acceptance tests for the `callstack.direct_delegatecalls` rule schema and its strict parsing
//! boundary.
//!
//! Every negative here is driven through a **real entry point** rather than a visitor in
//! isolation: the hot-reload `GET /rules` decode and the shared per-rule phase the audit RPC
//! uses. A visitor-only test is explicitly insufficient, because a duplicate key can be folded
//! away by an intermediate representation *before* the visitor is reached, and a unit test on the
//! visitor would still pass while the real path silently accepted the rule.

use serde_json::json;

use crate::client::RulesResponse;
use crate::rules::{load_rules_from_raw, RuleBindingClass, RuleSet};

/// Lowercase 20-byte proxy addresses used across the matrix.
const PROXY1: &str = "0x1111111111111111111111111111111111111111";
const PROXY2: &str = "0x2222222222222222222222222222222222222222";

/// A minimal single-event ABI, as rule JSON text.
const EVENT_ABI: &str = r#"{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":false}],"anonymous":false}"#;

// ---------------------------------------------------------------------------------------------
// Entry point A — the hot-reload `GET /rules` decode, from exact bytes.
// ---------------------------------------------------------------------------------------------

/// Decodes a complete `GET /rules` body from **exact bytes** and runs the shared per-rule phase,
/// exactly as the hot-reload path does. Returns the envelope error, or the resulting snapshot.
fn via_hot_reload(body: &str) -> Result<RuleSet, String> {
    let response: RulesResponse = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(load_rules_from_raw(response.protocol_version, response.content_version, &response.rules))
}

/// Wraps rule texts into a `GET /rules` body without ever building a `serde_json::Value`, so a
/// duplicate key inside a rule reaches the decoder intact.
fn rules_body(rules: &[&str]) -> String {
    format!(r#"{{"protocol_version":1,"content_version":7,"rules":[{}]}}"#, rules.join(","))
}

// ---------------------------------------------------------------------------------------------
// Entry point B — the shared per-rule phase the audit RPC request uses.
// ---------------------------------------------------------------------------------------------

/// Runs the same per-rule phase the audit RPC runs over its request's preserved rule bytes.
fn via_audit_request(rules: &[&str]) -> RuleSet {
    let raw: Vec<Box<serde_json::value::RawValue>> = rules
        .iter()
        .map(|r| serde_json::from_str(r).expect("rule element is syntactically valid JSON"))
        .collect();
    load_rules_from_raw(1, 0, &raw)
}

/// Asserts one rule text is rejected at **both** entry points with a reason containing `needle`,
/// and that nothing was installed in either case.
#[track_caller]
fn rejected_at_both_entry_points(rule: &str, needle: &str) {
    for (entry, set) in [
        ("hot-reload", via_hot_reload(&rules_body(&[rule])).expect("envelope decodes")),
        ("audit-request", via_audit_request(&[rule])),
    ] {
        assert!(
            set.rules.is_empty(),
            "{entry}: rule must not be installed, got {} rule(s)",
            set.rules.len()
        );
        assert_eq!(set.rejected.len(), 1, "{entry}: expected exactly one rejection");
        assert!(
            set.rejected[0].reason.contains(needle),
            "{entry}: reason {:?} does not contain {needle:?}",
            set.rejected[0].reason
        );
    }
}

/// Asserts one rule text is accepted at both entry points, returning the hot-reload snapshot.
#[track_caller]
fn accepted_at_both_entry_points(rule: &str) -> RuleSet {
    let audit = via_audit_request(&[rule]);
    assert!(audit.rejected.is_empty(), "audit-request: unexpected rejection {:?}", audit.rejected);
    let hot = via_hot_reload(&rules_body(&[rule])).expect("envelope decodes");
    assert!(hot.rejected.is_empty(), "hot-reload: unexpected rejection {:?}", hot.rejected);
    assert_eq!(hot.rules.len(), 1);
    hot
}

fn direct_only_rule(alias: &str, address: &str) -> String {
    format!(
        r#"{{"id":"direct-only","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"{alias}":"{address}"}}}},"condition":true,"action":"deny"}}"#
    )
}

// =============================================================================================
// a rule declaring neither section (the schema check), at both entry points.
// =============================================================================================

#[test]
fn rule_declaring_neither_section_is_rejected_at_both_entry_points() {
    rejected_at_both_entry_points(
        r#"{"id":"neither","event_abis":{},"condition":true,"action":"deny"}"#,
        "rule declares neither event_abis nor callstack",
    );
    // And the same holds when `event_abis` is omitted entirely rather than being empty.
    rejected_at_both_entry_points(
        r#"{"id":"neither","condition":true,"action":"deny"}"#,
        "rule declares neither event_abis nor callstack",
    );
}

// =============================================================================================
// checksummed / uppercase alias address (the schema check).
// =============================================================================================

#[test]
fn uppercase_or_checksummed_alias_address_is_rejected_at_both_entry_points() {
    // Note: an all-`1` address has no cased hex letters, so it cannot exercise this rule at all.
    // Every case below contains at least one letter whose case is observable.
    for address in [
        "0xABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD".to_string(),
        "0xAbCdefabcdefabcdefabcdefabcdefabcdefabcd".to_string(),
        // Right length and hex, exactly one uppercase digit: a checksum form, not a
        // normalisation.
        "0x111111111111111111111111111111111111111A".to_string(),
    ] {
        rejected_at_both_entry_points(
            &direct_only_rule("proxy1", &address),
            "is not a lowercase 0x 20-byte address",
        );
    }
}

#[test]
fn wrong_length_or_missing_prefix_alias_address_is_rejected() {
    for address in [
        "1111111111111111111111111111111111111111",     // no 0x
        "0x11111111111111111111111111111111111111",     // 19 bytes
        "0x111111111111111111111111111111111111111111", // 21 bytes
        "0x",
        "",
    ] {
        rejected_at_both_entry_points(
            &direct_only_rule("proxy1", address),
            "is not a lowercase 0x 20-byte address",
        );
    }
}

// =============================================================================================
// a 9th alias (the schema check), and an alias colliding with an event alias (the schema check).
// =============================================================================================

#[test]
fn ninth_alias_is_rejected_and_eight_are_accepted() {
    let entry = |n: usize| format!(r#""p{n}":"0x{:040x}""#, n + 1);

    let eight: Vec<String> = (0..8).map(entry).collect();
    let rule_eight = format!(
        r#"{{"id":"eight","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{{}}}}},"condition":true,"action":"deny"}}"#,
        eight.join(",")
    );
    let set = accepted_at_both_entry_points(&rule_eight);
    assert_eq!(set.rules[0].direct_delegatecalls.len(), 8);

    let nine: Vec<String> = (0..9).map(entry).collect();
    let rule_nine = format!(
        r#"{{"id":"nine","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{{}}}}},"condition":true,"action":"deny"}}"#,
        nine.join(",")
    );
    rejected_at_both_entry_points(&rule_nine, "at most 8 are allowed");
}

#[test]
fn alias_colliding_with_an_event_alias_is_rejected() {
    let rule = format!(
        r#"{{"id":"collide","event_abis":{{"shared":{EVENT_ABI}}},"callstack":{{"direct_delegatecalls":{{"shared":"{PROXY1}"}}}},"condition":true,"action":"deny"}}"#
    );
    rejected_at_both_entry_points(&rule, "collides with an event alias");
}

// =============================================================================================
// valid `event_abis` PLUS a misspelled `callStack` (the schema check).
//
// This is the defect the root-level gate exists for: without it the misspelled key is silently
// dropped and the rule loads as a WEAKENED event-only rule.
// =============================================================================================

#[test]
fn misspelled_callstack_sibling_rejects_the_rule_instead_of_weakening_it() {
    let rule = format!(
        r#"{{"id":"typo","event_abis":{{"e":{EVENT_ABI}}},"callStack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"condition":true,"action":"deny"}}"#
    );
    rejected_at_both_entry_points(&rule, "callStack");
    // And the reason names it as an unknown field rather than as some unrelated failure.
    let set = via_audit_request(&[&rule]);
    assert!(
        set.rejected[0].reason.starts_with("rule declares unknown field(s)"),
        "unexpected reason: {:?}",
        set.rejected[0].reason
    );
}

#[test]
fn other_callstack_spellings_are_rejected_too() {
    for key in ["directDelegatecalls", "callstacks", "CallStack", "call_stack"] {
        let rule = format!(
            r#"{{"id":"typo","event_abis":{{"e":{EVENT_ABI}}},"{key}":{{}},"condition":true,"action":"deny"}}"#
        );
        rejected_at_both_entry_points(&rule, key);
    }
}

#[test]
fn the_legacy_tolerated_list_is_empty_so_every_unmodelled_root_key_rejects() {
    assert!(
        crate::rules::RULE_LEGACY_TOLERATED_KEYS.is_empty(),
        "widening this list requires per-key evidence and a fresh design review"
    );
    let rule = format!(
        r#"{{"id":"extra","event_abis":{{"e":{EVENT_ABI}}},"some_future_field":1,"condition":true,"action":"deny"}}"#
    );
    rejected_at_both_entry_points(&rule, "some_future_field");
}

// =============================================================================================
// duplicate alias key (the duplicate-alias check), driven through the WHOLE entry point.
// =============================================================================================

/// A rule whose `direct_delegatecalls` repeats `proxy1`. Built as text, because a
/// `serde_json::Value` would already have folded the repeat last-wins.
fn duplicate_alias_rule(first: &str, second: &str) -> String {
    format!(
        r#"{{"id":"dup","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{first}","proxy1":"{second}"}}}},"condition":true,"action":"deny"}}"#
    )
}

#[test]
fn duplicate_alias_key_is_rejected_through_the_whole_entry_point() {
    rejected_at_both_entry_points(
        &duplicate_alias_rule(PROXY1, PROXY2),
        "declares alias 'proxy1' more than once",
    );
}

#[test]
fn duplicate_alias_with_the_same_value_is_still_a_duplicate() {
    // Identical values are NOT "harmlessly redundant": accepting them would mean the rule the
    // producer wrote is not the rule this node enforces.
    rejected_at_both_entry_points(
        &duplicate_alias_rule(PROXY1, PROXY1),
        "declares alias 'proxy1' more than once",
    );
}

#[test]
fn duplicate_alias_is_never_silently_accepted_with_the_last_value() {
    let rule = duplicate_alias_rule(PROXY1, PROXY2);
    let set = via_audit_request(&[&rule]);
    assert!(set.rules.is_empty(), "the rule must not load at all");
    // Specifically: it did not load bound to PROXY2.
    assert!(
        !set.rules.iter().any(|r| r.direct_delegatecalls.iter().any(|dd| dd
            .address
            .to_string()
            .to_lowercase()
            .contains("2222"))),
        "last-wins acceptance detected"
    );
}

#[test]
fn duplicate_alias_outcome_does_not_depend_on_value_order() {
    let forward = via_audit_request(&[&duplicate_alias_rule(PROXY1, PROXY2)]);
    let reversed = via_audit_request(&[&duplicate_alias_rule(PROXY2, PROXY1)]);
    assert_eq!(forward.rejected.len(), 1);
    assert_eq!(reversed.rejected.len(), 1);
    assert_eq!(
        forward.rejected[0].reason, reversed.rejected[0].reason,
        "the reason must not depend on which value appears last"
    );
}

#[test]
fn duplicate_alias_plus_an_unknown_root_key_reports_v0_first() {
    // The raw-key check precedes the typed decode, so the schema check is the reported reason — and the rule
    // is still rejected, so the duplicate does not vanish from the record.
    let rule = format!(
        r#"{{"id":"both","surprise":1,"event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}","proxy1":"{PROXY2}"}}}},"condition":true,"action":"deny"}}"#
    );
    let set = via_audit_request(&[&rule]);
    assert!(set.rules.is_empty());
    assert_eq!(set.rejected.len(), 1);
    assert!(
        set.rejected[0].reason.contains("surprise"),
        "the schema check must be reported first: {:?}",
        set.rejected[0].reason
    );
}

#[test]
fn duplicate_alias_plus_an_uppercase_address_reports_v10_first() {
    // Both defects are on the same rule; the typed decode (the duplicate-alias check) precedes semantic validation.
    let upper = "0x111111111111111111111111111111111111111A";
    let rule = format!(
        r#"{{"id":"both","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{upper}","proxy1":"{upper}"}}}},"condition":true,"action":"deny"}}"#
    );
    let set = via_audit_request(&[&rule]);
    assert_eq!(set.rejected.len(), 1);
    assert!(
        set.rejected[0].reason.contains("more than once"),
        "the duplicate-alias check must be reported first: {:?}",
        set.rejected[0].reason
    );
}

// =============================================================================================
// empty alias name (the schema check), and a non-ASCII alias being ACCEPTED.
// =============================================================================================

#[test]
fn empty_alias_name_is_rejected_at_both_entry_points() {
    rejected_at_both_entry_points(
        &direct_only_rule("", PROXY1),
        "direct_delegatecalls declares an empty alias name",
    );
}

#[test]
fn non_ascii_alias_is_accepted_and_binds_normally() {
    // No necessity or compatibility evidence supports a narrower alias grammar, so a non-empty
    // non-ASCII alias must load — and must never be reported with the empty-alias message.
    let alias = "代理1";
    let rule = format!(
        r#"{{"id":"unicode","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"{alias}":"{PROXY1}"}}}},"condition":{{"!":{{"in":[{{"var":"{alias}.delegateCodeHash"}},["0xaa"]]}}}},"action":"deny"}}"#
    );
    let set = accepted_at_both_entry_points(&rule);
    assert_eq!(set.rules[0].direct_delegatecalls.len(), 1);
    assert_eq!(set.rules[0].direct_delegatecalls[0].alias, alias);
}

// =============================================================================================
// `"callstack": null` (the schema check).
// =============================================================================================

#[test]
fn explicitly_null_callstack_is_rejected_rather_than_treated_as_absent() {
    let rule = format!(
        r#"{{"id":"nulled","event_abis":{{"e":{EVENT_ABI}}},"callstack":null,"condition":true,"action":"deny"}}"#
    );
    rejected_at_both_entry_points(&rule, "callstack is explicitly null; omit the field instead");
}

#[test]
fn an_absent_callstack_on_an_event_rule_still_loads_unchanged() {
    // The present-vs-null distinction must not make an ordinary event-only rule fail.
    let rule = format!(
        r#"{{"id":"event-only","event_abis":{{"e":{EVENT_ABI}}},"condition":true,"action":"deny"}}"#
    );
    let set = accepted_at_both_entry_points(&rule);
    assert!(set.rules[0].direct_delegatecalls.is_empty());
    assert_eq!(set.rules[0].binding_class(), RuleBindingClass::EventOnly);
    assert!(!set.declares_any_alias());
}

// =============================================================================================
// wrong JSON types (the schema check), and unknown keys inside `callstack` (the schema check).
// =============================================================================================

#[test]
fn wrong_json_types_are_rejected_naming_the_field() {
    let cases = [
        // `callstack` is not an object. The sequence forms matter most: serde's *derived*
        // struct decoder also accepts a JSON array positionally, so a one-element array would
        // bind `direct_delegatecalls` from element 0 unless the object form is required
        // explicitly. `deny_unknown_fields` does not help — it only constrains map keys.
        (r#""callstack":[{"proxy1":"0x1111111111111111111111111111111111111111"}]"#, "callstack"),
        (r#""callstack":[{}]"#, "callstack"),
        (
            r#""callstack":[{"direct_delegatecalls":{"proxy1":"0x1111111111111111111111111111111111111111"}}]"#,
            "callstack",
        ),
        (r#""callstack":[]"#, "callstack"),
        (r#""callstack":7"#, "callstack"),
        (r#""callstack":-1"#, "callstack"),
        (r#""callstack":1.5"#, "callstack"),
        (r#""callstack":true"#, "callstack"),
        (r#""callstack":"text""#, "callstack"),
        (r#""callstack":"""#, "callstack"),
        // `direct_delegatecalls` is not a map
        (r#""callstack":{"direct_delegatecalls":[]}"#, "direct_delegatecalls"),
        (r#""callstack":{"direct_delegatecalls":"text"}"#, "direct_delegatecalls"),
        // an alias value is not a string
        (r#""callstack":{"direct_delegatecalls":{"proxy1":7}}"#, "string"),
        (r#""callstack":{"direct_delegatecalls":{"proxy1":null}}"#, "string"),
        (r#""callstack":{"direct_delegatecalls":{"proxy1":["0x11"]}}"#, "string"),
    ];
    for (fragment, needle) in cases {
        let rule = format!(
            r#"{{"id":"typed","event_abis":{{"e":{EVENT_ABI}}},{fragment},"condition":true,"action":"deny"}}"#
        );
        rejected_at_both_entry_points(&rule, needle);
    }
}

/// The `callstack` **object** form is mandatory, at both entry points.
///
/// This is the regression guard for a real defect: `RawCallstack` was a derived `Deserialize`, and
/// a derived struct decoder also accepts the positional *sequence* form, so a one-element array
/// bound `direct_delegatecalls` from element 0 and the rule LOADED. `deny_unknown_fields` could
/// not catch it, because it constrains map keys only. Revert either layer of the fix and the first
/// case below installs a rule again.
#[test]
fn callstack_in_sequence_form_is_rejected_at_both_entry_points() {
    let sequence_forms = [
        // The exact shape that used to load: element 0 is read as `direct_delegatecalls`.
        format!(r#"[{{"proxy1":"{PROXY1}"}}]"#),
        // The same, named — a producer "fixing" the array by nesting the field.
        format!(r#"[{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}}]"#),
        // Two elements, an empty object, and an empty array.
        format!(r#"[{{"proxy1":"{PROXY1}"}},{{"proxy2":"{PROXY2}"}}]"#),
        "[{}]".to_string(),
        "[]".to_string(),
    ];
    for form in sequence_forms {
        let rule = format!(
            r#"{{"id":"seq","event_abis":{{"e":{EVENT_ABI}}},"callstack":{form},"condition":true,"action":"deny"}}"#
        );
        rejected_at_both_entry_points(&rule, "callstack must be a JSON object, got an array");
    }
}

/// Every non-object, non-null JSON kind for `callstack` is rejected by kind, naming the field.
#[test]
fn a_scalar_callstack_is_rejected_at_both_entry_points() {
    for (literal, kind) in [
        ("7", "a number"),
        ("-1", "a number"),
        ("1.5", "a number"),
        (r#""text""#, "a string"),
        (r#""""#, "a string"),
        ("true", "a boolean"),
        ("false", "a boolean"),
    ] {
        let rule = format!(
            r#"{{"id":"scalar","event_abis":{{"e":{EVENT_ABI}}},"callstack":{literal},"condition":true,"action":"deny"}}"#
        );
        rejected_at_both_entry_points(
            &rule,
            &format!("callstack must be a JSON object, got {kind}"),
        );
    }
}

/// `direct_delegatecalls` must be a map, and every alias value must be a string — both reported by
/// field name, at both entry points.
#[test]
fn direct_delegatecalls_must_be_a_map_of_strings_at_both_entry_points() {
    let not_a_map = [
        (r#"[]"#, "an array"),
        (format!(r#"[["proxy1","{PROXY1}"]]"#).as_str(), "an array"),
        (r#""text""#, "a string"),
        (r#"7"#, "a number"),
        (r#"true"#, "a boolean"),
        (r#"null"#, "null"),
    ]
    .map(|(literal, kind)| (literal.to_string(), kind));
    for (literal, kind) in not_a_map {
        let rule = format!(
            r#"{{"id":"notmap","event_abis":{{"e":{EVENT_ABI}}},"callstack":{{"direct_delegatecalls":{literal}}},"condition":true,"action":"deny"}}"#
        );
        rejected_at_both_entry_points(
            &rule,
            &format!("direct_delegatecalls must be a JSON object, got {kind}"),
        );
    }

    for (literal, kind) in [
        ("7", "a number"),
        ("1.5", "a number"),
        ("null", "null"),
        ("true", "a boolean"),
        (r#"["0x11"]"#, "an array"),
        (r#"{"address":"0x11"}"#, "an object"),
    ] {
        let rule = format!(
            r#"{{"id":"notstr","event_abis":{{"e":{EVENT_ABI}}},"callstack":{{"direct_delegatecalls":{{"proxy1":{literal}}}}},"condition":true,"action":"deny"}}"#
        );
        rejected_at_both_entry_points(
            &rule,
            &format!("direct_delegatecalls alias 'proxy1' must be a string, got {kind}"),
        );
    }
}

/// Defence in depth: the **typed decode on its own** also refuses the sequence form.
///
/// The raw-first shape check and this are independent layers, so each is tested independently —
/// otherwise removing one would leave the suite green on the strength of the other.
#[test]
fn the_typed_decode_alone_refuses_a_non_object_callstack() {
    use crate::rules::RawRule;

    for form in [
        format!(r#"[{{"proxy1":"{PROXY1}"}}]"#),
        format!(r#"[{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}}]"#),
        "[]".to_string(),
        "7".to_string(),
        r#""text""#.to_string(),
        "true".to_string(),
    ] {
        let rule = format!(
            r#"{{"id":"typed-only","event_abis":{{"e":{EVENT_ABI}}},"callstack":{form},"condition":true,"action":"deny"}}"#
        );
        let error = serde_json::from_str::<RawRule>(&rule)
            .expect_err("the typed decode must refuse a non-object callstack on its own")
            .to_string();
        assert!(error.contains("callstack"), "reason must name the field: {error}");
    }

    // And a non-map `direct_delegatecalls` / non-string alias value, likewise.
    for fragment in [
        r#"{"direct_delegatecalls":[]}"#.to_string(),
        r#"{"direct_delegatecalls":"text"}"#.to_string(),
        r#"{"direct_delegatecalls":{"proxy1":7}}"#.to_string(),
        r#"{"direct_delegatecalls":{"proxy1":null}}"#.to_string(),
    ] {
        let rule = format!(
            r#"{{"id":"typed-only","event_abis":{{"e":{EVENT_ABI}}},"callstack":{fragment},"condition":true,"action":"deny"}}"#
        );
        assert!(
            serde_json::from_str::<RawRule>(&rule).is_err(),
            "the typed decode must refuse {fragment}"
        );
    }

    // The object form with a well-formed section still decodes, so the guards above are not
    // passing by rejecting everything.
    let good = format!(
        r#"{{"id":"typed-only","event_abis":{{"e":{EVENT_ABI}}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"condition":true,"action":"deny"}}"#
    );
    let decoded = serde_json::from_str::<RawRule>(&good).expect("the object form decodes");
    let section = decoded.callstack.expect("present").expect("not null");
    assert_eq!(section.direct_delegatecalls.get("proxy1").map(String::as_str), Some(PROXY1));
}

#[test]
fn an_unknown_key_inside_callstack_rejects_the_rule() {
    for key in ["direct_delegatecall", "directDelegatecalls", "selectors", "impl_address"] {
        let rule = format!(
            r#"{{"id":"inner","event_abis":{{"e":{EVENT_ABI}}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}},"{key}":1}},"condition":true,"action":"deny"}}"#
        );
        rejected_at_both_entry_points(&rule, key);
    }
}

#[test]
fn an_empty_direct_delegatecalls_map_is_rejected() {
    let rule = format!(
        r#"{{"id":"empty","event_abis":{{"e":{EVENT_ABI}}},"callstack":{{"direct_delegatecalls":{{}}}},"condition":true,"action":"deny"}}"#
    );
    rejected_at_both_entry_points(&rule, "direct_delegatecalls is empty");
}

// =============================================================================================
// a direct-delegatecall-only `audit` rule is a load failure (the schema check).
// =============================================================================================

#[test]
fn direct_delegatecall_only_audit_rule_is_a_load_failure() {
    let rule = format!(
        r#"{{"id":"audit-only","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"audit_types":["quota"],"condition":true,"action":"audit"}}"#
    );
    rejected_at_both_entry_points(&rule, "direct-delegatecall-only rule cannot be an audit rule");
}

#[test]
fn a_mixed_audit_rule_is_still_permitted() {
    // the schema check scopes to the direct-only case: a mixed rule keeps an event side to take its payload
    // from, so it remains a legal audit rule.
    let transfer = r#"{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":false}"#;
    let rule = format!(
        r#"{{"id":"mixed-audit","event_abis":{{"transfer":{transfer}}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"audit_types":["quota"],"condition":true,"action":"audit"}}"#
    );
    let set = accepted_at_both_entry_points(&rule);
    assert_eq!(set.rules[0].binding_class(), RuleBindingClass::Mixed);
}

// =============================================================================================
// Phase A ordering: envelope problems precede any per-rule problem.
// =============================================================================================

/// The malformed rule used by the ordering tests: its `callstack` has the wrong JSON type, so it
/// would fail the typed decode if Phase B were ever reached.
fn malformed_rule() -> String {
    format!(
        r#"{{"id":"malformed","event_abis":{{"e":{EVENT_ABI}}},"callstack":7,"condition":true,"action":"deny"}}"#
    )
}

#[test]
fn unknown_top_level_key_is_cited_ahead_of_version_and_rule_problems() {
    let body = format!(
        r#"{{"surprise":1,"protocol_version":2,"content_version":7,"rules":[{}]}}"#,
        malformed_rule()
    );
    let error = via_hot_reload(&body).expect_err("the envelope itself must be rejected");
    assert!(error.contains("surprise"), "must cite the unknown top-level key: {error}");
    assert!(!error.contains("callstack"), "must not surface the nested rule error first: {error}");
}

#[test]
fn unsupported_version_is_cited_when_there_is_no_unknown_top_level_key() {
    // The envelope decodes (the version field is typed), so the version check is the handler's,
    // and it precedes any per-rule problem.
    let body =
        format!(r#"{{"protocol_version":2,"content_version":7,"rules":[{}]}}"#, malformed_rule());
    let set = via_hot_reload(&body).expect("the envelope shape is valid");
    assert_eq!(set.protocol_version, 2);
    assert!(
        !crate::config::is_supported_protocol(set.protocol_version),
        "version 2 must not be supported"
    );
    let reason = crate::rules::raw::unsupported_protocol_reason(set.protocol_version);
    assert!(reason.contains('2') && reason.contains('1'), "{reason}");
}

#[test]
fn a_duplicate_top_level_envelope_key_is_named() {
    let body = r#"{"protocol_version":1,"protocol_version":1,"content_version":7,"rules":[]}"#;
    let error = via_hot_reload(body).expect_err("a repeated envelope key must be rejected");
    assert!(error.contains("protocol_version"), "{error}");
    assert!(error.contains("more than once"), "{error}");
}

// =============================================================================================
// full aggregation across several differently-failing rules.
// =============================================================================================

#[test]
fn three_differently_failing_rules_are_all_reported_with_their_reasons() {
    let v0 = format!(
        r#"{{"id":"r-v0","event_abis":{{"e":{EVENT_ABI}}},"callStack":{{}},"condition":true,"action":"deny"}}"#
    );
    let v10 = format!(
        r#"{{"id":"r-v10","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}","proxy1":"{PROXY2}"}}}},"condition":true,"action":"deny"}}"#
    );
    let v4 = r#"{"id":"r-v4","event_abis":{},"callstack":{"direct_delegatecalls":{"proxy1":"0x111111111111111111111111111111111111111A"}},"condition":true,"action":"deny"}"#
        .to_string();

    for set in [
        via_audit_request(&[&v0, &v10, &v4]),
        via_hot_reload(&rules_body(&[&v0, &v10, &v4])).expect("envelope decodes"),
    ] {
        assert!(set.rules.is_empty(), "nothing may be installed");
        assert_eq!(set.rejected.len(), 3, "all three must be reported: {:?}", set.rejected);

        let by_id: std::collections::BTreeMap<&str, &str> =
            set.rejected.iter().map(|r| (r.id.as_str(), r.reason.as_str())).collect();
        assert!(by_id["r-v0"].contains("callStack"), "{:?}", by_id["r-v0"]);
        assert!(by_id["r-v10"].contains("more than once"), "{:?}", by_id["r-v10"]);
        assert!(
            by_id["r-v4"].contains("is not a lowercase 0x 20-byte address"),
            "{:?}",
            by_id["r-v4"]
        );
    }
}

#[test]
fn one_failing_rule_does_not_hide_a_valid_one_or_get_installed() {
    let bad = duplicate_alias_rule(PROXY1, PROXY2);
    let good = direct_only_rule("proxy2", PROXY2);
    let set = via_audit_request(&[&bad, &good]);
    // The valid rule compiled, and the invalid one is still reported — which is what makes the
    // all-or-nothing decision at the caller a real decision rather than a silent subset.
    assert_eq!(set.rules.len(), 1);
    assert_eq!(set.rules[0].id, "direct-only");
    assert_eq!(set.rejected.len(), 1);
}

// =============================================================================================
// verdicts are invariant under key and element reordering.
// =============================================================================================

#[test]
fn reordering_top_level_keys_does_not_change_the_envelope_verdict() {
    let rule = malformed_rule();
    let a =
        format!(r#"{{"surprise":1,"protocol_version":2,"content_version":7,"rules":[{rule}]}}"#);
    let b =
        format!(r#"{{"rules":[{rule}],"content_version":7,"protocol_version":2,"surprise":1}}"#);
    let ea = via_hot_reload(&a).expect_err("rejected");
    let eb = via_hot_reload(&b).expect_err("rejected");
    assert_eq!(ea, eb, "the verdict must not be a function of input key order");
}

#[test]
fn reordering_rules_keeps_the_same_reasons_keyed_by_id() {
    let v0 = format!(
        r#"{{"id":"r-v0","event_abis":{{"e":{EVENT_ABI}}},"callStack":{{}},"condition":true,"action":"deny"}}"#
    );
    let v10 = duplicate_alias_rule(PROXY1, PROXY2);
    let v4 = r#"{"id":"r-v4","event_abis":{},"callstack":{"direct_delegatecalls":{"proxy1":"0x111111111111111111111111111111111111111A"}},"condition":true,"action":"deny"}"#
        .to_string();

    let reasons = |set: &RuleSet| -> std::collections::BTreeMap<String, String> {
        set.rejected.iter().map(|r| (r.id.clone(), r.reason.clone())).collect()
    };
    // Every rule here carries an id, so the comparison is the SET OF REASONS KEYED BY ID.
    let forward = via_audit_request(&[&v0, &v10, &v4]);
    let shuffled = via_audit_request(&[&v4, &v0, &v10]);
    assert_eq!(reasons(&forward), reasons(&shuffled));
}

#[test]
fn an_unidentified_rules_index_tracks_its_new_input_position() {
    // A rule without a usable id is located by index, so under reordering the reported index must
    // follow the NEW position. Asserting the index stayed the same would be wrong.
    let anonymous = r#"{"event_abis":{},"condition":true,"action":"deny"}"#;
    let named = direct_only_rule("proxy1", PROXY1);

    let first = via_audit_request(&[anonymous, &named]);
    let anon_first = first.rejected.iter().find(|r| r.id.contains("no id")).expect("reported");
    assert!(anon_first.id.contains("index 0"), "{:?}", anon_first.id);

    let second = via_audit_request(&[&named, anonymous]);
    let anon_second = second.rejected.iter().find(|r| r.id.contains("no id")).expect("reported");
    assert!(anon_second.id.contains("index 1"), "{:?}", anon_second.id);
}

// =============================================================================================
// a missing / duplicated / non-string `id` is located by an explicit index.
// =============================================================================================

#[test]
fn a_rule_without_a_usable_id_is_located_by_an_explicit_index_never_a_fabricated_id() {
    let cases = [
        // missing id
        r#"{"event_abis":{},"callstack":{"direct_delegatecalls":{"proxy1":"0x111111111111111111111111111111111111111A"}},"condition":true,"action":"deny"}"#.to_string(),
        // repeated id — neither occurrence may be picked
        r#"{"id":"a","id":"b","event_abis":{},"condition":true,"action":"deny"}"#.to_string(),
        // non-string id
        r#"{"id":7,"event_abis":{},"condition":true,"action":"deny"}"#.to_string(),
    ];

    for (case_index, rule) in cases.iter().enumerate() {
        // Place the rule at index 1 so a correct implementation cannot pass by reporting 0.
        let filler = direct_only_rule("filler", PROXY2);
        let set = via_audit_request(&[&filler, rule]);
        let reported = set.rejected.iter().find(|r| r.id.contains("no id")).unwrap_or_else(|| {
            panic!("case {case_index}: expected an index-located rejection, got {:?}", set.rejected)
        });
        assert!(reported.id.contains("index 1"), "case {case_index}: {:?}", reported.id);
        // The identity guarantee is what this test asserts — NOT which validation rule fired.
        // A missing or non-string `id` may legitimately surface as a typed-decode error first.
        assert!(!reported.reason.is_empty());
    }
}

#[test]
fn a_rule_with_a_usable_id_is_reported_by_that_id() {
    let set = via_audit_request(&[&duplicate_alias_rule(PROXY1, PROXY2)]);
    assert_eq!(set.rejected[0].id, "dup");
}

// =============================================================================================
// a rule failing both the schema check and the duplicate-alias check reports the schema check.
// =============================================================================================

#[test]
fn v0_precedes_v10_on_the_same_rule() {
    let rule = format!(
        r#"{{"id":"both","unknown_root":1,"event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}","proxy1":"{PROXY2}"}}}},"condition":true,"action":"deny"}}"#
    );
    for set in [
        via_audit_request(&[&rule]),
        via_hot_reload(&rules_body(&[&rule])).expect("envelope decodes"),
    ] {
        assert_eq!(set.rejected.len(), 1);
        assert!(set.rejected[0].reason.contains("unknown_root"), "{:?}", set.rejected[0].reason);
    }
}

// =============================================================================================
// the schema check — a duplicate rule-ROOT key is the same fidelity defect one level up.
// =============================================================================================

#[test]
fn duplicate_rule_root_key_is_rejected_rather_than_folded_last_wins() {
    // Without this check the second `event_abis` silently wins and could hide the first.
    let rule = format!(
        r#"{{"id":"dupkey","event_abis":{{"e":{EVENT_ABI}}},"event_abis":{{}},"condition":true,"action":"deny"}}"#
    );
    rejected_at_both_entry_points(&rule, "declares root key 'event_abis' more than once");
}

#[test]
fn duplicate_callstack_root_key_is_rejected() {
    let rule = format!(
        r#"{{"id":"dupcs","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"callstack":null,"condition":true,"action":"deny"}}"#
    );
    rejected_at_both_entry_points(&rule, "declares root key 'callstack' more than once");
}

// =============================================================================================
// a rule element that is not a JSON object is rejected in the scan, by index.
// =============================================================================================

#[test]
fn a_non_object_rule_element_is_rejected_explicitly_by_index() {
    for element in ["7", r#""text""#, "[]", "null", "true"] {
        let body = format!(
            r#"{{"protocol_version":1,"content_version":7,"rules":[{},{element}]}}"#,
            direct_only_rule("proxy1", PROXY1)
        );
        let set = via_hot_reload(&body).expect("the envelope itself is well-formed");
        assert_eq!(set.rejected.len(), 1, "element {element} must be rejected");
        assert!(set.rejected[0].id.contains("index 1"), "{:?}", set.rejected[0].id);
        assert!(
            set.rejected[0].reason.contains("not a readable JSON object"),
            "{:?}",
            set.rejected[0].reason
        );
        // Nothing from the batch is installed by the caller, because `rejected` is non-empty.
        assert!(!set.rejected.is_empty());
    }
}

// =============================================================================================
// byte-fidelity guard. This is the regression guard for the defect where Phase A used
// an intermediate representation that folded duplicate keys away.
// =============================================================================================

#[test]
fn the_rule_path_never_materialises_a_value_or_map_for_a_rule() {
    // (a) The envelope type keeps rules as raw bytes. If this field's type ever becomes
    //     `Vec<Value>` / `Vec<RawRule>` / a map, this assertion stops compiling or stops holding.
    // Type-level guard: this helper only accepts a slice of RAW values, so it stops compiling if
    // `RulesResponse::rules` is ever changed to `Vec<Value>`, `Vec<RawRule>`, or a map.
    fn assert_rules_are_raw(rules: &[Box<serde_json::value::RawValue>]) -> usize {
        rules.len()
    }

    let body = rules_body(&[&duplicate_alias_rule(PROXY1, PROXY2)]);
    let response: RulesResponse = serde_json::from_str(&body).expect("decodes");
    assert_eq!(assert_rules_are_raw(&response.rules), 1);
    let src = response.rules[0].get();
    assert_eq!(src.matches(r#""proxy1""#).count(), 2, "both alias entries must still be present");

    // (b) Proof that the intermediate representation really does destroy the evidence, so this
    //     guard is testing something real rather than restating the implementation.
    let folded: serde_json::Value = serde_json::from_str(src).expect("also parses as a Value");
    let aliases = folded["callstack"]["direct_delegatecalls"].as_object().expect("object");
    assert_eq!(aliases.len(), 1, "a Value folds the duplicate away — never use it on this path");

    // (c) And the real path still rejects the rule, from the preserved bytes.
    let set = load_rules_from_raw(1, 7, &response.rules);
    assert!(set.rules.is_empty());
    assert!(set.rejected[0].reason.contains("more than once"));
}

#[test]
fn a_value_round_trip_is_rejected_as_a_decode_path() {
    // Converting to `Value` and back to raw for lifetime convenience is exactly the fidelity loss
    // the hard prohibition forbids. This pins that it would be detectable: the round-tripped
    // bytes no longer carry the duplicate, so the rule would wrongly LOAD.
    let text = duplicate_alias_rule(PROXY1, PROXY2);
    let round_tripped =
        serde_json::to_string(&serde_json::from_str::<serde_json::Value>(&text).expect("parses"))
            .expect("re-serializes");
    let raw: Vec<Box<serde_json::value::RawValue>> =
        vec![serde_json::from_str(&round_tripped).expect("raw")];
    let set = load_rules_from_raw(1, 0, &raw);
    assert_eq!(
        set.rules.len(),
        1,
        "a Value round trip silently makes the duplicate rule VALID — hence the prohibition"
    );

    // The real, byte-preserving path on the same original text rejects it.
    let preserved: Vec<Box<serde_json::value::RawValue>> =
        vec![serde_json::from_str(&text).expect("raw")];
    assert!(load_rules_from_raw(1, 0, &preserved).rules.is_empty());
}

// =============================================================================================
// Snapshot identity and the inert switch.
// =============================================================================================

#[test]
fn snapshot_stamp_is_full_identity_and_order_independent() {
    let a = direct_only_rule("p1", PROXY1);
    let b = format!(
        r#"{{"id":"second","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p2":"{PROXY2}"}}}},"condition":true,"action":"deny"}}"#
    );

    let forward = via_audit_request(&[&a, &b]);
    let reversed = via_audit_request(&[&b, &a]);
    assert_eq!(
        forward.snapshot_stamp().alias_set_digest,
        reversed.snapshot_stamp().alias_set_digest,
        "the digest must not depend on rule order"
    );

    // The digest alone is never sufficient: the protocol/content version is part of the identity.
    let mut other = via_audit_request(&[&a, &b]);
    other.content_version = 99;
    assert_ne!(forward.snapshot_stamp(), other.snapshot_stamp());
}

#[test]
fn an_event_only_snapshot_declares_no_alias_so_the_feature_stays_inert() {
    let rule = format!(
        r#"{{"id":"event-only","event_abis":{{"e":{EVENT_ABI}}},"condition":true,"action":"deny"}}"#
    );
    let set = via_audit_request(&[&rule]);
    assert!(!set.declares_any_alias());
    assert!(set.alias_addresses().is_empty());
}

#[test]
fn binding_classes_are_derived_from_the_declared_sections() {
    let transfer = r#"{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":false}"#;
    let mixed = format!(
        r#"{{"id":"m","event_abis":{{"transfer":{transfer}}},"callstack":{{"direct_delegatecalls":{{"proxy1":"{PROXY1}"}}}},"condition":true,"action":"deny"}}"#
    );
    assert_eq!(via_audit_request(&[&mixed]).rules[0].binding_class(), RuleBindingClass::Mixed);
    assert_eq!(
        via_audit_request(&[&direct_only_rule("p", PROXY1)]).rules[0].binding_class(),
        RuleBindingClass::DirectOnly
    );
}

#[test]
fn the_v2_selector_variables_are_not_provided_under_this_protocol() {
    // `matched` / `callType` / `address` / `codeHash` / `implAddress` / `implCodeHash` are not
    // part of this schema, and the protocol version does not bump.
    assert_eq!(crate::config::SUPPORTED_PROTOCOL_VERSIONS, &[1]);
    let rule = format!(
        r#"{{"id":"v2-shape","event_abis":{{}},"callstack":{{"selectors":{{"proxy1":{{"address":"{PROXY1}"}}}}}},"condition":true,"action":"deny"}}"#
    );
    rejected_at_both_entry_points(&rule, "selectors");
}

#[test]
fn a_valid_direct_only_rule_compiles_with_its_alias_bound_to_the_parsed_address() {
    let set = accepted_at_both_entry_points(&direct_only_rule("proxy1", PROXY1));
    let compiled = &set.rules[0];
    assert_eq!(compiled.direct_delegatecalls.len(), 1);
    assert_eq!(compiled.direct_delegatecalls[0].alias, "proxy1");
    assert_eq!(
        compiled.direct_delegatecalls[0].address,
        PROXY1.parse::<alloy_primitives::Address>().unwrap()
    );
    assert!(set.declares_any_alias());
    assert_eq!(set.alias_addresses().len(), 1);
}

#[test]
fn the_documented_wire_example_loads() {
    // The example from the design, verbatim in shape.
    let rule = json!({
        "id": "proxy-codehash-guard",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "proxy1": PROXY1 } },
        "condition": { "!": { "in": [ { "var": "proxy1.delegateCodeHash" },
                                      [ "0xbbbb000000000000000000000000000000000000000000000000000000000000" ] ] } },
        "action": "deny"
    })
    .to_string();
    let set = accepted_at_both_entry_points(&rule);
    assert_eq!(set.rules[0].id, "proxy-codehash-guard");
}
