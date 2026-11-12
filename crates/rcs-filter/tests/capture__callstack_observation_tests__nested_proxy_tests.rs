//! Nested proxies on a real EVM: which direct delegatecalls are observed, and which alias each
//! observation binds through the four condition variables.

use super::*;
use crate::handle::ScreenInput;
use crate::matching::{try_evaluate, MatchOutcome};
use crate::rules::{load_rules, RawRule, RuleSet, DEFAULT_MAX_EVENT_BINDINGS_PER_TX};

const P: Address = Address::repeat_byte(0xC1);
const I: Address = Address::repeat_byte(0xC2);
const Q: Address = Address::repeat_byte(0xC3);
const J: Address = Address::repeat_byte(0xC4);
const R: Address = Address::repeat_byte(0xC5);
const K: Address = Address::repeat_byte(0xC6);
const P_PRIME: Address = Address::repeat_byte(0xC7);

fn db_with(codes: &[(Address, Vec<u8>)]) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    for (address, code) in codes {
        db.insert_account_info(*address, account(0, Some(code.clone())));
    }
    db
}

/// One alias per selected address; `condition` decides.
fn rule_set(aliases: &[(&str, Address)], condition: serde_json::Value) -> RuleSet {
    let aliases: serde_json::Map<String, serde_json::Value> = aliases
        .iter()
        .map(|(alias, address)| ((*alias).to_string(), format!("{address:#x}").into()))
        .collect();
    let raw: RawRule = serde_json::from_value(serde_json::json!({
        "id": "nested",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": aliases },
        "condition": condition,
        "action": "deny"
    }))
    .expect("the rule deserializes");
    let rules = load_rules(crate::config::MIN_PROTOCOL_VERSION, 1, vec![raw]);
    assert!(rules.rejected.is_empty(), "{:?}", rules.rejected);
    rules
}

/// Captures one call to `target` under `rules`' own selectors and stamp, then screens it.
fn capture_and_screen(
    db: InMemoryDB,
    target: Address,
    rules: &RuleSet,
) -> (Vec<CallObservation>, MatchOutcome) {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::callstack_only(
            64,
            CallstackCaptureConfig::new(rules.stamp(), rules.callstack_selectors().to_vec()),
        ),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(call_tx(target)).expect("tx executes");
    assert!(result.result.is_success());
    let items = match evm.components_mut().1.finish_capture(result.result.logs()) {
        CaptureOutcome::Complete { observations, .. } => observations.items,
        other => panic!("expected a completed capture, got {other:?}"),
    };
    let outcome = try_evaluate(
        rules,
        &ScreenInput {
            tx_hash: B256::repeat_byte(0x77),
            origin: caller(),
            tx_to: Some(target),
            nonce: 0,
            value: U256::ZERO,
            block_height: 1,
            logs: &[],
            observations: crate::callstack::Observations::available(rules.stamp(), &items),
        },
        DEFAULT_MAX_EVENT_BINDINGS_PER_TX,
    )
    .expect("screening succeeds");
    (items, outcome)
}

fn direct_delegatecalls(items: &[CallObservation]) -> Vec<CallObservation> {
    items.iter().filter(|o| o.kind == CallObservationKind::DirectDelegatecall).cloned().collect()
}

fn var(name: &str) -> serde_json::Value {
    serde_json::json!({ "var": name })
}

/// P → DELEGATECALL I → I CALLs Q → Q DELEGATECALL J: one observation per selected proxy.
fn proxy_calling_a_proxy() -> (InMemoryDB, [Vec<u8>; 4]) {
    let codes = [delegatecall_to(I), call_to(Q), delegatecall_to(J), stop_only()];
    let db = db_with(&[
        (P, codes[0].clone()),
        (I, codes[1].clone()),
        (Q, codes[2].clone()),
        (J, codes[3].clone()),
    ]);
    (db, codes)
}

#[test]
fn each_selected_proxy_reports_its_own_first_level_delegatecall() {
    let (db, [p_code, i_code, q_code, j_code]) = proxy_calling_a_proxy();
    let rules = rule_set(&[("p", P), ("q", Q)], serde_json::json!(false));
    let (items, _) = capture_and_screen(db, P, &rules);
    assert_eq!(
        direct_delegatecalls(&items),
        vec![
            CallObservation::direct_delegatecall(P, code_hash(&p_code), I, code_hash(&i_code)),
            CallObservation::direct_delegatecall(Q, code_hash(&q_code), J, code_hash(&j_code)),
        ]
    );
}

