//! Real capture and a real verdict on `xlayer_auditTransactions`: capture / verdict / commit
//! semantics, the per-transaction native-transfer and callstack budgets, ordered batches and the
//! `NativeAndCallstack` mode's real + virtual log surface.

use alloy_primitives::{keccak256, Address, B256, U256};
use rcs_filter::rules::{load_rules, RawRule};
use revm::database::InMemoryDB;
use revm::state::{AccountInfo, Bytecode};
use revm::Database as _;
use xlayer_rpc::audit::rpc::observation_recorder;
use xlayer_rpc::audit::rpc::test_seams::execute_audit_batch;
use xlayer_rpc::audit::rpc::{
    AuditTransactionsRequest, AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX,
    AUDIT_MAX_NATIVE_TRANSFERS_PER_TX,
};
use xlayer_rpc::audit::verdict::Verdict;

use crate::harness::*;

/// A second proxy the request's rules do not observe.
const PROXY_B: Address = Address::repeat_byte(0xAB);

/// The contract that emits the `LOG0`s the event-binding fixture binds.
const LOG_TARGET: Address = Address::repeat_byte(0xDD);

// ---------------------------------------------------------------- batch semantics ---------

/// a `callstack` rule is accepted and classified from REAL capture: the
/// deposit→proxy→unapproved-implementation request returns the deny verdict.
#[test]
fn a_callstack_rule_denies_unapproved_implementation() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[],
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Deny);
    assert_eq!(run.proxy_slot0, U256::ZERO, "a denied transaction's state is not committed");
}

/// contrast: the same request against the APPROVED implementation returns allow, and
/// its state IS committed. Without this, the deny case above could pass for the wrong reason.
#[test]
fn same_request_with_approved_implementation_allows() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[],
        seeded_db(APPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::from(1u64), "an allowed transaction commits its state");
}

/// third direction of the same sample: an ORDINARY (non-delegatecall) call to the observed
/// proxy must ALSO be allowed. The frame is genuinely observed, but a plain call carries no
/// implementation, so the implementation code hash binds to null and the explicit non-null
/// guard stops the deny branch from being reached. Without this direction the pattern could
/// deny every ordinary caller of the proxy and both other directions would still pass.
#[test]
fn ordinary_call_to_the_observed_proxy_is_allowed() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[],
        // The proxy's own code performs NO delegatecall here: it is a bare `STOP`.
        seeded_db(UNAPPROVED_IMPL, 0),
    );
    let result = only(&run);
    assert_eq!(
        result.verdict,
        Verdict::Allow,
        "a plain call to the proxy has no implementation to compare and must not be denied"
    );
}

/// An unadjudicable capture on an ordinary deposit still gets the fail-closed non-verdict with
/// nothing committed — both a budget-exhausted capture and a foreign-stamped one — so the gate
/// keeps failing closed now that it is driven by the capture outcome rather than by selector
/// presence alone.
#[test]
fn the_unadjudicable_gate_catches_an_ordinary_entry() {
    let source = format!("0x{}", "66".repeat(32));

    // Unavailable: the observation budget is exhausted.
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[],
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed, "never Allow");
    assert_eq!(run.proxy_slot0, U256::ZERO, "state must not be committed");

    // Available, but produced under a different snapshot identity.
    let raw_rules = vec![proxy_impl_allowlist_rule()];
    let run = run_batch_stamped(
        &[deposit_request(&source, PROXY)],
        raw_rules.clone(),
        &[],
        seeded_db(APPROVED_IMPL, 1),
        Some(foreign_stamp(&raw_rules)),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed, "never Allow");
    assert_eq!(run.proxy_slot0, U256::ZERO, "state must not be committed");
}

