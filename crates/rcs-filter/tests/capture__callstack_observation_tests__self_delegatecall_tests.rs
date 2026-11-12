//! A successful Proxy self-`DELEGATECALL` on a real EVM: the frame must be observed as the
//! proxy's own direct delegatecall (four fields, including the proxy's own code hash), not as a
//! second plain call that a whitelist rule can never see.

use std::sync::Arc;

use crate::callstack::Observations;
use crate::config::MIN_PROTOCOL_VERSION;
use crate::handle::{FilterHandle, Screen, ScreenInput};
use crate::matching::{try_evaluate, MatchOutcome};
use crate::rules::{load_rules, RawRule, RuleSet, DEFAULT_MAX_EVENT_BINDINGS_PER_TX};
use crate::test_support::TestClock;
use crate::FilterConfig;

use super::*;

const P: Address = Address::repeat_byte(0x61);
const I: Address = Address::repeat_byte(0x62);
/// A code hash that is deliberately not any address's code in these fixtures.
const FOREIGN_HASH: B256 = B256::repeat_byte(0x99);

fn hex_addr(address: Address) -> String {
    format!("{address:#x}")
}

fn hex_hash(hash: B256) -> String {
    format!("{hash:#x}")
}

fn var(name: &str) -> serde_json::Value {
    serde_json::json!({ "var": name })
}

/// One `deny` rule on a single `direct_delegatecalls` alias.
fn rule_set(alias: &str, address: Address, condition: serde_json::Value) -> RuleSet {
    let mut aliases = serde_json::Map::new();
    aliases.insert(alias.to_string(), serde_json::Value::String(hex_addr(address)));
    let raw: RawRule = serde_json::from_value(serde_json::json!({
        "id": "self-delegatecall-guard",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": aliases },
        "condition": condition,
        "action": "deny"
    }))
    .expect("the rule deserializes");
    let rules = load_rules(MIN_PROTOCOL_VERSION, 1, vec![raw]);
    assert!(rules.rejected.is_empty(), "the fixture rule must load cleanly: {:?}", rules.rejected);
    rules
}

/// `CALLDATASIZE; PUSH1 20; JUMPI` — the outer call has no calldata and self-delegates once; the
/// delegated frame's one-byte calldata makes it jump over the delegatecall and stop.
fn self_delegating_proxy() -> Vec<u8> {
    vec![
        0x36, // CALLDATASIZE
        0x60, 0x14, // PUSH1 20 (jump destination)
        0x57, // JUMPI
        0x60, 0x00, // PUSH1 0 retSize
        0x60, 0x00, // PUSH1 0 retOffset
        0x60, 0x01, // PUSH1 1 argsSize
        0x60, 0x00, // PUSH1 0 argsOffset
        0x30, // ADDRESS
        0x62, 0x07, 0xA1, 0x20, // PUSH3 500000 gas
        0xF4, // DELEGATECALL
        0x50, // POP
        0x00, // STOP
        0x5b, // JUMPDEST (offset 20)
        0x00, // STOP
    ]
}

/// `CALLDATASIZE; PUSH1 40; JUMPI` — with no calldata it delegates once to `dst`; with any
/// calldata it jumps straight to the final `STOP`. The calldata branch is what keeps a
/// delegatecall chain from recursing when it comes back to this contract.
fn proxy_delegating_once_to(dst: Address) -> Vec<u8> {
    let mut code = vec![
        0x36, // CALLDATASIZE
        0x60, 0x28, // PUSH1 40 (jump destination)
        0x57, // JUMPI
        0x60, 0x00, // PUSH1 0 retSize
        0x60, 0x00, // PUSH1 0 retOffset
        0x60, 0x01, // PUSH1 1 argsSize
        0x60, 0x00, // PUSH1 0 argsOffset
        0x73, // PUSH20 dst
    ];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[0x62, 0x07, 0xA1, 0x20, 0xF4, 0x50, 0x00]); // gas; DELEGATECALL; POP; STOP
    code.extend_from_slice(&[0x5b, 0x00]); // JUMPDEST (offset 40); STOP
    code
}

