//! The timed-out-allow re-check driven end to end: a real `Proxy` execution produces the call
//! frames, the real buffer lifecycle reaches the fail-open timeout, and the re-screen of the next
//! execution either releases or drops it depending on what actually ran.

use std::sync::Arc;

use rcs_filter::callstack::{Observations, SnapshotStamp};
use rcs_filter::config::MIN_PROTOCOL_VERSION;
use rcs_filter::handle::{FilterHandle, Screen, ScreenInput};
use rcs_filter::pool::BufferStatus;
use rcs_filter::rules::{load_rules, RawRule, RuleSet};
use rcs_filter::test_support::TestClock;
use rcs_filter::FilterConfig;

use super::*;

const START: u64 = 1_751_000_000;
const TX: B256 = B256::repeat_byte(0x99);

const P: Address = Address::repeat_byte(0x71);
const I_OK: Address = Address::repeat_byte(0x72);
const I_ROGUE: Address = Address::repeat_byte(0x73);
const PLAIN: Address = Address::repeat_byte(0x75);
const DST: Address = Address::repeat_byte(0x74);

/// `DELEGATECALL(gas, SLOAD(0))` — the proxy reads its implementation from storage slot 0.
fn proxy_reading_slot_zero() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x54, 0x5a, 0xf4, 0x50, 0x00]
}

/// `CALL(gas, dst, value=1, 0, 0, 0, 0); POP; STOP` — one internal 1-wei transfer.
fn call_value_one_to(dst: Address) -> Vec<u8> {
    let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0x73];
    code.extend_from_slice(dst.as_slice());
    code.extend_from_slice(&[0x5a, 0xf1, 0x50, 0x00]); // GAS CALL POP STOP
    code
}

/// Behaviourally identical to [`call_value_one_to`] with a distinct code hash.
fn rogue_variant(dst: Address) -> Vec<u8> {
    let mut code = vec![0x60, 0x01, 0x50]; // PUSH1 1; POP
    code.extend_from_slice(&call_value_one_to(dst));
    code
}

fn hex_addr(address: Address) -> String {
    format!("{address:#x}")
}

fn hex_hash(hash: B256) -> String {
    format!("{hash:#x}")
}

/// The native-value quota audit rule (fail-open by default) plus the implementation whitelist.
fn rules() -> RuleSet {
    let audit: RawRule = serde_json::from_value(serde_json::json!({
        "id": "native-quota",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    { "name": "from", "type": "address", "indexed": true },
                    { "name": "to", "type": "address", "indexed": true },
                    { "name": "value", "type": "uint256", "indexed": false }
                ],
                "anonymous": false
            }
        },
        "audit_types": ["quota"],
        "condition": { "==": [ { "var": "transfer.address" }, hex_addr(NATIVE_ASSET_ADDRESS) ] },
        "action": "audit"
    }))
    .expect("the audit rule deserializes");
    let deny: RawRule = serde_json::from_value(serde_json::json!({
        "id": "impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "p": hex_addr(P) } },
        "condition": { "!": { "in": [
            { "var": "p.delegateCodeHash" },
            [ hex_hash(code_hash(&call_value_one_to(DST))) ]
        ] } },
        "action": "deny"
    }))
    .expect("the whitelist rule deserializes");
    let rules = load_rules(MIN_PROTOCOL_VERSION, 1, vec![audit, deny]);
    assert!(rules.rejected.is_empty(), "the fixture rules must load cleanly: {:?}", rules.rejected);
    rules
}

fn proxy_db(implementation: Address, impl_code: Vec<u8>) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(P, account(10, Some(proxy_reading_slot_zero())));
    db.insert_account_storage(P, U256::ZERO, implementation.into_word().into())
        .expect("the proxy's slot is writable");
    db.insert_account_info(implementation, account(10, Some(impl_code)));
    db.insert_account_info(DST, account(0, None));
    db
}

fn plain_db(plain_code: Vec<u8>) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(PLAIN, account(10, Some(plain_code)));
    db.insert_account_info(DST, account(0, None));
    db
}

/// Captures one candidate under the pinned snapshot's selector set and stamp, exactly as the
/// block builder does.
fn capture(
    db: InMemoryDB,
    target: Address,
    snapshot: &RuleSet,
) -> (Vec<Log>, Vec<CallObservation>) {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::with_callstack(
            1_000,
            16,
            CallstackCaptureConfig::new(snapshot.stamp(), snapshot.callstack_selectors().to_vec()),
        ),
    );
    evm.components_mut().1.start_capture();
    let result = evm.transact(call_tx(target)).expect("tx executes");
    assert!(result.result.is_success(), "the fixture transaction succeeds");
    match evm.components_mut().1.finish_capture(result.result.logs()) {
        CaptureOutcome::Complete { logs, observations } => (logs, observations.items),
        other => panic!("expected a completed capture, got {other:?}"),
    }
}