/// An empty rule set constructs no inspector and screens the plain EVM's raw execution logs, so
/// the response keeps its exact four-field shape and a `known_allowed` entry still commits as
/// before.
#[test]
fn selector_free_request_is_unchanged() {
    let source = format!("0x{}", "33".repeat(32));
    let rules = load_rules(1, 0, vec![]);
    assert!(
        rcs_filter::audit::audit_inspector(
            &rules,
            AUDIT_MAX_NATIVE_TRANSFERS_PER_TX,
            AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX
        )
        .is_none(),
        "an empty rule set must construct NO inspector"
    );

    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![],
        &[&source],
        seeded_db(APPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    let json = serde_json::to_value(result).expect("serializes");
    let object = json.as_object().expect("a result is a JSON object");
    assert_eq!(
        object.len(),
        4,
        "the wire shape is exactly tx_hash / source_hash / verdict / actions: {json}"
    );
    assert_eq!(run.proxy_slot0, U256::from(1u64), "known_allowed still commits as before");
}

/// A rules-bearing **event-only** request whose transaction performs a real internal native
/// transfer is denied by a rule keyed on the native-asset `Transfer` address. This is the direct
/// consequence of this path running in `NativeAndCallstack` mode and screening the capture's own
/// ordered real + virtual logs: the internal `CALL` synthesizes a virtual `Transfer` on the
/// native-asset identifier, and the rule below is written to deny exactly that.
#[test]
fn a_virtual_native_log_enters_the_screened_stream() {
    let source = format!("0x{}", "33".repeat(32));

    // Event-only request (no callstack selector at all) over a transaction that really performs
    // an internal native transfer: the virtual native log must reach the matcher and deny.
    let denied = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![native_transfer_deny_rule()],
        &[],
        value_transferring_db(),
    );
    assert_eq!(
        only(&denied).verdict,
        Verdict::Deny,
        "the internal native transfer's virtual log must enter the screened stream"
    );
    assert_eq!(denied.depositor_nonce, 0, "a denied transaction commits nothing");

    // Paired control: the same transaction against the same event declared on a DIFFERENT address
    // still allows, proving the deny came from the native virtual log, not from a real log. The
    // control address must differ from the native-asset identifier byte for byte.
    let other_address = Address::repeat_byte(0x11);
    assert_ne!(other_address, rcs_filter::capture::NATIVE_ASSET_ADDRESS);
    let allowed = run_batch(
        &[deposit_request(&format!("0x{}", "44".repeat(32)), PROXY)],
        vec![native_transfer_deny_rule_on(other_address)],
        &[],
        value_transferring_db(),
    );
    assert_eq!(
        only(&allowed).verdict,
        Verdict::Allow,
        "the deny is bound to the native-asset address, not to any real log"
    );
    assert_eq!(allowed.depositor_nonce, 1, "the control transaction commits its state");
}

/// An event-only request whose transaction performs NO internal native transfer still allows and
/// commits — guarding against a gate that fails closed on every event-only request.
#[test]
fn an_event_only_request_without_a_native_transfer_still_allows() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![native_transfer_deny_rule()],
        &[],
        // The proxy's own code is a bare STOP: no internal call, hence no virtual native log.
        seeded_db(APPROVED_IMPL, 0),
    );
    assert_eq!(only(&run).verdict, Verdict::Allow);
    assert_eq!(run.depositor_nonce, 1, "an admissible event-only request still commits");
}

/// A `known_allowed` entry keeps its unconditional bypass even with a native-transfer deny rule
/// armed: it is allowed, committed, and carries no actions.
#[test]
fn known_allowed_entry_with_a_native_deny_rule_is_allowed_and_committed() {
    let source = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![native_transfer_deny_rule()],
        &[&source],
        value_transferring_db(),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow, "known_allowed ignores its capture outcome");
    assert!(result.actions.is_none(), "known_allowed carries no actions");
    assert_eq!(run.depositor_nonce, 1, "the known_allowed commit is never withheld");
}

/// A native-transfer budget overflow is unadjudicable even though the request declares no
/// callstack rule: two real non-zero internal `CALL`s against a budget of one is genuinely over
/// the boundary (`observed 2 > limit 1`), so the transaction is `malformed` and commits nothing.
#[test]
fn a_native_budget_overflow_fails_closed_even_for_an_event_only_request() {
    let source = format!("0x{}", "77".repeat(32));
    let run = run_batch_with_native_budget(
        &[deposit_request(&source, PROXY)],
        vec![native_transfer_deny_rule()],
        &[],
        value_transferring_twice_db(),
        1,
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed, "a truncated capture never allows");
    assert_eq!(run.depositor_nonce, 0, "an over-budget transaction commits nothing");
}

