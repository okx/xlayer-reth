use super::*;
use crate::rules::{load_rules, RawRule};
use crate::test_support::{golden, log_builder};
use alloy_primitives::{keccak256, Bytes, LogData, U256};
use serde_json::json;

fn ruleset_a() -> RuleSet {
    load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()])
}

fn audit_rule(
    id: &str,
    event_alias: &str,
    event_name: &str,
    inputs: Value,
    audit_type: &str,
) -> RawRule {
    let mut event_abis = serde_json::Map::new();
    event_abis.insert(
        event_alias.to_string(),
        json!({
            "type": "event",
            "name": event_name,
            "inputs": inputs,
            "anonymous": false
        }),
    );
    serde_json::from_value(json!({
        "id": id,
        "event_abis": event_abis,
        "audit_types": [audit_type],
        "condition": true,
        "action": "audit"
    }))
    .unwrap()
}

fn erc20_audit_rule(id: &str, event_alias: &str) -> RawRule {
    audit_rule(
        id,
        event_alias,
        "Transfer",
        json!([
            {"name": "from", "type": "address", "indexed": true},
            {"name": "to", "type": "address", "indexed": true},
            {"name": "value", "type": "uint256", "indexed": false}
        ]),
        "quota",
    )
}

fn empty_event_log(address: Address, event_name: &str) -> Log {
    let topic0 = keccak256(format!("{event_name}()").as_bytes());
    Log { address, data: LogData::new_unchecked(vec![topic0], Bytes::new()) }
}

fn reset_decode_attempts() {
    TEST_DECODE_ATTEMPTS.with(|attempts| attempts.set(0));
}

fn decode_attempts() -> usize {
    TEST_DECODE_ATTEMPTS.with(std::cell::Cell::get)
}

fn transfer_input<'a>(logs: &'a [Log]) -> ScreenInput<'a> {
    ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: U256::ZERO,
        block_height: 1_000_000,
        logs,
        observations: Observations::NOT_CAPTURED,
    }
}

/// Emergency "deny everything except the TxBlacklist contract" rule. Declares a carrier
/// Transfer event (empty event_abis is rejected by compile_rule); the deny decision rides on
/// the tx-level `contract_address` condition, so no-log / contract-creation txs are covered.
const TX_BLACKLIST: &str = "0xb1ac000000000000000000000000000000000001";

fn emergency_deny_all_rule() -> RuleSet {
    let raw: RawRule = serde_json::from_value(json!({
        "id": "emergency-deny-all",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to", "type": "address", "indexed": true},
                    {"name": "value", "type": "uint256", "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": { "!=": [ {"var": "contract_address"}, TX_BLACKLIST ] },
        "action": "deny"
    }))
    .unwrap();
    load_rules(1, 1, vec![raw])
}

fn input_to(to: Option<Address>, logs: &[Log]) -> ScreenInput<'_> {
    ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: to,
        nonce: 1,
        value: U256::ZERO,
        block_height: 1_000_000,
        logs,
        observations: Observations::NOT_CAPTURED,
    }
}

#[test]
fn no_log_tx_hits_contract_address_deny() {
    // AC#1: a native/no-log transaction to a normal business target is denied.
    let rules = emergency_deny_all_rule();
    assert_eq!(
        evaluate(&rules, &input_to(Some(golden::claim_contract()), &[])),
        MatchOutcome::Deny
    );
}

#[test]
fn blacklist_target_is_not_denied() {
    // AC#3: a transaction whose `to` is the TxBlacklist contract must NOT be denied.
    let rules = emergency_deny_all_rule();
    let blacklist: Address = TX_BLACKLIST.parse().unwrap();
    assert_eq!(evaluate(&rules, &input_to(Some(blacklist), &[])), MatchOutcome::Allow);
}

#[test]
fn contract_creation_is_denied() {
    // AC#4: contract creation (tx.to == null) binds contract_address to null → denied.
    let rules = emergency_deny_all_rule();
    assert_eq!(evaluate(&rules, &input_to(None, &[])), MatchOutcome::Deny);
}