/// `DELEGATECALL(gas, dst, 0, 1, 0, 0); POP; STOP` — delegates into `dst` with one byte of
/// calldata, so a calldata-branching target stops instead of delegating again.
fn delegatecall_one_byte_to(dst: Address) -> Vec<u8> {
    let mut code = vec![
        0x60, 0x00, // PUSH1 0 retSize
        0x60, 0x00, // PUSH1 0 retOffset
        0x60, 0x01, // PUSH1 1 argsSize
        0x60, 0x00, // PUSH1 0 argsOffset
        0x73, // PUSH20 dst
    ];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[0x62, 0x07, 0xA1, 0x20, 0xF4, 0x50, 0x00]); // gas; DELEGATECALL; POP; STOP
    code
}

fn db_with(codes: &[(Address, Vec<u8>)]) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    for (address, code) in codes {
        db.insert_account_info(*address, account(0, Some(code.clone())));
    }
    db
}

/// Captures one call to `target` under `rules`' own selector set and stamp.
fn capture_with(db: InMemoryDB, target: Address, rules: &RuleSet) -> (Vec<CallObservation>, bool) {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::with_callstack(
            1_000,
            16,
            CallstackCaptureConfig::new(rules.stamp(), rules.callstack_selectors().to_vec()),
        ),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(call_tx(target)).expect("tx executes");
    let success = result.result.is_success();
    let items = match evm.components_mut().1.finish_capture(result.result.logs()) {
        CaptureOutcome::Complete { observations, .. } => observations.items,
        other => panic!("expected a completed capture, got {other:?}"),
    };
    (items, success)
}

fn screen_with(rules: &RuleSet, items: &[CallObservation]) -> MatchOutcome {
    try_evaluate(
        rules,
        &ScreenInput {
            tx_hash: B256::repeat_byte(0x77),
            origin: caller(),
            tx_to: Some(P),
            nonce: 0,
            value: U256::ZERO,
            block_height: 1,
            logs: &[],
            observations: Observations::available(rules.stamp(), items),
        },
        DEFAULT_MAX_EVENT_BINDINGS_PER_TX,
    )
    .expect("screening succeeds")
}

/// The same evaluation through the block builder's handle entry point, as a fresh transaction.
fn screen_via_handle(rules: &RuleSet, items: &[CallObservation]) -> Screen {
    let pinned = Arc::new(rules.clone());
    let handle =
        FilterHandle::for_test(FilterConfig::default(), rules.clone(), Arc::new(TestClock::new(1)));
    let input = ScreenInput {
        tx_hash: B256::repeat_byte(0x78),
        origin: caller(),
        tx_to: Some(P),
        nonce: 0,
        value: U256::ZERO,
        block_height: 1,
        logs: &[],
        observations: Observations::available(rules.stamp(), items),
    };
    handle.screen_tx_with_snapshot(&input, &pinned)
}

/// C1 — a successful self-delegatecall yields the call frame plus a four-field direct
/// delegatecall whose implementation is the proxy itself.
#[test]
fn a_successful_self_delegatecall_is_a_direct_delegatecall_observation() {
    let proxy_code = self_delegating_proxy();
    let db = db_with(&[(P, proxy_code.clone())]);
    let (outcome, success) = run_observing_tx(db, P, &[P], 16);
    assert!(success, "the self-delegatecall transaction succeeds");
    assert_eq!(
        observations(outcome),
        vec![
            CallObservation::call(P, code_hash(&proxy_code)),
            CallObservation::direct_delegatecall(
                P,
                code_hash(&proxy_code),
                P,
                code_hash(&proxy_code),
            ),
        ]
    );
}

/// C2 — a rule whose whitelist only names an external implementation denies the self-delegation,
/// through both the pure matcher and the block builder's screening entry.
#[test]
fn a_whitelist_that_omits_the_proxys_own_code_denies_the_self_delegatecall() {
    let rules = rule_set(
        "p",
        P,
        serde_json::json!({ "!": { "in": [
            { "var": "p.delegateCodeHash" }, [hex_hash(FOREIGN_HASH)]
        ] } }),
    );
    let db = db_with(&[(P, self_delegating_proxy())]);
    let (items, success) = capture_with(db, P, &rules);
    assert!(success);
    assert_eq!(screen_with(&rules, &items), MatchOutcome::Deny);
    assert_eq!(screen_via_handle(&rules, &items), Screen::Deny);
}

