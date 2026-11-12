use super::*;
use crate::rules::{Action, CompiledCondition};
use crate::test_support::golden;
use serde_json::{json, Value};

fn parse(json: &str) -> RawRule {
    serde_json::from_str(json).expect("valid rule json")
}

/// Minimal single-event `deny` rule carrying the given condition, for exercising
/// condition compilation and cross-rule set interning.
fn deny_rule_with_condition(id: &str, condition: Value) -> RawRule {
    serde_json::from_value(json!({
        "id": id,
        "event_abis": {
            "e": {
                "type": "event",
                "name": "E",
                "inputs": [{"name": "a", "type": "address", "indexed": false}],
                "anonymous": false
            }
        },
        "condition": condition,
        "action": "deny"
    }))
    .expect("valid rule")
}

#[test]
fn load_rules_shares_arc_across_rules_and_standalone_compiles() {
    // two rules, same static address list (different order) → one shared Arc
    let a = "0x0101010101010101010101010101010101010101";
    let b = "0x0202020202020202020202020202020202020202";
    let raw = vec![
        deny_rule_with_condition("r1", json!({"in": [{"var":"origin"}, [a, b]]})),
        deny_rule_with_condition("r2", json!({"in": [{"var":"origin"}, [b, a]]})),
    ];
    let set = load_rules(1, 1, raw);
    assert_eq!(set.rules.len(), 2);
    let arc_of = |r: &CompiledRule| match &r.compiled_condition {
        CompiledCondition::AddressSetIn { set, .. } => set.clone(),
        other => panic!("expected AddressSetIn, got {other:?}"),
    };
    assert!(std::sync::Arc::ptr_eq(&arc_of(&set.rules[0]), &arc_of(&set.rules[1])));

    // standalone compile_rule (fresh interner) still succeeds
    let standalone =
        compile_rule(deny_rule_with_condition("r3", json!({"in": [{"var":"origin"}, [a]]})))
            .unwrap();
    assert!(matches!(standalone.compiled_condition, CompiledCondition::AddressSetIn { .. }));
}

#[test]
fn scenario_a_rule_compiles_and_indexes() {
    let set = load_rules(1, 1, vec![parse(golden::RULE_SCENARIO_A)]);
    assert_eq!(set.rules.len(), 1);
    assert_eq!(set.rules[0].id, "risk-check-erc20-tokenx-bridge");
    assert_eq!(set.rules[0].action, Action::Audit);
    // Transfer(address,address,uint256) topic0 is indexed.
    assert_eq!(set.index.len(), 1);
}

#[test]
fn missing_input_name_rejected() {
    let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"type":"address","indexed":true}],"anonymous":false}},"condition":true,"action":"deny"}"#;
    assert!(compile_rule(parse(json)).is_err());
}

#[test]
fn duplicate_input_name_rejected() {
    let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":false},{"name":"a","type":"uint256","indexed":false}],"anonymous":false}},"condition":true,"action":"deny"}"#;
    assert!(compile_rule(parse(json)).is_err());
}

#[path = "rules__validation__tests__selector_family_tests.rs"]
#[cfg(test)]
mod selector_family_tests;

#[test]
fn event_alias_depth_above_eight_is_rejected() {
    let mut raw = parse(
        r#"{"id":"deep","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":true,"action":"deny"}"#,
    );
    let event = raw.event_abis.remove("e").unwrap();
    for index in 0..8 {
        raw.event_abis.insert(format!("e{index}"), event.clone());
    }
    assert!(compile_rule(raw.clone()).is_ok(), "eight aliases must remain valid");
    raw.event_abis.insert("e8".to_string(), event);

    let set = load_rules(1, 1, vec![raw]);
    assert!(set.rules.is_empty());
    assert_eq!(set.rejected.len(), 1);
    assert!(set.rejected[0].reason.contains("at most 8"));
}

#[test]
fn audit_rule_with_empty_audit_types_is_rejected() {
    let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"audit_types":[],"condition":true,"action":"audit"}"#;
    assert!(compile_rule(parse(json)).is_err());
}

#[test]
fn quota_wrong_shape_rejected() {
    // audit_types quota but shape is not ERC20/ERC1155.
    let json = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":false}],"anonymous":false}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
    assert!(compile_rule(parse(json)).is_err());
}

#[test]
fn rendered_quota_rule_accepts_transfer_batch_shape() {
    let json = r#"{
          "id":"rendered-quota-rule",
          "event_abis":{
            "transfer":{"type":"event","name":"Transfer","inputs":[
              {"name":"from","type":"address","indexed":true},
              {"name":"to","type":"address","indexed":true},
              {"name":"value","type":"uint256","indexed":false}],"anonymous":false},
            "transferSingle":{"type":"event","name":"TransferSingle","inputs":[
              {"name":"operator","type":"address","indexed":true},
              {"name":"from","type":"address","indexed":true},
              {"name":"to","type":"address","indexed":true},
              {"name":"id","type":"uint256","indexed":false},
              {"name":"value","type":"uint256","indexed":false}],"anonymous":false},
            "transferBatch":{"type":"event","name":"TransferBatch","inputs":[
              {"name":"operator","type":"address","indexed":true},
              {"name":"from","type":"address","indexed":true},
              {"name":"to","type":"address","indexed":true},
              {"name":"ids","type":"uint256[]","indexed":false},
              {"name":"values","type":"uint256[]","indexed":false}],"anonymous":false}
          },
          "audit_types":["quota"],
          "condition":true,
          "action":"audit",
          "audit_timeout_action":"allow"
        }"#;

    let compiled = compile_rule(parse(json)).expect("RCS-rendered quota rule must compile");
    assert_eq!(compiled.events.len(), 3);
}