#[test]
fn no_log_tx_negative_condition_on_missing_event_field_denies() {
    // AC#2: a deny rule whose condition tests a MISSING event field under null semantics.
    let raw: RawRule = serde_json::from_value(json!({
        "id": "deny-when-transfer-to-null",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to", "type": "address", "indexed": true},
                    {"name": "value", "type": "uint256", "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": { "==": [ {"var": "transfer.to"}, null ] },
        "action": "deny"
    }))
    .unwrap();
    let rules = load_rules(1, 1, vec![raw]);
    assert_eq!(
        evaluate(&rules, &input_to(Some(golden::claim_contract()), &[])),
        MatchOutcome::Deny
    );
}

#[test]
fn unrelated_log_is_treated_as_missing_event_and_still_denies() {
    // AC#5: a log unrelated to the rule's event decodes to nothing → missing event →
    // the deny-all condition still fires on a no-matching-event transaction.
    let rules = emergency_deny_all_rule();
    let unrelated = empty_event_log(golden::token_x(), "SomethingElse");
    assert_eq!(
        evaluate(&rules, &input_to(Some(golden::claim_contract()), &[unrelated])),
        MatchOutcome::Deny
    );
}

#[test]
fn all_events_missing_audit_emits_no_action_and_allows() {
    // AC#7: an audit rule whose condition holds ONLY under an all-null binding must not
    // produce an Audit (no empty action, no submit) — it stays Allow.
    let mut rule = audit_rule("audit-null", "event", "Event", json!([]), "custom");
    rule.condition = json!(true);
    let rules = load_rules(1, 1, vec![rule]);
    assert_eq!(
        evaluate(&rules, &input_to(Some(golden::claim_contract()), &[])),
        MatchOutcome::Allow
    );
}

#[test]
fn deny_beats_audit_under_all_null_bindings() {
    // AC#6: with both an audit rule (no-op under all-null) and a deny rule (fires under
    // all-null), the transaction is denied. Priority deny > audit > allow is preserved.
    let mut audit = audit_rule("audit-null", "event", "Event", json!([]), "custom");
    audit.condition = json!(true);
    let deny: RawRule = serde_json::from_value(json!({
        "id": "deny-null",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to", "type": "address", "indexed": true},
                    {"name": "value", "type": "uint256", "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": true,
        "action": "deny"
    }))
    .unwrap();
    let rules = load_rules(1, 1, vec![audit, deny]);
    assert_eq!(
        evaluate(&rules, &input_to(Some(golden::claim_contract()), &[])),
        MatchOutcome::Deny
    );
}

#[test]
fn later_matching_log_denies_and_discards_collected_audit_actions() {
    let audit = erc20_audit_rule("audit-first", "transfer");
    let mut deny = erc20_audit_rule("deny-later", "transfer");
    deny.action = Action::Deny;
    deny.condition = json!({
        "==": [
            {"var": "transfer.from"},
            golden::BLACKLISTED_FROM
        ]
    });
    let rules = load_rules(1, 1, vec![audit, deny]);
    let logs = [
        log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        ),
        log_builder::erc20_transfer(
            golden::token_x(),
            golden::blacklisted_from(),
            golden::recipient(),
            golden::one_token(),
        ),
    ];

    assert_eq!(evaluate(&rules, &transfer_input(&logs)), MatchOutcome::Deny);
}

#[test]
fn later_matching_log_is_included_in_audit_actions() {
    let mut audit = erc20_audit_rule("audit-later", "transfer");
    audit.condition = json!({
        "==": [
            {"var": "transfer.from"},
            golden::BLACKLISTED_FROM
        ]
    });
    let rules = load_rules(1, 1, vec![audit]);
    let logs = [
        log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            U256::from(1),
        ),
        log_builder::erc20_transfer(
            golden::token_x(),
            golden::blacklisted_from(),
            golden::recipient(),
            U256::from(2),
        ),
    ];

    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &transfer_input(&logs)) else {
        panic!("later matching physical log must produce Audit")
    };
    assert_eq!(actions["quota"].len(), 1);
    assert_eq!(actions["quota"][0].params["from"], json!(golden::BLACKLISTED_FROM));
    assert_eq!(actions["quota"][0].params["value"], json!("2"));
}

#[test]
fn identical_physical_logs_at_distinct_indexes_are_all_preserved() {
    let rules = load_rules(1, 1, vec![erc20_audit_rule("audit-both", "transfer")]);
    let log = log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    );
    let logs = [log.clone(), log];

    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &transfer_input(&logs)) else {
        panic!("both physical logs must produce Audit")
    };
    assert_eq!(actions["quota"].len(), 2);
    assert_eq!(actions["quota"][0], actions["quota"][1]);
}

