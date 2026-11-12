//! Real capture stream fed into the filter's `screen_tx` / `evaluate` path.

use super::*;
use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Signature, TxKind};
use op_revm::OpSpecId;
use rcs_filter::callstack::Observations;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;
// ── screen_tx integration tests ───────────────────────────────────────────
// These tests run a real OpEvm transaction, capture the native transfer
// stream, then feed it directly into FilterHandle::screen_tx, proving
// the full pipeline from EVM execution through the rule engine.

use rcs_filter::rules::{load_rules, RawRule};
use rcs_filter::test_support::TestClock;
use rcs_filter::{BufferStatus, FilterConfig, FilterHandle, Screen, ScreenInput};
use std::sync::Arc;

const DENY_NATIVE_RULE: &str = r#"{
        "id": "deny-native",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"transfer.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "action": "deny"
    }"#;

const AUDIT_NATIVE_RULE: &str = r#"{
        "id": "audit-1",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"transfer.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "audit_types": ["type_a"],
        "action": "audit"
    }"#;

const AUDIT_NATIVE_RULE_2: &str = r#"{
        "id": "audit-2",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"transfer.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "audit_types": ["type_b"],
        "action": "audit"
    }"#;

const DUAL_ALIAS_DENY_NATIVE_RULE: &str = r#"{
        "id": "dual-alias-deny",
        "event_abis": {
            "t1": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            },
            "t2": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"t1.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "action": "deny"
    }"#;

/// Same Transfer ABI as the rule above, but the condition is on the transaction-level `value`
/// binding rather than on any event field. Deployed rules may still be written against that
/// legacy binding, so it must keep working now that a virtual `transfer.*` log set exists
/// alongside it.
const LEGACY_VALUE_DENY_RULE: &str = r#"{
        "id": "legacy-value-deny",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"value"},"1"]},
        "action": "deny"
    }"#;

/// A second audit rule carrying the SAME `audit_type` as [`AUDIT_NATIVE_RULE`]. Two same-type
/// rules matching the same physical log must collapse to one action, which is what distinguishes
/// a genuine `(audit_type, log_index)` dedup from an implicit "one rule, one action" placement.
const AUDIT_NATIVE_RULE_SAME_TYPE: &str = r#"{
        "id": "audit-same-type",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"transfer.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "audit_types": ["type_a"],
        "action": "audit"
    }"#;

/// An audit rule whose condition is satisfied by transaction-level data alone, so it also holds
/// when the declared event matches NO physical log. That is the all-null binding case: the rule
/// is satisfied, yet no Audit action may be emitted because there is no physical event behind it.
const AUDIT_NULL_BINDING_RULE: &str = r#"{
        "id": "audit-null-binding",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"value"},"0"]},
        "audit_types": ["type_null"],
        "action": "audit"
    }"#;

/// Dual-alias rule over two aliases of the SAME Transfer ABI, whose condition never fires. It
/// isolates the matcher-binding budget from the rule's own verdict: any Deny on the larger
/// stream can then only come from the binding counter, never from the rule matching.
const DUAL_ALIAS_BUDGET_RULE: &str = r#"{
        "id": "dual-alias-budget",
        "event_abis": {
            "t1": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            },
            "t2": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"t1.address"},"0x000000000000000000000000000000000000dead"]},
        "action": "deny"
    }"#;

fn deny_native_handle() -> FilterHandle {
    let raw: RawRule = serde_json::from_str(DENY_NATIVE_RULE).unwrap();
    let rules = load_rules(1, 1, vec![raw]);
    assert_eq!(rules.rules.len(), 1, "deny-native rule must compile cleanly");
    FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(TestClock::new(1_751_000_000)))
}

fn audit_native_handle() -> FilterHandle {
    let raw: RawRule = serde_json::from_str(AUDIT_NATIVE_RULE).unwrap();
    let rules = load_rules(1, 1, vec![raw]);
    assert_eq!(rules.rules.len(), 1, "audit-1 rule must compile cleanly");
    FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(TestClock::new(1_751_000_000)))
}