/// The boundary is "over", not "at": the same two-`CALL` fixture completes at a budget of two and
/// is adjudicated (the native rule denies it), while a single-`CALL` fixture overflows a budget
/// of zero.
#[test]
fn a_native_capture_exactly_at_the_budget_is_adjudicated() {
    let source = format!("0x{}", "78".repeat(32));
    let at_limit = run_batch_with_native_budget(
        &[deposit_request(&source, PROXY)],
        vec![native_transfer_deny_rule()],
        &[],
        value_transferring_twice_db(),
        2,
    );
    assert_eq!(
        only(&at_limit).verdict,
        Verdict::Deny,
        "two observations at a budget of two complete and are adjudicated"
    );
    assert_eq!(at_limit.depositor_nonce, 0, "the denied transaction commits nothing");

    let over = run_batch_with_native_budget(
        &[deposit_request(&format!("0x{}", "79".repeat(32)), PROXY)],
        vec![native_transfer_deny_rule()],
        &[],
        value_transferring_db(),
        0,
    );
    assert_eq!(
        only(&over).verdict,
        Verdict::Malformed,
        "one observation over a budget of zero is unadjudicable"
    );
}

/// Event and callstack rules are both reachable on the same execution: the event rule audits the
/// emitted proxy `Transfer`, and the callstack rule's denial still fires, with the recorded
/// observation set naming the proxy delegatecall.
#[test]
fn event_and_callstack_rules_both_reach_the_matcher() {
    let source = format!("0x{}", "33".repeat(32));

    // Event dimension alone: the proxy→logging-impl Transfer drives Audit.
    let event_only = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![audit_on_proxy_transfer_rule()],
        &[],
        logging_db(),
    );
    assert_eq!(only(&event_only).verdict, Verdict::Audit, "the event rule reaches the matcher");

    // Both dimensions: the callstack deny short-circuits the event audit.
    let _ = observation_recorder::take();
    let mixed = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![audit_on_proxy_transfer_rule(), proxy_impl_allowlist_rule()],
        &[],
        logging_db(),
    );
    let recorded = observation_recorder::take();
    assert_eq!(recorded.len(), 1, "one transaction, one recorded observation set");
    assert!(
        delegatecall_of_proxy(&recorded[0]).is_some(),
        "the proxy delegatecall observation reaches screening: {:?}",
        recorded[0]
    );
    assert_eq!(
        only(&mixed).verdict,
        Verdict::Deny,
        "the callstack denial still fires alongside the event rule"
    );
}

/// Deny on a `Transfer` whose `log.address` is the native-asset identifier — i.e. exactly the
/// virtual log `NativeAndCallstack` capture synthesizes, and nothing a real EVM emits here.
fn native_transfer_deny_rule() -> RawRule {
    native_transfer_deny_rule_on(rcs_filter::capture::NATIVE_ASSET_ADDRESS)
}

/// The same deny shape, keyed on an explicit address — used by the control case to show the deny
/// is bound to the native-asset address rather than to any real log.
fn native_transfer_deny_rule_on(address: Address) -> RawRule {
    serde_json::from_value(serde_json::json!({
        "id": "deny-native-transfer",
        "event_abis": { "transfer": {
            "type": "event", "name": "Transfer",
            "inputs": [
                { "name": "from", "type": "address", "indexed": true },
                { "name": "to", "type": "address", "indexed": true },
                { "name": "value", "type": "uint256", "indexed": false }
            ],
            "anonymous": false
        } },
        "condition": { "==": [
            { "var": "transfer.address" },
            format!("{address:#x}")
        ] },
        "action": "deny"
    }))
    .expect("valid native-log rule")
}

/// A proxy whose own code performs a non-zero internal `CALL` — a real native transfer.
fn value_transferring_db() -> InMemoryDB {
    // retSize, retOffset, argsSize, argsOffset (zero), value = 1, PUSH20 target, GAS, CALL, POP, STOP
    let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0x73];
    code.extend_from_slice(APPROVED_IMPL.as_slice());
    code.extend_from_slice(&[0x5a, 0xf1, 0x50, 0x00]);

    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    db.insert_account_info(
        PROXY,
        AccountInfo {
            balance: U256::from(1_000u64),
            code: Some(Bytecode::new_raw(code.into())),
            ..Default::default()
        },
    );
    db.insert_account_info(
        APPROVED_IMPL,
        AccountInfo { code: Some(Bytecode::new_raw(impl_code(0x01).into())), ..Default::default() },
    );
    db
}

// ----------------------------------------------------- repeated-adjudication fixtures ----

/// an ORDINARY entry (no `known_allowed` at all), running UNAPPROVED code, whose rules are all
/// skipped by origin: the request declares a selector, the execution could not be identified, so
/// it is the non-verdict and its state stays out of the overlay.
#[test]
fn capture_failure_on_an_ordinary_entry_with_skipped_rules_is_the_non_verdict() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
        &[],
        seeded_db(UNAPPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed);
    assert_eq!(
        run.proxy_slot0,
        U256::ZERO,
        "unidentified execution of unapproved code must not be committed"
    );
}