#[test]
fn a_rule_on_the_inner_proxy_fires_only_on_its_own_binding() {
    let q_code = proxy_calling_a_proxy().1[2].clone();
    // Fires on Q's own four fields.
    let on_q = rule_set(
        &[("q", Q)],
        serde_json::json!({ "and": [
            { "==": [ var("q.proxyAddress"), format!("{Q:#x}") ] },
            { "==": [ var("q.proxyCodeHash"), format!("{:#x}", code_hash(&q_code)) ] },
            { "==": [ var("q.delegateTo"), format!("{J:#x}") ] }
        ]}),
    );
    assert_eq!(capture_and_screen(proxy_calling_a_proxy().0, P, &on_q).1, MatchOutcome::Deny);

    // P's target is never Q's binding, and Q's target is never P's.
    let crossed_q = rule_set(
        &[("q", Q)],
        serde_json::json!({ "==": [ var("q.delegateTo"), format!("{I:#x}") ] }),
    );
    assert_eq!(capture_and_screen(proxy_calling_a_proxy().0, P, &crossed_q).1, MatchOutcome::Allow);
    let crossed_p = rule_set(
        &[("p", P)],
        serde_json::json!({ "==": [ var("p.delegateTo"), format!("{J:#x}") ] }),
    );
    assert_eq!(capture_and_screen(proxy_calling_a_proxy().0, P, &crossed_p).1, MatchOutcome::Allow);
    let p_is_p = rule_set(
        &[("p", P)],
        serde_json::json!({ "==": [ var("p.proxyAddress"), format!("{P:#x}") ] }),
    );
    assert_eq!(
        capture_and_screen(proxy_calling_a_proxy().0, P, &p_is_p).1,
        MatchOutcome::Deny,
        "control"
    );
}

#[test]
fn proxy_code_delegating_further_is_implementation_internal() {
    // P → DELEGATECALL R, so P's frame runs R's code, and that code DELEGATECALLs K. Only the
    // first layer is P's: K is never observed, even with R itself selected.
    let p_code = delegatecall_to(R);
    let r_code = delegatecall_to(K);
    let db = db_with(&[(P, p_code.clone()), (R, r_code.clone()), (K, stop_only())]);
    let rules = rule_set(
        &[("p", P), ("r", R)],
        serde_json::json!({ "==": [ var("p.delegateTo"), format!("{K:#x}") ] }),
    );
    let (items, outcome) = capture_and_screen(db, P, &rules);
    assert_eq!(
        direct_delegatecalls(&items),
        vec![CallObservation::direct_delegatecall(P, code_hash(&p_code), R, code_hash(&r_code))]
    );
    assert!(
        !items.iter().any(|o| o.address == K || o.impl_address == Some(K)),
        "K must never be observed: {items:?}"
    );
    assert!(!items.iter().any(|o| o.address == R), "R never runs as itself: {items:?}");
    // `r` is unobserved, so the rule is suppressed whatever its condition says.
    assert_eq!(outcome, MatchOutcome::Allow);
}

#[test]
fn a_selected_address_reached_only_by_delegatecall_reports_nothing_for_itself() {
    // P → DELEGATECALL I → I DELEGATECALLs P'. P' is selected but never runs as itself.
    let p_code = delegatecall_to(I);
    let i_code = delegatecall_to(P_PRIME);
    let db = db_with(&[(P, p_code.clone()), (I, i_code.clone()), (P_PRIME, stop_only())]);
    let rules = rule_set(
        &[("p", P), ("pp", P_PRIME)],
        serde_json::json!({ "==": [ var("pp.proxyAddress"), format!("{P_PRIME:#x}") ] }),
    );
    let (items, outcome) = capture_and_screen(db, P, &rules);
    assert_eq!(
        direct_delegatecalls(&items),
        vec![CallObservation::direct_delegatecall(P, code_hash(&p_code), I, code_hash(&i_code))]
    );
    assert!(!items.iter().any(|o| o.address == P_PRIME), "nothing for P': {items:?}");
    assert_eq!(outcome, MatchOutcome::Allow, "the alias on P' is unobserved, so suppressed");
}
