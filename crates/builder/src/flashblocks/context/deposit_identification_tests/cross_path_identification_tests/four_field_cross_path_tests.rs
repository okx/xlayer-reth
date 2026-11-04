//! The four direct-delegatecall condition variables decide identically on all three paths.

use super::*;

/// "Deny unless all four fields are approved": the proxy itself, the code its own frame ran, the
/// delegate target and the code that ran there.
fn four_field_rule_json(proxy_hash: B256, target: Address, target_hash: B256) -> serde_json::Value {
    serde_json::json!({
        "id": "four-field-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "and": [
            { "==": [ { "var": "entry.proxyAddress" }, format!("{PROXY:#x}") ] },
            { "in": [ { "var": "entry.proxyCodeHash" }, [format!("{proxy_hash:#x}")] ] },
            { "in": [ { "var": "entry.delegateTo" }, [format!("{target:#x}")] ] },
            { "in": [ { "var": "entry.delegateCodeHash" }, [format!("{target_hash:#x}")] ] }
        ]}},
        "action": "deny"
    })
}

#[tokio::test]
async fn the_four_field_rule_reaches_the_same_decision_on_all_three_paths() {
    let proxy_code = delegating_proxy_code(APPROVED_IMPL, 1);
    let proxy_hash = code_hash(&proxy_code);
    let target_hash = code_hash(&impl_code(0x01));
    let cases = [
        ("all four approved", four_field_rule_json(proxy_hash, APPROVED_IMPL, target_hash), false),
        (
            "proxy code hash not approved",
            four_field_rule_json(B256::repeat_byte(0x5e), APPROVED_IMPL, target_hash),
            true,
        ),
        (
            "target address not approved",
            four_field_rule_json(proxy_hash, UNAPPROVED_IMPL, target_hash),
            true,
        ),
        (
            "target code hash not approved",
            four_field_rule_json(proxy_hash, APPROVED_IMPL, B256::repeat_byte(0x5f)),
            true,
        ),
    ];
    for (case, rule, objected) in cases {
        let raw: RawRule = serde_json::from_value(rule.clone()).expect("the rule deserializes");
        let (candidate_observed, screen) = candidate_path(vec![raw.clone()], proxy_code.clone());
        let (deposit_observed, identification) = deposit_path(vec![raw], proxy_code.clone());
        let (audit_observed, verdict, audit_identification, audit_depositor_nonce) =
            audit_path_with_rule(rule, proxy_code.clone());

        let delegatecall = candidate_observed
            .iter()
            .find(|o| o.kind == CallObservationKind::DirectDelegatecall)
            .unwrap_or_else(|| panic!("{case}: the proxy's delegatecall is observed"));
        assert_eq!(delegatecall.address, PROXY, "{case}");
        assert_eq!(delegatecall.code_hash, proxy_hash, "{case}: the proxy's executed code hash");
        assert_eq!(delegatecall.impl_address, Some(APPROVED_IMPL), "{case}");
        assert_eq!(delegatecall.impl_code_hash, Some(target_hash), "{case}");
        assert_eq!(candidate_observed, deposit_observed, "{case}: candidate vs deposit frames");
        assert_eq!(deposit_observed, audit_observed, "{case}: deposit vs audit frames");

        if objected {
            assert_eq!(screen, Screen::Deny, "{case}: candidate loop");
            assert_eq!(identification, DepositIdentification::DenyWorthy, "{case}: deposit loop");
            assert_eq!(verdict, Verdict::Deny, "{case}: audit path");
            assert_eq!(audit_identification, Some(CallstackIdentification::Objected), "{case}");
            assert_eq!(audit_depositor_nonce, 0, "{case}: a denied audit tx commits nothing");
        } else {
            assert_eq!(screen, Screen::Allow, "{case}: candidate loop");
            assert_eq!(identification, DepositIdentification::Allow, "{case}: deposit loop");
            assert_eq!(verdict, Verdict::Allow, "{case}: audit path");
            assert_eq!(audit_identification, Some(CallstackIdentification::Clean), "{case}");
            assert_eq!(audit_depositor_nonce, 1, "{case}: an allowed audit tx commits");
        }
    }
}