#[test]
fn within_budget_non_first_complete_multi_event_binding_is_evaluated() {
    let raw: RawRule = serde_json::from_value(json!({
        "id": "paired-events",
        "event_abis": {
            "event_a": {
                "type": "event",
                "name": "EventA",
                "inputs": [],
                "anonymous": false
            },
            "event_b": {
                "type": "event",
                "name": "EventB",
                "inputs": [],
                "anonymous": false
            }
        },
        "audit_types": ["custom"],
        "condition": {
            "and": [
                {"==": [{"var": "event_a.address"}, golden::CLAIM_CONTRACT]},
                {"==": [{"var": "event_b.address"}, golden::RECIPIENT]}
            ]
        },
        "action": "audit"
    }))
    .unwrap();
    let rules = load_rules(1, 1, vec![raw]);
    let logs = [
        empty_event_log(golden::token_x(), "EventA"),
        empty_event_log(golden::claim_contract(), "EventB"),
        empty_event_log(golden::claim_contract(), "EventA"),
        empty_event_log(golden::recipient(), "EventB"),
    ];

    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &transfer_input(&logs)) else {
        panic!("non-first complete binding must produce Audit")
    };
    let custom = &actions["custom"];
    assert_eq!(custom.len(), 2);
    assert_eq!(custom[0].name, "event_a");
    assert_eq!(custom[0].address, golden::CLAIM_CONTRACT);
    assert_eq!(custom[1].name, "event_b");
    assert_eq!(custom[1].address, golden::RECIPIENT);
}

#[test]
fn literal_true_rule_without_any_physical_log_denies_but_never_audits() {
    // Deny-All: a literal-true deny rule now denies a no-log transaction; a
    // literal-true audit rule stays Allow because an all-null binding emits no audit content.
    let mut deny = audit_rule("no-log", "event", "Event", json!([]), "custom");
    deny.action = Action::Deny;
    let deny_rules = load_rules(1, 1, vec![deny]);
    assert_eq!(evaluate(&deny_rules, &transfer_input(&[])), MatchOutcome::Deny);

    let audit = audit_rule("no-log", "event", "Event", json!([]), "custom");
    let audit_rules = load_rules(1, 1, vec![audit]);
    assert_eq!(evaluate(&audit_rules, &transfer_input(&[])), MatchOutcome::Allow);
}

#[test]
fn complete_binding_count_rejects_usize_multiplication_overflow() {
    assert_eq!(checked_binding_count([usize::MAX, 2]), None);
}

#[test]
fn excessive_complete_bindings_are_rejected_and_fail_closed() {
    let raw: RawRule = serde_json::from_value(json!({
        "id": "excessive-bindings",
        "event_abis": {
            "a": {"type": "event", "name": "A", "inputs": [], "anonymous": true},
            "b": {"type": "event", "name": "B", "inputs": [], "anonymous": true}
        },
        "audit_types": ["custom"],
        "condition": false,
        "action": "audit"
    }))
    .unwrap();
    let rules = load_rules(1, 1, vec![raw]);
    // 65 × 65 = 4,225 complete bindings, just over the intended 4,096 budget.
    let logs = (0..65)
        .map(|_| Log {
            address: golden::token_x(),
            data: LogData::new_unchecked(vec![], Bytes::new()),
        })
        .collect::<Vec<_>>();
    let input = transfer_input(&logs);

    // An explicit budget of 4,096 rejects the 4,225 bindings.
    assert!(matches!(
        try_evaluate(&rules, &input, 4_096),
        Err(MatchError::BindingBudgetExceeded { .. })
    ));
    // The fail-closed wrapper uses the DEFAULT budget (10,000); a scenario exceeding it
    // (101 × 101 = 10,201) still fails closed to Deny.
    let over_default = (0..101)
        .map(|_| Log {
            address: golden::token_x(),
            data: LogData::new_unchecked(vec![], Bytes::new()),
        })
        .collect::<Vec<_>>();
    assert_eq!(evaluate(&rules, &transfer_input(&over_default)), MatchOutcome::Deny);
}