/// repeatedly adjudicating an already-approved entry converges: every round returns the
/// byte-identical serialized result, and adjudication is never re-entered.
///
/// The paired baseline run is what gives the assertion teeth — the same transaction, the same
/// rules and the same state DO reach adjudication while the entry is not yet approved, so the
/// approved rounds are not passing merely because nothing could ever have matched.
#[test]
fn repeated_adjudication_of_an_approved_entry_converges_and_never_re_enters_adjudication() {
    let source = format!("0x{}", "33".repeat(32));
    let approved = keccak256(logging_impl_code());
    let rules =
        vec![proxy_impl_allowlist_rule_approving(&[approved]), audit_on_proxy_transfer_rule()];

    let fresh = run_batch(&[deposit_request(&source, PROXY)], rules.clone(), &[], logging_db());
    assert_eq!(
        only(&fresh).verdict,
        Verdict::Audit,
        "baseline: an unapproved entry really does reach adjudication on this fixture"
    );

    let mut rounds: Vec<String> = Vec::new();
    for round in 0..3 {
        let run =
            run_batch(&[deposit_request(&source, PROXY)], rules.clone(), &[&source], logging_db());
        let result = only(&run);
        assert_eq!(
            result.verdict,
            Verdict::Allow,
            "round {round}: an already-approved entry keeps its allow"
        );
        assert_ne!(
            result.verdict,
            Verdict::Audit,
            "round {round}: adjudication must be unreachable for an approved entry"
        );
        rounds.push(serde_json::to_string(result).expect("the result serializes"));
    }
    assert!(
        rounds.windows(2).all(|pair| pair[0] == pair[1]),
        "the rounds must be byte-identical: {rounds:?}"
    );
}

// ---------------------------------------------------------------- default and boundary --------

#[test]
fn the_audit_budget_is_1000_and_independent_of_the_builder_budget() {
    assert_eq!(AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX, 1_000);
    assert_eq!(
        AUDIT_MAX_NATIVE_TRANSFERS_PER_TX, 10_000,
        "the audit path truncates internal native transfers at the builder's own default"
    );
    assert_eq!(
        rcs_filter::capture::CANDIDATE_MAX_CALLSTACK_OBSERVATIONS_PER_TX,
        10_000,
        "the payload builder's own budget is a separate module constant"
    );
}

#[test]
fn exactly_1000_observations_are_adjudicated_and_committed_under_the_default_budget() {
    let source = format!("0x{}", "33".repeat(32));
    let _ = observation_recorder::take();
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[],
        // 999 first-level delegatecalls: the proxy's own frame plus one per delegatecall is 1 000.
        seeded_db(APPROVED_IMPL, 999),
    );

    let recorded = observation_recorder::take();
    assert_eq!(recorded.len(), 1, "one transaction, one recorded observation set");
    assert_eq!(
        recorded[0].as_ref().map(Vec::len),
        Some(1_000),
        "the default budget admits exactly 1 000 observations"
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::from(1u64), "an allowed transaction commits its state");
    assert_eq!(run.depositor_nonce, 1);
}

#[test]
fn the_1001st_observation_fails_closed_without_committing_under_the_default_budget() {
    let source = format!("0x{}", "33".repeat(32));
    let _ = observation_recorder::take();
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[],
        seeded_db(APPROVED_IMPL, 1_000),
    );

    let recorded = observation_recorder::take();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0], None,
        "an over-budget capture is an unavailable set, never an empty available one"
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed, "never Allow on a capture failure");
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict does not commit state");
    assert_eq!(run.depositor_nonce, 0);
}