/// C3 — the legitimate control: the whitelist contains the proxy's own code hash and target.
#[test]
fn a_whitelist_that_contains_the_proxys_own_code_allows_the_self_delegatecall() {
    let proxy_code = self_delegating_proxy();
    let own_hash = code_hash(&proxy_code);
    let rules = rule_set(
        "p",
        P,
        serde_json::json!({ "!": { "and": [
            { "==": [ var("p.delegateTo"), hex_addr(P) ] },
            { "==": [ var("p.delegateCodeHash"), hex_hash(own_hash) ] }
        ] } }),
    );
    let db = db_with(&[(P, proxy_code)]);
    let (items, success) = capture_with(db, P, &rules);
    assert!(success);
    assert_eq!(screen_with(&rules, &items), MatchOutcome::Allow);
}

/// C4 — every one of the four fields is bound for the self-delegation.
#[test]
fn all_four_fields_bind_the_proxy_to_itself() {
    let proxy_code = self_delegating_proxy();
    let own_hash = code_hash(&proxy_code);
    let rules = rule_set(
        "p",
        P,
        serde_json::json!({ "and": [
            { "==": [ var("p.proxyAddress"), hex_addr(P) ] },
            { "==": [ var("p.proxyCodeHash"), hex_hash(own_hash) ] },
            { "==": [ var("p.delegateTo"), hex_addr(P) ] },
            { "==": [ var("p.delegateCodeHash"), hex_hash(own_hash) ] }
        ] }),
    );
    let db = db_with(&[(P, proxy_code)]);
    let (items, success) = capture_with(db, P, &rules);
    assert!(success);
    assert_eq!(screen_with(&rules, &items), MatchOutcome::Deny);
}

/// C5 — the plain-call control is unchanged: own code, no delegatecall, no direct observation.
#[test]
fn a_plain_call_into_the_proxy_stays_a_plain_call() {
    let proxy_code = stop_only();
    let db = db_with(&[(P, proxy_code.clone())]);
    let (outcome, success) = run_observing_tx(db, P, &[P], 16);
    assert!(success);
    assert_eq!(observations(outcome), vec![CallObservation::call(P, code_hash(&proxy_code))]);
}

/// C6 — an external delegatecall is unchanged, and the whitelist still denies an illegal target.
#[test]
fn an_external_delegatecall_is_unchanged_and_still_denied() {
    let proxy_code = delegatecall_to(I);
    let impl_code = stop_only();
    let rules = rule_set(
        "p",
        P,
        serde_json::json!({ "!": { "in": [
            { "var": "p.delegateCodeHash" }, [hex_hash(FOREIGN_HASH)]
        ] } }),
    );
    let db = db_with(&[(P, proxy_code.clone()), (I, impl_code.clone())]);
    let (items, success) = capture_with(db, P, &rules);
    assert!(success);
    assert_eq!(
        items,
        vec![
            CallObservation::call(P, code_hash(&proxy_code)),
            CallObservation::direct_delegatecall(
                P,
                code_hash(&proxy_code),
                I,
                code_hash(&impl_code),
            ),
        ]
    );
    assert_eq!(screen_with(&rules, &items), MatchOutcome::Deny);
}

/// C7 — a delegatecall from implementation code back into the proxy stays a plain call: it is not
/// the proxy's own direct delegatecall.
#[test]
fn an_implementation_delegating_back_to_the_proxy_is_not_a_direct_delegatecall() {
    let proxy_code = proxy_delegating_once_to(I);
    let impl_code = delegatecall_one_byte_to(P);
    let rules = rule_set("p", P, serde_json::json!({ "==": [ var("p.delegateTo"), hex_addr(P) ] }));
    let db = db_with(&[(P, proxy_code.clone()), (I, impl_code.clone())]);
    let (items, success) = capture_with(db, P, &rules);
    assert!(success);
    assert_eq!(
        items,
        vec![
            CallObservation::call(P, code_hash(&proxy_code)),
            CallObservation::direct_delegatecall(
                P,
                code_hash(&proxy_code),
                I,
                code_hash(&impl_code),
            ),
            CallObservation::call(P, code_hash(&proxy_code)),
        ]
    );
    // The alias binds the one real direct delegatecall (`delegateTo == I`), so the rule on `P`
    // does not fire.
    assert_eq!(screen_with(&rules, &items), MatchOutcome::Allow);
}