#[test]
fn complete_binding_budget_is_accumulated_across_candidate_rules() {
    let rule = |id: &str| -> RawRule {
        serde_json::from_value(json!({
            "id": id,
            "event_abis": {
                "a": {"type": "event", "name": "A", "inputs": [], "anonymous": true},
                "b": {"type": "event", "name": "B", "inputs": [], "anonymous": true}
            },
            "audit_types": ["custom"],
            "condition": false,
            "action": "audit"
        }))
        .unwrap()
    };
    let rules = load_rules(1, 1, vec![rule("first"), rule("second")]);
    // Each rule has 46 × 46 = 2,116 bindings (individually below budget), but the
    // evaluation total is 4,232 and must be rejected before recursing into rule two.
    let logs = (0..46)
        .map(|_| Log {
            address: golden::token_x(),
            data: LogData::new_unchecked(vec![], Bytes::new()),
        })
        .collect::<Vec<_>>();

    assert!(matches!(
        try_evaluate(&rules, &transfer_input(&logs), 4_096),
        Err(MatchError::BindingBudgetExceeded { .. })
    ));
}

// Builds a single audit rule with two anonymous zero-topic events, so K matching logs yield
// K × K complete bindings — a precise knob for exercising the configurable binding budget.
fn two_event_rule() -> RuleSet {
    let raw: RawRule = serde_json::from_value(json!({
        "id": "binding-budget",
        "event_abis": {
            "a": {"type": "event", "name": "A", "inputs": [], "anonymous": true},
            "b": {"type": "event", "name": "B", "inputs": [], "anonymous": true}
        },
        "audit_types": ["custom"],
        "condition": false,
        "action": "audit"
    }))
    .unwrap();
    load_rules(1, 1, vec![raw])
}

fn zero_topic_logs(count: usize) -> Vec<Log> {
    (0..count)
        .map(|_| Log {
            address: golden::token_x(),
            data: LogData::new_unchecked(vec![], Bytes::new()),
        })
        .collect()
}

#[test]
fn binding_budget_boundary_100_121() {
    let rules = two_event_rule();
    // 10 × 10 = 100 == limit → allowed (budget check is strictly greater-than).
    assert!(evaluate_checked(&rules, &transfer_input(&zero_topic_logs(10)), 100).is_ok());
    // 11 × 11 = 121 > 100 → rejected.
    assert!(matches!(
        evaluate_checked(&rules, &transfer_input(&zero_topic_logs(11)), 100),
        Err(MatchError::BindingBudgetExceeded { .. })
    ));
}

#[test]
fn low_binding_budget_2_3_boundary() {
    // A single-event rule yields exactly K bindings for K matching logs.
    let raw: RawRule = serde_json::from_value(json!({
        "id": "binding-budget-low",
        "event_abis": {"a": {"type": "event", "name": "A", "inputs": [], "anonymous": true}},
        "audit_types": ["custom"],
        "condition": false,
        "action": "audit"
    }))
    .unwrap();
    let rules = load_rules(1, 1, vec![raw]);
    assert!(evaluate_checked(&rules, &transfer_input(&zero_topic_logs(2)), 2).is_ok());
    assert!(matches!(
        evaluate_checked(&rules, &transfer_input(&zero_topic_logs(3)), 2),
        Err(MatchError::BindingBudgetExceeded { .. })
    ));
}

#[test]
fn filter_skip_contract_mismatch_decodes_zero_logs() {
    let mut raw: RawRule = serde_json::from_str(golden::RULE_SCENARIO_A).unwrap();
    raw.contract_address = Some(golden::CLAIM_CONTRACT.to_string());
    let rules = load_rules(1, 1, vec![raw]);
    let logs = [log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];
    let mut input = transfer_input(&logs);
    input.tx_to = Some(golden::token_x());
    reset_decode_attempts();
    assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
    assert_eq!(decode_attempts(), 0);
}

#[test]
fn filter_skip_origin_mismatch_decodes_zero_logs() {
    let mut raw: RawRule = serde_json::from_str(golden::RULE_SCENARIO_A).unwrap();
    raw.origin = Some(golden::ORIGIN.to_string());
    let rules = load_rules(1, 1, vec![raw]);
    let logs = [log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];
    let mut input = transfer_input(&logs);
    input.origin = golden::recipient();
    reset_decode_attempts();
    assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
    assert_eq!(decode_attempts(), 0);
}