#[test]
fn the_default_budget_is_per_transaction_not_pooled_across_the_batch() {
    let first = format!("0x{}", "33".repeat(32));
    let second = format!("0x{}", "44".repeat(32));
    // Two in-budget transactions: 2 000 observations in total, either alone exhausting a pooled
    // allowance of 1 000.
    let run = run_batch(
        &[deposit_request(&first, PROXY), deposit_request(&second, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[],
        seeded_db(APPROVED_IMPL, 999),
    );
    assert_eq!(run.results[0].verdict, Verdict::Allow);
    assert_eq!(run.results[1].verdict, Verdict::Allow);
    assert_eq!(run.depositor_nonce, 2, "each transaction had its own allowance");

    // An over-budget transaction followed by an in-budget one: the second is captured and
    // adjudicated independently of the first.
    let over = format!("0x{}", "55".repeat(32));
    let under = format!("0x{}", "66".repeat(32));
    let mut db = seeded_db(APPROVED_IMPL, 1_000);
    db.insert_account_info(
        PROXY_B,
        AccountInfo {
            code: Some(Bytecode::new_raw(delegating_proxy_code(APPROVED_IMPL, 0).into())),
            ..Default::default()
        },
    );
    let run = run_batch(
        &[deposit_request(&over, PROXY), deposit_request(&under, PROXY_B)],
        vec![proxy_impl_allowlist_rule()],
        &[],
        db,
    );
    assert_eq!(run.results[0].verdict, Verdict::Malformed);
    assert_eq!(run.results[1].verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::ZERO, "the over-budget transaction committed nothing");
    assert_eq!(run.depositor_nonce, 1, "only the second transaction committed");
}

/// A `known_allowed` entry is never checked for codeHash: even an over-budget capture leaves it
/// `Allow` and committed. The commit is what rebuilds the base state the later entries run on.
#[test]
fn an_over_budget_known_allowed_entry_is_allowed_and_committed() {
    let source = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[&source],
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow, "known_allowed ignores its capture outcome");
    assert_eq!(run.proxy_slot0, U256::from(1u64), "the known_allowed commit is never withheld");
    assert_eq!(run.depositor_nonce, 1);
}

// ------------------------------------------------------------------- event-binding budget ----

/// Two anonymous zero-input events, each binding every `LOG0` the target emits, so the per-rule
/// binding count is the product of the two candidate lists. The condition never holds, so an
/// admissible count allows.
fn two_anonymous_events_rule() -> RawRule {
    serde_json::from_value(serde_json::json!({
        "id": "two-anonymous-events",
        "event_abis": {
            "a": { "type": "event", "name": "A", "inputs": [], "anonymous": true },
            "b": { "type": "event", "name": "B", "inputs": [], "anonymous": true }
        },
        "condition": false,
        "action": "deny"
    }))
    .expect("valid two-anonymous-event rule")
}

/// `LOG0(0, 0)` repeated `times`, then `STOP`.
fn log0_emitting_code(times: usize) -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 0..times {
        code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0xa0]);
    }
    code.push(0x00);
    code
}

fn log0_db(times: usize) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    db.insert_account_info(
        LOG_TARGET,
        AccountInfo {
            code: Some(Bytecode::new_raw(log0_emitting_code(times).into())),
            ..Default::default()
        },
    );
    db
}

#[test]
fn the_event_binding_budget_fails_closed_before_enumeration_and_commits_nothing() {
    let admissible = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), LOG_TARGET)],
        vec![two_anonymous_events_rule()],
        &[],
        log0_db(100),
    );
    assert_eq!(
        only(&admissible).verdict,
        Verdict::Allow,
        "100 logs × 100 logs = 10 000 bindings is exactly the limit, not over it"
    );
    assert_eq!(admissible.depositor_nonce, 1, "an admissible transaction commits its state");

    let over = run_batch(
        &[deposit_request(&format!("0x{}", "44".repeat(32)), LOG_TARGET)],
        vec![two_anonymous_events_rule()],
        &[],
        log0_db(101),
    );
    assert_eq!(
        only(&over).verdict,
        Verdict::Malformed,
        "101 logs × 101 logs = 10 201 bindings exceeds the budget before enumeration"
    );
    assert_eq!(over.depositor_nonce, 0, "an over-budget transaction commits nothing");
}

// ------------------------------------------------------------- ordered upgrade batches ----

/// The storage slot the upgradeable proxy keeps its implementation address in.
const IMPL_SLOT: U256 = U256::ZERO;

/// Implementation code with a distinct hash per marker that writes no storage, so a call through
/// the proxy cannot overwrite the pointer it is being judged by.
fn ordered_impl_code(tag: u8) -> Vec<u8> {
    vec![0x60, tag, 0x50, 0x00] // PUSH1 tag; POP; STOP
}