#[test]
fn non_audit_rules_do_not_require_quota_event_shapes() {
    let deny = r#"{"id":"deny-owner-change","event_abis":{"owner_change":{"type":"event","name":"OwnershipTransferred","inputs":[{"name":"previousOwner","type":"address","indexed":true},{"name":"newOwner","type":"address","indexed":true}],"anonymous":false}},"condition":true,"action":"deny"}"#;
    let allow = r#"{"id":"allow-owner-change","event_abis":{"owner_change":{"type":"event","name":"OwnershipTransferred","inputs":[{"name":"previousOwner","type":"address","indexed":true},{"name":"newOwner","type":"address","indexed":true}],"anonymous":false}},"condition":true,"action":"allow"}"#;

    for (json, expected_action) in [(deny, Action::Deny), (allow, Action::Allow)] {
        let rule = compile_rule(parse(json)).expect("non-audit rule is not quota-shaped");
        assert_eq!(rule.action, expected_action);
        assert_eq!(rule.audit_types, [QUOTA]);
    }
}

#[test]
fn audit_without_timeout_action_defaults_allow() {
    let json = r#"{"id":"r","event_abis":{"transfer":{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":false}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
    let r = compile_rule(parse(json)).expect("valid");
    assert_eq!(r.audit_timeout_action, TimeoutAction::Allow);
}

#[test]
fn invalid_rules_do_not_drop_valid_ones() {
    let bad = r#"{"id":"bad","event_abis":{},"condition":true,"action":"deny"}"#;
    let set = load_rules(1, 5, vec![parse(bad), parse(golden::RULE_SCENARIO_A)]);
    assert_eq!(set.rules.len(), 1);
    assert_eq!(set.content_version, 5);
}

#[test]
fn missing_required_rule_and_event_fields_fail_decoding() {
    // `event_abis` is optional on the wire now that a rule may select purely on call frames,
    // so an omitted map decodes to an empty one. The rule is still refused — one layer later,
    // at validation, with a reason naming both selector families.
    let missing_event_abis = r#"{"id":"r","condition":true,"action":"deny"}"#;
    let decoded = serde_json::from_str::<RawRule>(missing_event_abis)
        .expect("an omitted event_abis decodes to an empty map");
    assert!(decoded.event_abis.is_empty());
    assert!(decoded.callstack.is_none(), "an omitted callstack is an absent section");
    let reason = compile_rule(decoded).expect_err("a rule that can never match");
    assert!(reason.contains("neither event_abis nor callstack"), "{reason}");

    for event in [
        r#"{"name":"E","inputs":[],"anonymous":false}"#,
        r#"{"type":"event","inputs":[],"anonymous":false}"#,
        r#"{"type":"event","name":"E","anonymous":false}"#,
        r#"{"type":"event","name":"E","inputs":[]}"#,
    ] {
        let json = format!(
            r#"{{"id":"r","event_abis":{{"e":{event}}},"condition":true,"action":"deny"}}"#
        );
        assert!(serde_json::from_str::<RawRule>(&json).is_err(), "accepted {event}");
    }

    let missing_indexed = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address"}],"anonymous":false}},"condition":true,"action":"deny"}"#;
    assert!(serde_json::from_str::<RawRule>(missing_indexed).is_err());
}

#[test]
fn event_topic_limits_are_enforced() {
    let non_anonymous = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":true},{"name":"b","type":"address","indexed":true},{"name":"c","type":"address","indexed":true},{"name":"d","type":"address","indexed":true}],"anonymous":false}},"condition":true,"action":"deny"}"#;
    assert!(compile_rule(parse(non_anonymous)).is_err());

    let anonymous = r#"{"id":"r","event_abis":{"e":{"type":"event","name":"E","inputs":[{"name":"a","type":"address","indexed":true},{"name":"b","type":"address","indexed":true},{"name":"c","type":"address","indexed":true},{"name":"d","type":"address","indexed":true}],"anonymous":true}},"condition":true,"action":"deny"}"#;
    assert!(compile_rule(parse(anonymous)).is_ok());
}

#[test]
fn duplicate_rule_ids_are_all_rejected_without_dropping_unique_rules() {
    let duplicate = r#"{"id":"duplicate","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":true,"action":"deny"}"#;
    let unique = r#"{"id":"unique","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":true,"action":"deny"}"#;
    let set = load_rules(1, 1, vec![parse(duplicate), parse(duplicate), parse(unique)]);
    assert_eq!(set.rules.len(), 1);
    assert_eq!(set.rules[0].id, "unique");
}

#[test]
fn only_exact_event_abi_type_is_accepted() {
    let rule = |abi_type: &str| {
        format!(
            r#"{{"id":"r","event_abis":{{"e":{{"type":"{abi_type}","name":"E","inputs":[],"anonymous":false}}}},"condition":true,"action":"deny"}}"#
        )
    };
    assert!(compile_rule(parse(&rule("event"))).is_ok());
    assert!(compile_rule(parse(&rule("function"))).is_err());
    assert!(compile_rule(parse(&rule("Event"))).is_err());
}

#[test]
fn invalid_jsonlogic_rejects_only_affected_rules() {
    let invalid_operator = r#"{"id":"bad-op","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":{"cat":[1,2]},"action":"deny"}"#;
    let invalid_arity = r#"{"id":"bad-arity","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":{"==":[1]},"action":"deny"}"#;
    let valid = r#"{"id":"valid","event_abis":{"e":{"type":"event","name":"E","inputs":[],"anonymous":false}},"condition":{"==":[1,1]},"action":"deny"}"#;
    let set = load_rules(1, 1, vec![parse(invalid_operator), parse(valid), parse(invalid_arity)]);
    assert_eq!(set.rules.len(), 1);
    assert_eq!(set.rules[0].id, "valid");
}