#[test]
fn missing_named_event_binds_null() {
    let raw = r#"{
          "id":"null-companion",
          "event_abis":{
            "transfer":{"type":"event","name":"Transfer","inputs":[
              {"name":"from","type":"address","indexed":true},
              {"name":"to","type":"address","indexed":true},
              {"name":"value","type":"uint256","indexed":false}],"anonymous":false},
            "companion":{"type":"event","name":"Companion","inputs":[],"anonymous":false}
          },
          "audit_types":["custom"],
          "condition":{"==":[{"var":"companion.address"},null]},
          "action":"audit"
        }"#;
    let rules = load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()]);
    let logs = [log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];
    let input = transfer_input(&logs);
    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
        panic!("missing companion must bind null and satisfy the condition")
    };
    assert_eq!(actions["custom"].len(), 1);
    assert_eq!(actions["custom"][0].name, "transfer");
}

#[test]
fn same_log_uses_strictest_action_and_audit_union() {
    let mut allow = erc20_audit_rule("allow", "transfer");
    allow.action = Action::Allow;
    let quota = erc20_audit_rule("quota", "transfer");
    let custom = audit_rule(
        "custom",
        "transfer",
        "Transfer",
        json!([
            {"name": "from", "type": "address", "indexed": true},
            {"name": "to", "type": "address", "indexed": true},
            {"name": "value", "type": "uint256", "indexed": false}
        ]),
        "custom",
    );
    let logs = [log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];
    let input = transfer_input(&logs);
    let rules = load_rules(1, 1, vec![allow, quota, custom]);
    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
        panic!("audit must dominate allow")
    };
    assert_eq!(actions.keys().map(String::as_str).collect::<Vec<_>>(), ["custom", "quota"]);

    let mut deny = erc20_audit_rule("deny", "transfer");
    deny.action = Action::Deny;
    let rules = load_rules(1, 1, vec![erc20_audit_rule("audit", "transfer"), deny]);
    assert_eq!(evaluate(&rules, &input), MatchOutcome::Deny);
}

#[test]
fn deny_stops_remaining_candidate_scan() {
    let mut deny = erc20_audit_rule("deny-first", "transfer");
    deny.action = Action::Deny;
    let rules = load_rules(1, 1, vec![deny, erc20_audit_rule("audit-later", "transfer")]);
    let logs = [log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];
    reset_decode_attempts();
    assert_eq!(evaluate(&rules, &transfer_input(&logs)), MatchOutcome::Deny);
    assert_eq!(decode_attempts(), 1);
}

#[test]
fn scenario_a_matches_audit_with_quota_item() {
    let rules = ruleset_a();
    let logs = vec![log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];
    let input = ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: alloy_primitives::U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };
    match evaluate(&rules, &input) {
        MatchOutcome::Audit { actions, .. } => {
            let quota = actions.get("quota").expect("quota key");
            assert_eq!(quota.len(), 1);
            assert_eq!(quota[0].name, "transfer");
            assert_eq!(quota[0].address, golden::TOKEN_X);
            assert_eq!(quota[0].params.get("value").unwrap(), golden::ONE_TOKEN);
            assert_eq!(quota[0].params.get("from").unwrap(), golden::BRIDGE_ERC20);
        }
        other => panic!("expected Audit, got {other:?}"),
    }
}

#[test]
fn transfer_batch_decodes_uint_arrays_as_json_string_arrays() {
    let rules = load_rules(
        1,
        1,
        vec![audit_rule(
            "batch-quota",
            "transferBatch",
            "TransferBatch",
            json!([
                {"name": "operator", "type": "address", "indexed": true},
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "ids", "type": "uint256[]", "indexed": false},
                {"name": "values", "type": "uint256[]", "indexed": false}
            ]),
            "quota",
        )],
    );
    assert_eq!(rules.rules.len(), 1);

    let logs = vec![log_builder::erc1155_transfer_batch(
        golden::token_x(),
        golden::origin(),
        golden::bridge_erc20(),
        golden::recipient(),
        vec![U256::from(7), U256::from(8)],
        vec![U256::from(100), U256::from(200)],
    )];
    let input = ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };

    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
        panic!("expected audit")
    };
    let item = &actions["quota"][0];
    assert_eq!(item.name, "transferBatch");
    assert_eq!(item.params["operator"], json!(golden::ORIGIN));
    assert_eq!(item.params["from"], json!(golden::BRIDGE_ERC20));
    assert_eq!(item.params["to"], json!(golden::RECIPIENT));
    assert_eq!(item.params["ids"], json!(["7", "8"]));
    assert_eq!(item.params["values"], json!(["100", "200"]));

    let wire = serde_json::to_value(&actions).unwrap();
    assert_eq!(wire["quota"][0]["params"]["ids"], json!(["7", "8"]));
    assert_eq!(wire["quota"][0]["params"]["values"], json!(["100", "200"]));
}