/// An upgradeable proxy, in runtime bytecode.
///
/// With calldata it performs the upgrade and nothing else:
/// `SSTORE(IMPL_SLOT, calldataload(0))` — no delegatecall, so it produces no
/// direct-delegatecall observation at all. With empty calldata it delegatecalls into
/// whatever address the slot currently holds, which is the frame a `callstack` alias
/// on this proxy observes. That is what makes the second deposit's verdict a function
/// of the first deposit's committed write rather than of the fixture.
fn upgradeable_proxy_code() -> Vec<u8> {
    vec![
        0x36, // CALLDATASIZE
        0x60, 0x13, // PUSH1 19 -> the upgrade JUMPDEST
        0x57, // JUMPI  (calldata present => upgrade)
        // --- delegate branch: retSize, retOffset, argsSize, argsOffset ---
        0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, //
        0x60, 0x00, // PUSH1 IMPL_SLOT
        0x54, // SLOAD -> implementation address
        0x5a, // GAS
        0xf4, // DELEGATECALL
        0x50, // POP
        0x00, // STOP
        // --- upgrade branch (offset 19) ---
        0x5b, // JUMPDEST
        0x60, 0x00, // PUSH1 0 (calldata offset)
        0x35, // CALLDATALOAD -> the new implementation address, word-aligned
        0x60, 0x00, // PUSH1 IMPL_SLOT
        0x55, // SSTORE
        0x00, // STOP
    ]
}

/// The audit path's own database, with the upgradeable proxy and its initial pointer.
fn upgradeable_db(initial_impl: Address) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    for (address, code) in [
        (PROXY, upgradeable_proxy_code()),
        (APPROVED_IMPL, ordered_impl_code(0x01)),
        (UNAPPROVED_IMPL, ordered_impl_code(0x02)),
    ] {
        db.insert_account_info(
            address,
            AccountInfo { code: Some(Bytecode::new_raw(code.into())), ..Default::default() },
        );
    }
    db.insert_account_storage(PROXY, IMPL_SLOT, initial_impl.into_word().into())
        .expect("the fixture proxy's slot is writable");
    db
}

/// One deposit request body. `data` empty means "call the proxy"; `data` carrying a
/// word-aligned address means "upgrade the proxy to it".
fn deposit_request_body(source_hash: B256, data: &str) -> serde_json::Value {
    serde_json::json!({
        "source_hash": format!("{source_hash:#x}"),
        "from": format!("{DEPOSITOR:#x}"),
        "to": format!("{PROXY:#x}"),
        "mint": "0",
        "value": "0",
        "gas_limit": 1_000_000,
        "is_system_transaction": false,
        "data": data,
    })
}

/// The 32-byte, right-aligned calldata word that upgrades the proxy to `target`.
fn upgrade_calldata(target: Address) -> String {
    format!("0x{}", alloy_primitives::hex::encode(target.into_word()))
}

/// What the real ordered batch did: per-transaction verdicts, the observation set each
/// execution actually saw, and the proxy's implementation slot afterwards.
struct BatchOutcome {
    verdicts: Vec<Verdict>,
    observed: Vec<Option<Vec<rcs_filter::callstack::CallObservation>>>,
    implementation_after: Address,
    depositor_nonce: u64,
}

/// Drives ONE ordered batch of deposit requests through the complete production batch,
/// against a proxy whose pointer starts at `initial_impl`.
fn run_ordered_batch(
    approved: &[B256],
    initial_impl: Address,
    deposits: Vec<serde_json::Value>,
) -> BatchOutcome {
    let count = deposits.len();
    let request: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
        "rules": [proxy_impl_allowlist_rule_json_approving(approved)],
        "txs": deposits,
    }))
    .expect("the fixture request satisfies the audit RPC's own wire schema");
    assert_eq!(request.txs.len(), count, "every deposit reached the request");

    let mut db = upgradeable_db(initial_impl);
    let _ = observation_recorder::take();
    let results = execute_audit_batch(&request, &mut db, audit_env())
        .expect("the production batch executes the fixture batch");
    let observed = observation_recorder::take();
    assert_eq!(observed.len(), count, "one recorded observation set per transaction");

    let verdicts = results.iter().map(|result| result.verdict).collect();
    // Read the pointer back out of the batch's OWN database, so the state assertion is
    // about what the batch committed rather than about what it reported.
    let slot = db.storage(PROXY, IMPL_SLOT).expect("the proxy's implementation slot is readable");
    let depositor_nonce =
        db.basic(DEPOSITOR).expect("the depositor account is readable").expect("seeded").nonce;
    BatchOutcome {
        verdicts,
        observed,
        implementation_after: Address::from_word(slot.into()),
        depositor_nonce,
    }
}