fn screen_input<'a>(
    target: Address,
    logs: &'a [Log],
    items: &'a [CallObservation],
    stamp: SnapshotStamp,
) -> ScreenInput<'a> {
    ScreenInput {
        tx_hash: TX,
        origin: caller(),
        tx_to: Some(target),
        nonce: 0,
        value: U256::ZERO,
        block_height: 1,
        logs,
        observations: Observations::available(stamp, items),
    }
}

fn handle_with(rules: RuleSet) -> (FilterHandle, Arc<TestClock>) {
    let clock = Arc::new(TestClock::new(START));
    (FilterHandle::for_test(FilterConfig::default(), rules, clock.clone()), clock)
}

/// Drives the exact cumulative-timeout state-machine call the timeout worker performs.
fn expire(handle: &FilterHandle) {
    handle.with_pool(|pool| {
        let resolved = pool.check_timeouts(&FilterConfig::default(), START + 91);
        assert_eq!(resolved.len(), 1, "the cumulative deadline resolves the entry");
    });
    assert_eq!(handle.buffer_status(&TX), Some(BufferStatus::TimedOutAllow));
}

/// E1 — the proxy upgraded to a non-whitelisted implementation between executions; the timed-out
/// transaction must be dropped by the re-check instead of released.
#[test]
fn timed_out_allow_drops_when_the_upgraded_implementation_is_not_whitelisted() {
    let (handle, _clock) = handle_with(rules());
    let pinned = handle.rule_snapshot();
    let (logs, items) = capture(proxy_db(I_OK, call_value_one_to(DST)), P, &pinned);
    let input = screen_input(P, &logs, &items, pinned.stamp());
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::AuditPending);
    assert_eq!(handle.buffer_status(&TX), Some(BufferStatus::NotSubmitted));

    expire(&handle);

    let saved = handle.saved_rule_snapshot(&TX).expect("the buffered entry keeps its snapshot");
    let (logs, items) = capture(proxy_db(I_ROGUE, rogue_variant(DST)), P, &saved);
    let input = screen_input(P, &logs, &items, saved.stamp());
    assert_eq!(handle.screen_tx_with_snapshot(&input, &saved), Screen::Drop);
    assert_eq!(handle.buffer_status(&TX), Some(BufferStatus::Dropped));
}

/// E2 — the implementation is still the whitelisted one, so the timed-out transaction is released.
#[test]
fn timed_out_allow_releases_when_the_implementation_is_still_whitelisted() {
    let (handle, _clock) = handle_with(rules());
    let pinned = handle.rule_snapshot();
    let (logs, items) = capture(proxy_db(I_OK, call_value_one_to(DST)), P, &pinned);
    let input = screen_input(P, &logs, &items, pinned.stamp());
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::AuditPending);

    expire(&handle);

    let (logs, items) = capture(proxy_db(I_OK, call_value_one_to(DST)), P, &pinned);
    let input = screen_input(P, &logs, &items, pinned.stamp());
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::AuditApproved);
    assert_eq!(handle.buffer_status(&TX), Some(BufferStatus::TimedOutAllow));
}

/// E3 — a plain quota transaction with no proxy keeps the ordinary fail-open release.
#[test]
fn timed_out_allow_releases_a_plain_quota_transaction() {
    let (handle, _clock) = handle_with(rules());
    let pinned = handle.rule_snapshot();
    let (logs, items) = capture(plain_db(call_value_one_to(DST)), PLAIN, &pinned);
    let input = screen_input(PLAIN, &logs, &items, pinned.stamp());
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::AuditPending);

    expire(&handle);

    let (logs, items) = capture(plain_db(call_value_one_to(DST)), PLAIN, &pinned);
    let input = screen_input(PLAIN, &logs, &items, pinned.stamp());
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::AuditApproved);
}

/// E4 — the approved path is unchanged: an approval is still checked against the current
/// execution, and the upgraded implementation is dropped there too.
#[test]
fn approved_path_drops_the_upgraded_implementation() {
    let (handle, _clock) = handle_with(rules());
    let pinned = handle.rule_snapshot();
    let (logs, items) = capture(proxy_db(I_OK, call_value_one_to(DST)), P, &pinned);
    let input = screen_input(P, &logs, &items, pinned.stamp());
    assert_eq!(handle.screen_tx_with_snapshot(&input, &pinned), Screen::AuditPending);

    handle.advance_to_approved_for_test(&TX);
    assert_eq!(handle.buffer_status(&TX), Some(BufferStatus::Approved));

    let saved = handle.saved_rule_snapshot(&TX).expect("the approved entry keeps its snapshot");
    let (logs, items) = capture(proxy_db(I_ROGUE, rogue_variant(DST)), P, &saved);
    let input = screen_input(P, &logs, &items, saved.stamp());
    assert_eq!(handle.screen_tx_with_snapshot(&input, &saved), Screen::Drop);
    assert_eq!(handle.buffer_status(&TX), Some(BufferStatus::Dropped));
}