#[test]
fn distinct_aliases_on_same_log_are_deduplicated_by_audit_type_and_log_index() {
    let rules = load_rules(
        1,
        1,
        vec![
            erc20_audit_rule("quota-a", "first_transfer"),
            erc20_audit_rule("quota-b", "second_transfer"),
        ],
    );
    let logs = vec![log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];
    let input = ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: alloy_primitives::U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };

    match evaluate(&rules, &input) {
        MatchOutcome::Audit { actions, .. } => {
            let quota = actions.get("quota").expect("quota key");
            assert_eq!(quota.len(), 1);
            assert_eq!(quota[0].name, "first_transfer");
            assert_eq!(quota[0].params.get("value").unwrap(), golden::ONE_TOKEN);
        }
        other => panic!("expected Audit, got {other:?}"),
    }
}

#[test]
fn same_physical_log_is_kept_independently_for_distinct_audit_types() {
    let rules = load_rules(
        1,
        1,
        vec![
            erc20_audit_rule("quota", "quota_transfer"),
            audit_rule(
                "custom",
                "custom_transfer",
                "Transfer",
                json!([
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to", "type": "address", "indexed": true},
                    {"name": "value", "type": "uint256", "indexed": false}
                ]),
                "custom",
            ),
        ],
    );
    let logs = [log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];

    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &transfer_input(&logs)) else {
        panic!("both audit types must be preserved")
    };
    assert_eq!(actions["quota"].len(), 1);
    assert_eq!(actions["quota"][0].name, "quota_transfer");
    assert_eq!(actions["custom"].len(), 1);
    assert_eq!(actions["custom"][0].name, "custom_transfer");
}

#[test]
fn identical_cross_rule_items_are_deduplicated() {
    let rules = load_rules(
        1,
        1,
        vec![erc20_audit_rule("quota-a", "transfer"), erc20_audit_rule("quota-b", "transfer")],
    );
    let logs = vec![log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    )];
    let input = ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: alloy_primitives::U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };
    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
        panic!("expected audit")
    };
    assert_eq!(actions["quota"].len(), 1);
}

#[test]
fn matching_signature_with_malformed_data_does_not_match() {
    let rules = ruleset_a();
    let topic0 = rules.rules[0].events[0].topic0;
    let logs = vec![Log {
        address: golden::token_x(),
        data: LogData::new_unchecked(vec![topic0], Bytes::new()),
    }];
    let input = ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: alloy_primitives::U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
}

#[test]
fn malformed_log_is_missing_event_denies_for_deny_allows_for_audit() {
    // A signature-matching but undecodable log is a missing event (null binding). Under
    // Deny-All that still denies for a deny rule; an audit rule stays Allow (no empty submit).
    let inputs = json!([
        {"name": "from", "type": "address", "indexed": true},
        {"name": "to", "type": "address", "indexed": true},
        {"name": "value", "type": "uint256", "indexed": false}
    ]);
    let expected = [(Action::Deny, MatchOutcome::Deny), (Action::Audit, MatchOutcome::Allow)];
    for (action, want) in expected {
        let mut raw = erc20_audit_rule("malformed", "transfer");
        raw.action = action;
        raw.event_abis.get_mut("transfer").unwrap().inputs =
            serde_json::from_value(inputs.clone()).unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        let topic0 = rules.rules[0].events[0].topic0;
        let logs = vec![Log {
            address: golden::token_x(),
            data: LogData::new_unchecked(vec![topic0], Bytes::new()),
        }];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };
        assert_eq!(evaluate(&rules, &input), want);
    }
}