/// The one direct-delegatecall observation of the proxy in `set`, if any.
fn delegatecall_of_proxy(
    set: &Option<Vec<rcs_filter::callstack::CallObservation>>,
) -> Option<&rcs_filter::callstack::CallObservation> {
    set.as_ref()?.iter().find(|observation| {
        observation.kind == rcs_filter::callstack::CallObservationKind::DirectDelegatecall
            && observation.address == PROXY
    })
}

/// The direction that matters: the batch starts with an **approved**
/// implementation installed, the first deposit upgrades it to an **unapproved** one,
/// and the second deposit — which merely calls the proxy — is denied.
///
/// Three independent things are asserted, because any one of them alone could be
/// satisfied for the wrong reason:
///   1. the state update really happened and really was committed (the slot the batch's
///      own database holds afterwards),
///   2. the later execution's own observation set names the NEW implementation and its
///      executed code hash — this is what "judged against the new implementation"
///      means, and it comes from the batch's own recording, not from re-deriving it,
///   3. the verdicts.
#[test]
fn a_later_deposit_is_judged_against_the_implementation_an_earlier_one_installed() {
    let approved = keccak256(ordered_impl_code(0x01)); // APPROVED_IMPL's code
    let upgrade = B256::repeat_byte(0x41);
    let call = B256::repeat_byte(0x42);

    let outcome = run_ordered_batch(
        &[approved],
        APPROVED_IMPL,
        vec![
            deposit_request_body(upgrade, &upgrade_calldata(UNAPPROVED_IMPL)),
            deposit_request_body(call, "0x"),
        ],
    );

    // 1. The earlier deposit's write survived into the state the batch carries.
    assert_eq!(
        outcome.implementation_after, UNAPPROVED_IMPL,
        "the earlier deposit's upgrade must be committed, or the later deposit would \
             be judged against the old implementation for the wrong reason"
    );

    // 2. The upgrade itself delegatecalls nothing, so it produces no candidate frame;
    //    the later execution's own observation names the newly installed one.
    assert!(
        delegatecall_of_proxy(&outcome.observed[0]).is_none(),
        "the upgrade transaction performs no delegatecall: {:?}",
        outcome.observed[0]
    );
    let later = delegatecall_of_proxy(&outcome.observed[1])
        .expect("the later deposit's execution delegatecalled the proxy");
    assert_eq!(
        later.impl_address,
        Some(UNAPPROVED_IMPL),
        "the later execution observed the NEW implementation"
    );
    assert_eq!(
        later.impl_code_hash,
        Some(keccak256(ordered_impl_code(0x02))),
        "and the executed bytecode hash of that new implementation"
    );

    // 3. The verdicts. The upgrade observes no frame at all, so the alias-bearing rule
    //    is suppressed for it and it is allowed (which is what lets it commit); the
    //    later deposit is denied on the implementation the upgrade installed.
    assert_eq!(
        outcome.verdicts,
        vec![Verdict::Allow, Verdict::Deny],
        "the upgrade is allowed and commits; the later deposit is denied"
    );
    assert_eq!(
        outcome.depositor_nonce, 1,
        "exactly one of the two deposits committed its state: the upgrade"
    );
}

/// The mirror, and the reason the test above cannot pass for the wrong reason: run the
/// SAME batch with the pointer starting on the unapproved implementation and the
/// earlier deposit upgrading it to the approved one, and the later deposit is
/// **allowed**.
///
/// The two cases differ only in which implementation the earlier deposit installs, so
/// a later deposit judged against the *initial* pointer — or against the fixture, or
/// against nothing — would give the same verdict in both and fail here.
#[test]
fn the_same_batch_allows_when_the_earlier_deposit_installs_the_approved_one() {
    let approved = keccak256(ordered_impl_code(0x01));
    let outcome = run_ordered_batch(
        &[approved],
        UNAPPROVED_IMPL,
        vec![
            deposit_request_body(B256::repeat_byte(0x43), &upgrade_calldata(APPROVED_IMPL)),
            deposit_request_body(B256::repeat_byte(0x44), "0x"),
        ],
    );

    assert_eq!(outcome.implementation_after, APPROVED_IMPL);
    let later =
        delegatecall_of_proxy(&outcome.observed[1]).expect("the later deposit delegatecalled");
    assert_eq!(later.impl_address, Some(APPROVED_IMPL));
    assert_eq!(later.impl_code_hash, Some(keccak256(ordered_impl_code(0x01))));
    assert_eq!(
        outcome.verdicts,
        vec![Verdict::Allow, Verdict::Allow],
        "both deposits are allowed once the approved implementation is installed"
    );
    assert_eq!(outcome.depositor_nonce, 2, "both allowed deposits committed, in order");
}