fn run_capturing_call_tx_with_value(
    db: InMemoryDB,
    from: Address,
    target: Address,
    value: U256,
    budget: usize,
) -> (CaptureOutcome, bool) {
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        db,
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        ),
        RcsInspector::new(budget),
    );
    evm.components_mut().1.start_capture();
    let tx = TxLegacy {
        nonce: 0,
        gas_limit: 2_000_000,
        to: TxKind::Call(target),
        value,
        ..Default::default()
    }
    .into_signed(Signature::new(Default::default(), Default::default(), Default::default()));
    let op_tx = OpTx::from_recovered_tx(&tx, from);
    let result = evm.transact(op_tx).expect("tx executes");
    let success = result.result.is_success();
    let outcome = evm.components_mut().1.finish_capture(result.result.logs());
    (outcome, success)
}

/// Internal CREATE with 7-wei endowment produces a native transfer captured in the real
/// stream; the deny-native rule fires on that log through the full screen_tx path.
#[test]
fn original_nt2_internal_create_denied_through_real_screen_tx() {
    let factory = Address::from([0xF3; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(factory, account(1_000, Some(create_endowment_7())));

    let (outcome, success) = run_capturing_tx(db, factory, 16);
    assert!(success, "internal-CREATE tx must succeed");
    let stream = match outcome {
        CaptureOutcome::Complete { logs: s, .. } => s,
        other => panic!("expected Complete, got {other:?}"),
    };
    assert!(!stream.is_empty(), "CREATE endowment must produce a native transfer");

    let handle = deny_native_handle();
    let input = ScreenInput {
        tx_hash: B256::from([0x11; 32]),
        origin: caller(),
        tx_to: Some(factory),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &stream,
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(handle.screen_tx(&input), Screen::Deny);
}

/// A top-level value CALL (tx.value = 1) produces a single native Transfer as the root
/// frame hook fires; the deny-native rule fires on that real synthetic log.
#[test]
fn top_level_native_denied_through_real_screen_tx() {
    let target = Address::from([0xBB; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));

    let (outcome, success) =
        run_capturing_call_tx_with_value(db, caller(), target, U256::from(1u64), 16);
    assert!(success, "top-level value CALL must succeed");
    let stream = match outcome {
        CaptureOutcome::Complete { logs: s, .. } => s,
        other => panic!("expected Complete, got {other:?}"),
    };
    assert_eq!(stream.len(), 1, "exactly one top-level native transfer expected");
    assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);

    let handle = deny_native_handle();
    let input = ScreenInput {
        tx_hash: B256::from([0x22; 32]),
        origin: caller(),
        tx_to: Some(target),
        nonce: 0,
        value: U256::from(1u64),
        block_height: 0,
        logs: &stream,
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(handle.screen_tx(&input), Screen::Deny);
}

/// A top-level value CALL to a contract that itself does an internal value CALL produces
/// two native Transfers (one per frame); a single deny-native rule fires on the first
/// match, proving the rule works across a mixed root+nested stream.
#[test]
fn one_transfer_rule_denies_both_toplevel_and_internal_real_stream() {
    let inner_dst = Address::from([0xDD; 20]);
    let contract = Address::from([0xCC; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(contract, account(1_000, Some(call_value_1_to(inner_dst))));

    let (outcome, success) =
        run_capturing_call_tx_with_value(db, caller(), contract, U256::from(1u64), 16);
    assert!(success, "tx must succeed");
    let stream = match outcome {
        CaptureOutcome::Complete { logs: s, .. } => s,
        other => panic!("expected Complete, got {other:?}"),
    };
    assert_eq!(stream.len(), 2, "root-frame + internal-frame native transfers expected");

    let handle = deny_native_handle();
    let input = ScreenInput {
        tx_hash: B256::from([0x33; 32]),
        origin: caller(),
        tx_to: Some(contract),
        nonce: 0,
        value: U256::from(1u64),
        block_height: 0,
        logs: &stream,
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(handle.screen_tx(&input), Screen::Deny);
}

/// The capture budget is enforced at the EVM level: budget=2 fits two native transfers
/// (Complete), while budget=1 causes LimitExceeded at the second transfer.
/// Falsifiable: reducing call count or raising budget would change the outcome.
#[test]
fn real_stream_native_budget_limit_plus_one_denies() {
    let a = Address::from([0xE1; 20]);
    let b = Address::from([0xE2; 20]);
    let contract = Address::from([0xE3; 20]);

    // budget=2: both transfers captured → Complete(2)
    let mut db1 = InMemoryDB::default();
    db1.insert_account_info(caller(), account(1_000_000_000, None));
    db1.insert_account_info(contract, account(1_000, Some(call_value_1_twice(a, b))));
    let (outcome1, _) = run_capturing_tx(db1, contract, 2);
    assert!(
        matches!(&outcome1, CaptureOutcome::Complete { logs: s, .. } if s.len() == 2),
        "budget=2 with 2 transfers must produce Complete(2), got {outcome1:?}"
    );

    // budget=1: second transfer overflows → LimitExceeded{attempted:2, limit:1}
    let mut db2 = InMemoryDB::default();
    db2.insert_account_info(caller(), account(1_000_000_000, None));
    db2.insert_account_info(contract, account(1_000, Some(call_value_1_twice(a, b))));
    let (outcome2, _) = run_capturing_tx(db2, contract, 1);
    assert!(
        matches!(
            outcome2,
            CaptureOutcome::LimitExceeded {
                attempted: 2,
                limit: 1,
                budget: ObservationBudget::NativeTransfer,
            }
        ),
        "budget=1 with 2 transfers must produce LimitExceeded{{2,1}}, got {outcome2:?}"
    );
}

/// AuditPending → advance to Approved → AuditApproved (same logs); then empty-log
/// re-screen → inconsistent → Drop.  Exercises the full audit lifecycle end-to-end.
#[test]
fn real_stream_screen_tx_and_screen_existing_consistent() {
    let dst = Address::from([0xF4; 20]);
    let contract = Address::from([0xF5; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(contract, account(1_000, Some(call_value_1_to(dst))));

    let (outcome, success) = run_capturing_tx(db, contract, 16);
    assert!(success);
    let stream = match outcome {
        CaptureOutcome::Complete { logs: s, .. } => s,
        other => panic!("expected Complete, got {other:?}"),
    };
    assert_eq!(stream.len(), 1, "one native transfer expected");

    let handle = audit_native_handle();
    let tx_hash = B256::from([0x55; 32]);

    let input = ScreenInput {
        tx_hash,
        origin: caller(),
        tx_to: Some(contract),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &stream,
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(handle.screen_tx(&input), Screen::AuditPending);

    handle.advance_to_approved_for_test(&tx_hash);

    assert_eq!(handle.screen_tx(&input), Screen::AuditApproved, "same logs → consistent");

    let empty_input = ScreenInput {
        tx_hash,
        origin: caller(),
        tx_to: Some(contract),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &[],
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(handle.screen_tx(&empty_input), Screen::Drop, "empty logs → inconsistent → Drop");
}

/// A real stream containing one native Transfer + one real LOG0 (superset of the rule's
/// expected shape), screened by a rule whose condition reads the LEGACY transaction-level
/// `value` binding rather than any event field. That binding must keep working unchanged now
/// that the virtual `transfer.*` log set exists alongside it.
#[test]
fn legacy_value_rule_still_fires_on_real_capture_superset_stream() {
    let dst = Address::from([0xC6; 20]);
    let contract = Address::from([0xB6; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    db.insert_account_info(contract, account(1_000, Some(call_value_1_then_log0(dst))));

    let (outcome, success) = run_capturing_tx(db, contract, 16);
    assert!(success);
    let stream = match outcome {
        CaptureOutcome::Complete { logs: s, .. } => s,
        other => panic!("expected Complete, got {other:?}"),
    };
    assert_eq!(stream.len(), 2, "one native transfer + one real log expected");
    assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS, "first log must be native transfer");
    assert_ne!(stream[1].address, NATIVE_ASSET_ADDRESS, "second log must be the real log");

    let raw: RawRule = serde_json::from_str(LEGACY_VALUE_DENY_RULE).unwrap();
    let rules = load_rules(1, 1, vec![raw]);
    assert_eq!(rules.rules.len(), 1, "legacy-value-deny rule must compile cleanly");

    // Fires: the rule's condition is on the transaction-level `value` binding, which is `1`.
    let firing = FilterHandle::for_test(
        FilterConfig::default(),
        rules.clone(),
        Arc::new(TestClock::new(1_751_000_000)),
    );
    let firing_input = ScreenInput {
        tx_hash: B256::from([0x66; 32]),
        origin: caller(),
        tx_to: Some(contract),
        nonce: 0,
        value: U256::from(1u64),
        block_height: 0,
        logs: &stream,
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(firing.screen_tx(&firing_input), Screen::Deny);

    // Negative control: the SAME rules and the SAME captured stream, with only the
    // transaction-level `value` changed to zero, must NOT fire. This proves the rule matched on
    // the legacy tx-level binding — not on the event's own `transfer.value` (which is 1 wei in
    // the captured log and is untouched by this change).
    let controlled = FilterHandle::for_test(
        FilterConfig::default(),
        rules,
        Arc::new(TestClock::new(1_751_000_000)),
    );
    let controlled_input = ScreenInput { value: U256::ZERO, ..firing_input };
    assert_eq!(
        controlled.screen_tx(&controlled_input),
        Screen::Allow,
        "with tx-level value 0 the legacy rule must not fire, so the Deny above is the \
             transaction-level `value` binding and not the event's transfer.value"
    );
}

/// Two real streams of different widths compared under one dual-alias rule (t1 × t2 over the
/// SAME Transfer ABI), so the binding count grows quadratically while the rule's own verdict
/// stays fixed. The within-budget stream and the over-budget stream are then distinguishable
/// controls rather than a single Deny that a rule which denies by itself could also produce.
///
/// `dual_alias_budget_handle` admits `max_event_bindings_per_tx` so the same stream can be
/// screened either side of the boundary.
fn dual_alias_budget_handle(max_event_bindings_per_tx: usize) -> FilterHandle {
    let raw: RawRule = serde_json::from_str(DUAL_ALIAS_BUDGET_RULE).unwrap();
    let rules = load_rules(1, 1, vec![raw]);
    assert_eq!(rules.rules.len(), 1, "dual-alias-budget rule must compile cleanly");
    FilterHandle::for_test(
        FilterConfig { max_event_bindings_per_tx, ..Default::default() },
        rules,
        Arc::new(TestClock::new(1_751_000_000)),
    )
}

#[test]
fn dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream() {
    let a = Address::from([0xE4; 20]);
    let b = Address::from([0xE5; 20]);
    let contract = Address::from([0xE6; 20]);

    // Stream-A: ONE native transfer → t1 × t2 = 1 × 1 = 1 binding.
    let mut db_a = InMemoryDB::default();
    db_a.insert_account_info(caller(), account(1_000_000_000, None));
    let (outcome_a, _) = run_capturing_call_tx_with_value(
        db_a,
        caller(),
        Address::from([0xE7; 20]),
        U256::from(1u64),
        16,
    );
    let stream_a = match outcome_a {
        CaptureOutcome::Complete { logs: s, .. } => s,
        other => panic!("expected Complete for stream-A, got {other:?}"),
    };
    assert_eq!(stream_a.len(), 1, "stream-A must carry exactly one native transfer");

    // Stream-B: TWO native transfers → t1 × t2 = 2 × 2 = 4 bindings.
    let mut db_b = InMemoryDB::default();
    db_b.insert_account_info(caller(), account(1_000_000_000, None));
    db_b.insert_account_info(contract, account(1_000, Some(call_value_1_twice(a, b))));
    let (outcome_b, _) = run_capturing_tx(db_b, contract, 16);
    let stream_b = match outcome_b {
        CaptureOutcome::Complete { logs: s, .. } => s,
        other => panic!("expected Complete for stream-B, got {other:?}"),
    };
    assert_eq!(stream_b.len(), 2, "stream-B must carry exactly two native transfers");

    let input_a = ScreenInput {
        tx_hash: B256::from([0x77; 32]),
        origin: caller(),
        tx_to: Some(contract),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &stream_a,
        observations: Observations::NOT_CAPTURED,
    };
    let input_b = ScreenInput {
        tx_hash: B256::from([0x78; 32]),
        origin: caller(),
        tx_to: Some(contract),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &stream_b,
        observations: Observations::NOT_CAPTURED,
    };

    // Within budget: 1 binding, and the rule's condition is false → Allow. This is the
    // budget-not-exceeded side of the comparison.
    assert_eq!(
        dual_alias_budget_handle(3).screen_tx(&input_a),
        Screen::Allow,
        "stream-A stays within the binding budget and must not be denied"
    );

    // limit-th binding count (4) with budget 3 → the (limit+1)-th binding is refused, so the
    // rule never even completes and the transaction fails closed with Deny.
    assert_eq!(
        dual_alias_budget_handle(3).screen_tx(&input_b),
        Screen::Deny,
        "stream-B exceeds the binding budget and must fail closed"
    );

    // Boundary control: raise the budget to exactly stream-B's binding count. The SAME stream
    // and the SAME rule now complete and, since the condition is false, Allow — proving the
    // Deny above came from the binding counter at the boundary, not from the rule's verdict.
    assert_eq!(
        dual_alias_budget_handle(4).screen_tx(&input_b),
        Screen::Allow,
        "at the exact binding count the same stream is admitted, so the Deny was the budget"
    );

    // The rule itself still denies when it genuinely matches, so the Allow/Deny split above is
    // attributable to the budget and not to a rule that can never deny.
    let deny_raw: RawRule = serde_json::from_str(DUAL_ALIAS_DENY_NATIVE_RULE).unwrap();
    let deny_rules = load_rules(1, 1, vec![deny_raw]);
    assert_eq!(deny_rules.rules.len(), 1, "dual-alias-deny rule must compile cleanly");
    let deny_handle = FilterHandle::for_test(
        FilterConfig { max_event_bindings_per_tx: 4, ..Default::default() },
        deny_rules,
        Arc::new(TestClock::new(1_751_000_000)),
    );
    assert_eq!(
        deny_handle.screen_tx(&input_b),
        Screen::Deny,
        "the same two-alias shape denies on its own verdict once the budget admits it"
    );
}

/// `log.address` in the lower-cased hex form the action payload carries.
fn lower_hex(a: Address) -> String {
    format!("{a:#x}")
}

/// Audit-rule merging on a real captured stream, asserted on the CONTENT that reaches the buffer
/// rather than on the screen verdict alone. Three properties are pinned, each with its own
/// control:
/// - (a) two rules declaring the SAME `audit_type` and matching the SAME physical log collapse
///   to ONE action, keyed by `(audit_type, log_index)`, carrying the decoded `transfer` inputs;
/// - (b) a rule declaring a DIFFERENT `audit_type` is retained as its own action — the
///   `audit_type` is the key of the `actions` map, not a field of the action item;
/// - (c) an all-null binding (a satisfied audit rule with no physical event behind it) produces
///   NO action at all, proven against the same rule firing when a physical event does back it.
#[test]
fn audit_type_action_merge_action_content_no_empty_audit_on_real_stream() {
    let target = Address::from([0xB8; 20]);
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller(), account(1_000_000_000, None));
    let (outcome, success) =
        run_capturing_call_tx_with_value(db, caller(), target, U256::from(1u64), 16);
    assert!(success, "top-level value CALL must succeed");
    let stream = match outcome {
        CaptureOutcome::Complete { logs: s, .. } => s,
        other => panic!("expected Complete, got {other:?}"),
    };
    assert_eq!(stream.len(), 1, "exactly one native transfer expected");
    assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);

    // ---- (a) + (b): same-type dedup, different-type retention, action content ----------------
    let raw1: RawRule = serde_json::from_str(AUDIT_NATIVE_RULE).unwrap(); // audit_type "type_a"
    let raw2: RawRule = serde_json::from_str(AUDIT_NATIVE_RULE_SAME_TYPE).unwrap(); // "type_a"
    let raw3: RawRule = serde_json::from_str(AUDIT_NATIVE_RULE_2).unwrap(); // audit_type "type_b"
    let rules = load_rules(1, 1, vec![raw1, raw2, raw3]);
    assert_eq!(rules.rules.len(), 3, "all three audit rules must compile cleanly");
    let handle = FilterHandle::for_test(
        FilterConfig::default(),
        rules,
        Arc::new(TestClock::new(1_751_000_000)),
    );
    let merged_tx = B256::from([0x88; 32]);
    let merged_input = ScreenInput {
        tx_hash: merged_tx,
        origin: caller(),
        tx_to: Some(target),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &stream,
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(
        handle.screen_tx(&merged_input),
        Screen::AuditPending,
        "merged audit rules → AuditPending"
    );
    assert_eq!(
        handle.buffer_status(&merged_tx),
        Some(BufferStatus::NotSubmitted),
        "pool entry must be NotSubmitted after first screen"
    );

    let actions = handle
        .with_pool(|pool| pool.get(&merged_tx).map(|entry| entry.actions.clone()))
        .expect("the audited transaction must have a buffered entry");
    // (b) `audit_type` is the KEY of the actions map, so the two distinct types are two keys.
    let keys: Vec<&str> = actions.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["type_a", "type_b"], "audit_type is the actions-map key");

    // (a) Two rules with the same audit_type matching the same log dedup to ONE action.
    let type_a = actions.get("type_a").expect("type_a must be present");
    assert_eq!(
        type_a.len(),
        1,
        "same (audit_type, log_index) must collapse to a single action, got {type_a:?}"
    );
    let type_b = actions.get("type_b").expect("type_b must be present");
    assert_eq!(type_b.len(), 1, "the distinct audit_type keeps its own single action");

    // ...and the retained action carries the decoded transfer content, not an empty shell.
    let item = &type_a[0];
    assert_eq!(item.name, "transfer", "the action names the rule-local event alias");
    assert_eq!(
        item.address,
        lower_hex(NATIVE_ASSET_ADDRESS),
        "the action address is the triggering log's address"
    );
    let expected_from = lower_hex(caller());
    let expected_to = lower_hex(target);
    assert_eq!(item.params.get("from").and_then(|v| v.as_str()), Some(expected_from.as_str()));
    assert_eq!(item.params.get("to").and_then(|v| v.as_str()), Some(expected_to.as_str()));
    assert_eq!(item.params.get("value").and_then(|v| v.as_str()), Some("1"));

    // ---- (c) negative control: a satisfied audit rule with NO physical event ---------------
    // The condition is on the transaction-level `value`, so it holds whether or not the event
    // matched; what must differ is the presence of a physical log behind the binding.
    let null_raw: RawRule = serde_json::from_str(AUDIT_NULL_BINDING_RULE).unwrap();
    let null_rules = load_rules(1, 1, vec![null_raw]);
    assert_eq!(null_rules.rules.len(), 1, "audit-null-binding rule must compile cleanly");
    let null_handle = FilterHandle::for_test(
        FilterConfig::default(),
        null_rules,
        Arc::new(TestClock::new(1_751_000_000)),
    );

    let empty_tx = B256::from([0x89; 32]);
    let empty_input = ScreenInput {
        tx_hash: empty_tx,
        origin: caller(),
        tx_to: Some(target),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &[],
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(
        null_handle.screen_tx(&empty_input),
        Screen::Allow,
        "an all-null binding must not produce an Audit action"
    );
    assert_eq!(
        null_handle.buffer_status(&empty_tx),
        None,
        "no buffered entry may exist for an all-null binding (no empty Audit submitted)"
    );
    assert_eq!(null_handle.buffered_len(), 0, "the buffer must stay empty");
    assert!(
        null_handle.with_pool(|pool| pool.get(&empty_tx).is_none()),
        "no action payload may be stored for an all-null binding"
    );

    // Positive counterpart on the SAME rule: with the real stream the event binds physically,
    // so the very same satisfied condition now yields the action. This is what makes the
    // Allow above evidence about the physical-event guard rather than about the condition.
    let backed_tx = B256::from([0x8A; 32]);
    let backed_input = ScreenInput {
        tx_hash: backed_tx,
        origin: caller(),
        tx_to: Some(target),
        nonce: 0,
        value: U256::ZERO,
        block_height: 0,
        logs: &stream,
        observations: Observations::NOT_CAPTURED,
    };
    assert_eq!(
        null_handle.screen_tx(&backed_input),
        Screen::AuditPending,
        "the same rule must audit once a physical event backs the binding"
    );
    let backed_actions = null_handle
        .with_pool(|pool| pool.get(&backed_tx).map(|entry| entry.actions.clone()))
        .expect("the backed transaction must have a buffered entry");
    assert_eq!(
        backed_actions.get("type_null").map(Vec::len),
        Some(1),
        "the backed binding produces exactly one type_null action"
    );
}