#[test]
fn anonymous_indexed_event_matches_by_successful_decode() {
    let raw = r#"{"id":"anon","event_abis":{"transfer":{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":true}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
    let rules = load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()]);
    let normal = log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    );
    let logs = vec![Log {
        address: normal.address,
        data: LogData::new_unchecked(normal.data.topics()[1..].to_vec(), normal.data.data.clone()),
    }];
    let input = ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: alloy_primitives::U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };
    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
        panic!("expected audit")
    };
    assert_eq!(actions["quota"][0].name, "transfer");
    assert_eq!(actions["quota"][0].params["value"], golden::ONE_TOKEN);
}

#[test]
fn anonymous_event_rejects_wrong_topic_count_and_malformed_data() {
    let raw = r#"{"id":"anon","event_abis":{"transfer":{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":true}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
    let rules = load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()]);
    let normal = log_builder::erc20_transfer(
        golden::token_x(),
        golden::bridge_erc20(),
        golden::recipient(),
        golden::one_token(),
    );
    let indexed = normal.data.topics()[1..].to_vec();
    let cases = [
        Log {
            address: normal.address,
            data: LogData::new_unchecked(indexed[..1].to_vec(), normal.data.data.clone()),
        },
        Log {
            address: normal.address,
            data: LogData::new_unchecked(
                [indexed.clone(), vec![keccak256("extra")]].concat(),
                normal.data.data.clone(),
            ),
        },
        Log { address: normal.address, data: LogData::new_unchecked(indexed, Bytes::new()) },
    ];

    for log in cases {
        let logs = [log];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };
        assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
    }
}

#[test]
fn anonymous_non_indexed_event_preserves_all_successful_logs() {
    let raw = r#"{"id":"anon","event_abis":{"message":{"type":"event","name":"Message","inputs":[],"anonymous":true}},"audit_types":["custom"],"condition":true,"action":"audit"}"#;
    let rules = load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()]);
    let logs = vec![
        Log { address: golden::token_x(), data: LogData::new_unchecked(vec![], Bytes::new()) },
        Log {
            address: golden::claim_contract(),
            data: LogData::new_unchecked(vec![], Bytes::new()),
        },
    ];
    let input = ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: alloy_primitives::U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };
    let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
        panic!("expected audit")
    };
    assert_eq!(actions["custom"].len(), 2);
    assert_eq!(actions["custom"][0].address, golden::TOKEN_X);
    assert_eq!(actions["custom"][1].address, golden::CLAIM_CONTRACT);
}

#[test]
fn distinct_logs_in_the_same_audit_type_are_preserved() {
    let rules = load_rules(
        1,
        1,
        vec![
            audit_rule("custom-a", "event_a", "EventA", json!([]), "custom"),
            audit_rule("custom-b", "event_b", "EventB", json!([]), "custom"),
        ],
    );
    let logs = vec![
        empty_event_log(golden::token_x(), "EventA"),
        empty_event_log(golden::claim_contract(), "EventB"),
    ];
    let input = ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: alloy_primitives::U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };

    match evaluate(&rules, &input) {
        MatchOutcome::Audit { actions, .. } => {
            let custom = actions.get("custom").expect("custom key");
            assert_eq!(custom.len(), 2);
            assert_eq!(custom[0].name, "event_a");
            assert_eq!(custom[1].name, "event_b");
        }
        other => panic!("expected Audit, got {other:?}"),
    }
}

#[test]
fn scenario_b_deny_short_circuits() {
    let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_B).unwrap()]);
    let logs = vec![log_builder::erc20_transfer(
        golden::token_x(),
        golden::blacklisted_from(),
        golden::recipient(),
        golden::one_token(),
    )];
    let input = ScreenInput {
        tx_hash: golden::tx_b(),
        origin: golden::origin_b(),
        tx_to: Some(golden::claim_contract()),
        nonce: 0,
        value: alloy_primitives::U256::ZERO,
        block_height: 1_000_000,
        logs: &logs,
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(evaluate(&rules, &input), MatchOutcome::Deny);
}

#[test]
fn no_matching_log_allows() {
    let rules = ruleset_a();
    let input = ScreenInput {
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        tx_to: Some(golden::claim_contract()),
        nonce: 1,
        value: alloy_primitives::U256::ZERO,
        block_height: 1_000_000,
        logs: &[],
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
}