/// Ordering is load-bearing, not incidental: put the call BEFORE the upgrade and the
/// verdicts swap. Same two transactions, same rule, same initial state — only the
/// order differs.
#[test]
fn reversing_the_order_moves_the_denial_to_the_other_deposit() {
    let approved = keccak256(ordered_impl_code(0x01));
    let outcome = run_ordered_batch(
        &[approved],
        UNAPPROVED_IMPL,
        vec![
            deposit_request_body(B256::repeat_byte(0x45), "0x"),
            deposit_request_body(B256::repeat_byte(0x46), &upgrade_calldata(APPROVED_IMPL)),
        ],
    );

    assert_eq!(
        outcome.verdicts,
        vec![Verdict::Deny, Verdict::Allow],
        "the call is judged against the pointer in force when IT ran"
    );
    let earlier =
        delegatecall_of_proxy(&outcome.observed[0]).expect("the earlier deposit delegatecalled");
    assert_eq!(earlier.impl_address, Some(UNAPPROVED_IMPL));
    assert_eq!(outcome.implementation_after, APPROVED_IMPL);
    assert_eq!(
        outcome.depositor_nonce, 1,
        "the denied deposit's state was discarded; the upgrade's was committed"
    );
}

/// The duplicate-key cases, completing their "nothing is executed or committed" half on the entry
/// point that can actually execute something.
///
/// The rule-layer and request-decode cases prove the duplicate key is refused. This
/// proves what that refusal means in state terms: the identical batch, with one
/// duplicate key added to the rule text, never reaches execution — so the depositor's
/// nonce and the proxy's implementation pointer are exactly what they were, while the
/// same batch without the duplicate does execute and does commit.
#[test]
fn a_duplicate_key_in_the_request_executes_and_commits_nothing() {
    let approved = format!("{:#x}", keccak256(ordered_impl_code(0x01)));
    let deposit = serde_json::to_string(&deposit_request_body(
        B256::repeat_byte(0x47),
        &upgrade_calldata(UNAPPROVED_IMPL),
    ))
    .expect("the fixture deposit serialises");
    let rule = format!(
        r#"{{"id":"proxy-impl-allowlist","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"entry":"{PROXY:#x}"}}}},"condition":{{"!":{{"in":[{{"var":"entry.delegateCodeHash"}},["{approved}"]]}}}},"action":"deny"}}"#
    );

    // Raw request text, so the duplicate key is still present when the decode runs.
    for (tail, label) in [
        (r#","action":"allow""#, "duplicate modelled root key"),
        (
            r#","callstack":{"direct_delegatecalls":{"entry":"0x00000000000000000000000000000000000000cc"}}"#,
            "duplicate callstack section",
        ),
    ] {
        let with_duplicate = format!("{}{tail}}}", rule.trim_end_matches('}'));
        let body = format!(r#"{{"rules":[{with_duplicate}],"txs":[{deposit}]}}"#);
        assert!(
            serde_json::from_str::<AuditTransactionsRequest>(&body).is_err(),
            "{label}: the request carrying it must not decode, so nothing can run"
        );
    }

    // The alias-duplicate shape, likewise.
    let alias_dup = format!(
        r#"{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"entry":"{PROXY:#x}","entry":"0x00000000000000000000000000000000000000cc"}}}},"condition":true,"action":"deny"}}"#
    );
    assert!(
        serde_json::from_str::<AuditTransactionsRequest>(&format!(
            r#"{{"rules":[{alias_dup}],"txs":[{deposit}]}}"#
        ))
        .is_err(),
        "a duplicate alias must not decode either"
    );

    // The control: the same batch WITHOUT the duplicate decodes, executes and commits
    // — so the refusals above really did stop something that would otherwise have run.
    let control = run_ordered_batch(
        &[keccak256(ordered_impl_code(0x01))],
        APPROVED_IMPL,
        vec![deposit_request_body(B256::repeat_byte(0x47), &upgrade_calldata(UNAPPROVED_IMPL))],
    );
    assert_eq!(control.verdicts, vec![Verdict::Allow]);
    assert_eq!(control.depositor_nonce, 1, "the control really did execute and commit");
    assert_eq!(control.implementation_after, UNAPPROVED_IMPL);
}
