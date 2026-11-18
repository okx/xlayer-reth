//! Real capture and a real verdict on `xlayer_auditTransactions`: capture / verdict / commit
//! semantics, the per-transaction observation budget, the synthetic-prefix exception, ordered
//! batches and the `CallstackOnly` mode's log surface.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, B256, U256};
use metrics_util::debugging::DebuggingRecorder;
use rcs_filter::rules::{load_rules, RawRule};
use rcs_filter::test_support::synthetic_ban_golden::golden_bans;
use revm::database::InMemoryDB;
use revm::state::{AccountInfo, Bytecode};
use revm::Database as _;
use xlayer_rpc::audit::rpc::observation_recorder;
use xlayer_rpc::audit::rpc::test_seams::execute_audit_batch;
use xlayer_rpc::audit::rpc::{
    AuditTransactionsRequest, KnownAllowedKind, AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX,
};
use xlayer_rpc::audit::verdict::{CallstackIdentification, Verdict};

use crate::harness::*;

/// A second proxy the request's rules do not observe.
const PROXY_B: Address = Address::repeat_byte(0xAB);

/// The contract that emits the `LOG0`s the event-binding fixture binds.
const LOG_TARGET: Address = Address::repeat_byte(0xDD);

// ---------------------------------------------------------------- batch semantics ---------

/// a `callstack` rule is accepted and classified from REAL capture: the
/// deposit→proxy→unapproved-implementation request returns the deny verdict.
#[test]
fn v2_callstack_rule_denies_unapproved_implementation() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[],
        None,
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Deny);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Objected));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a denied transaction's state is not committed");
}

/// contrast: the same request against the APPROVED implementation returns allow, and
/// its state IS committed. Without this, the deny case above could pass for the wrong reason.
#[test]
fn same_request_with_approved_implementation_allows() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[],
        None,
        seeded_db(APPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Clean));
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
        1,
        &[],
        None,
        // The proxy's own code performs NO delegatecall here: it is a bare `STOP`.
        seeded_db(UNAPPROVED_IMPL, 0),
    );
    let result = only(&run);
    assert_eq!(
        result.verdict,
        Verdict::Allow,
        "a plain call to the proxy has no implementation to compare and must not be denied"
    );
    assert_eq!(
        result.callstack_identification,
        Some(CallstackIdentification::Clean),
        "the frame WAS identified; it simply carries no implementation"
    );
}

/// a `known_allowed` Original denied on the callstack dimension does NOT stop the
/// batch: a later transaction still executes and is classified.
#[test]
fn a_denied_original_does_not_stop_the_batch() {
    let first = format!("0x{}", "33".repeat(32));
    let second = format!("0x{}", "44".repeat(32));
    let run = run_batch(
        &[deposit_request(&first, PROXY), deposit_request(&second, APPROVED_IMPL)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&first],
        Some([(first.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(run.results[0].verdict, Verdict::Deny);
    let later = &run.results[1];
    assert_eq!(later.verdict, Verdict::Allow);
}

/// An UNLABELLED `known_allowed` entry that is not a synthetic ban, executing an unapproved
/// implementation, is adjudicated as an approved original: `Deny`, with nothing committed.
#[test]
fn an_unlabelled_original_running_unapproved_code_is_denied_and_commits_nothing() {
    let source = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        // No label map at all — the shape a caller that is not label-aware sends.
        None,
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Deny, "an unlabelled original is re-adjudicated");
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Objected));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a denied original commits nothing");

    // The identical execution labelled `Original` gets the identical result.
    let labelled = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let result = only(&labelled);
    assert_eq!(result.verdict, Verdict::Deny);
    assert_eq!(labelled.proxy_slot0, U256::ZERO, "a denied Original commits nothing");
}

/// A legacy batch carrying no label map at all: several `known_allowed` entries plus an
/// ordinary deposit, with alias-bearing rules, all executing an unapproved implementation.
#[test]
fn an_unlabelled_batch_adjudicates_its_originals_and_the_ordinary_deposit_alike() {
    let first = format!("0x{}", "66".repeat(32));
    let second = format!("0x{}", "67".repeat(32));
    let ordinary = format!("0x{}", "68".repeat(32));
    let run = run_batch(
        &[
            deposit_request(&first, PROXY),
            deposit_request(&second, PROXY),
            deposit_request(&ordinary, PROXY),
        ],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&first, &second],
        None,
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(run.results[0].verdict, Verdict::Deny, "unlabelled original");
    assert_eq!(run.results[1].verdict, Verdict::Deny, "unlabelled original");
    assert_eq!(
        run.results[2].verdict,
        Verdict::Deny,
        "the ordinary deposit is adjudicated, not exempted"
    );
    assert_eq!(run.proxy_slot0, U256::ZERO, "no denied transaction added state");
}

/// An unlabelled entry that is not a synthetic ban, whose observation set cannot be adjudicated
/// — unavailable, and separately `Available` under a FOREIGN stamp — gets the non-verdict with
/// nothing committed, exactly as a labelled `Original` does.
#[test]
fn an_unadjudicable_unlabelled_original_is_the_non_verdict_and_commits_nothing() {
    let source = format!("0x{}", "66".repeat(32));

    // Unavailable: the observation budget is exhausted.
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        None,
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict commits nothing");

    // Available, but produced under a different snapshot identity.
    let raw_rules = vec![proxy_impl_allowlist_rule()];
    let run = run_batch_stamped(
        &[deposit_request(&source, PROXY)],
        raw_rules.clone(),
        1,
        &[&source],
        None,
        seeded_db(APPROVED_IMPL, 1),
        Some(foreign_stamp(&raw_rules, 1)),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict commits nothing");
}

/// Proof that the synthetic-prefix exemption is NARROWED and not widened: with the same
/// unadjudicable observation set, an ordinary deposit and a labelled `Original` both still get
/// the fail-closed non-verdict, with nothing committed.
#[test]
fn the_unadjudicable_gate_still_catches_ordinary_and_labelled_original_entries() {
    let source = format!("0x{}", "66".repeat(32));
    for kinds in [
        None,
        Some(
            [(source.clone(), KnownAllowedKind::Original)]
                .into_iter()
                .collect::<BTreeMap<String, KnownAllowedKind>>(),
        ),
    ] {
        let known: &[&str] = if kinds.is_some() { &[&source] } else { &[] };
        let run = run_batch(
            &[deposit_request(&source, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            1,
            known,
            kinds,
            seeded_db(APPROVED_IMPL, 1_000),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Malformed, "never Allow");
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
        assert_eq!(run.proxy_slot0, U256::ZERO, "state must not be committed");
    }
}

/// when the request declares no selector, no inspector is constructed, the
/// identification diagnostic is `not_applicable`, and a v1 response keeps its exact previous
/// shape (the field is absent from the JSON).
#[test]
fn selector_free_request_is_unchanged() {
    let source = format!("0x{}", "33".repeat(32));
    let rules = load_rules(1, 0, vec![event_only_rule()]);
    assert!(
        rcs_filter::audit::callstack_only_inspector(
            &rules,
            AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX
        )
        .is_none(),
        "a selector-free request must construct NO inspector"
    );

    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![event_only_rule()],
        1,
        &[&source],
        None,
        seeded_db(APPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(
        result.callstack_identification, None,
        "a v1 response must not carry the diagnostic"
    );
    let json = serde_json::to_value(result).expect("serializes");
    assert!(
        json.get("callstack_identification").is_none(),
        "the v1 wire shape is unchanged: {json}"
    );
    assert_eq!(run.proxy_slot0, U256::from(1u64), "known_allowed still commits as before");
}

/// an **event-only** rule with a non-zero internal native transfer in the transaction
/// returns exactly its pre-change verdict, which proves no virtual native-transfer log
/// entered the screened stream. This is the direct consequence of this path running in
/// `CallstackOnly` mode: under `NativeAndCallstack` the same transaction WOULD produce a
/// virtual `Transfer` on the native-asset identifier, and the rule below is written to deny
/// exactly that.
#[test]
fn no_virtual_native_log_enters_the_screened_stream() {
    let source = format!("0x{}", "33".repeat(32));

    // With capture armed (a selector-bearing rule is present) and a transaction that really
    // performs an internal native transfer: the native-log rule must NOT match.
    let armed = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![native_transfer_deny_rule(), inert_selector_rule()],
        1,
        &[],
        None,
        value_transferring_db(),
    );
    // Without capture at all — the pre-change path for this rule.
    let unarmed = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![native_transfer_deny_rule()],
        1,
        &[],
        None,
        value_transferring_db(),
    );

    assert_eq!(only(&unarmed).verdict, Verdict::Allow, "baseline: no native log is screened");
    assert_eq!(
        only(&armed).verdict,
        only(&unarmed).verdict,
        "arming capture must not introduce a virtual native-transfer log"
    );
}

/// Deny on a `Transfer` whose `log.address` is the native-asset identifier — i.e. exactly the
/// virtual log `NativeAndCallstack` capture would synthesize, and nothing a real EVM emits
/// here.
fn native_transfer_deny_rule() -> RawRule {
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
            format!("{:#x}", rcs_filter::capture::NATIVE_ASSET_ADDRESS)
        ] },
        "action": "deny"
    }))
    .expect("valid native-log rule")
}

/// An alias-bearing rule that exists only to arm capture: its condition can never hold,
/// because the alias only ever binds the address it was declared on.
fn inert_selector_rule() -> RawRule {
    serde_json::from_value(serde_json::json!({
        "id": "arm-capture-only",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "==": [ { "var": "entry.delegateTo" }, format!("{PROXY:#x}") ] },
        "action": "deny"
    }))
    .expect("valid alias rule")
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

/// the same for an ORDINARY entry (no `known_allowed` at all), running UNAPPROVED code: the
/// request declares a selector, the execution could not be identified, so it is the
/// non-verdict and its state stays out of the overlay.
#[test]
fn capture_failure_on_an_ordinary_entry_with_skipped_rules_is_the_non_verdict() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
        1,
        &[],
        None,
        seeded_db(UNAPPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
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
    let kinds = BTreeMap::from([(source.clone(), KnownAllowedKind::Original)]);

    let fresh =
        run_batch(&[deposit_request(&source, PROXY)], rules.clone(), 1, &[], None, logging_db());
    assert_eq!(
        only(&fresh).verdict,
        Verdict::Audit,
        "baseline: an unapproved entry really does reach adjudication on this fixture"
    );

    let mut rounds: Vec<String> = Vec::new();
    for round in 0..3 {
        let run = run_batch(
            &[deposit_request(&source, PROXY)],
            rules.clone(),
            1,
            &[&source],
            Some(kinds.clone()),
            logging_db(),
        );
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
fn the_audit_default_budget_is_1000_and_independent_of_the_filter_config() {
    assert_eq!(AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX, 1_000);
    assert_eq!(
        rcs_filter::config::FilterConfig::default().max_callstack_observations_per_tx,
        10_000,
        "the payload builder's own default is unchanged and unrelated"
    );
}

#[test]
fn exactly_1000_observations_are_adjudicated_and_committed_under_the_default_budget() {
    let source = format!("0x{}", "33".repeat(32));
    let _ = observation_recorder::take();
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[],
        None,
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
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Clean));
    assert_eq!(run.proxy_slot0, U256::from(1u64), "an allowed transaction commits its state");
    assert_eq!(run.depositor_nonce, 1);
}

#[test]
fn the_1001st_observation_fails_closed_without_committing_under_the_default_budget() {
    let source = format!("0x{}", "33".repeat(32));
    let _ = observation_recorder::take();
    let (run, out) = capture_tracing(|| {
        run_batch(
            &[deposit_request(&source, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            1,
            &[],
            None,
            seeded_db(APPROVED_IMPL, 1_000),
        )
    });

    let recorded = observation_recorder::take();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0], None,
        "an over-budget capture is an unavailable set, never an empty available one"
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed, "never Allow on a capture failure");
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict does not commit state");
    assert_eq!(run.depositor_nonce, 0);
    assert!(
        !out.contains("xlayer_audit_rpc::synthetic_ban_budget"),
        "only a synthetic prefix emits the over-budget event: {out}"
    );
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
        1,
        &[],
        None,
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
        1,
        &[],
        None,
        db,
    );
    assert_eq!(run.results[0].verdict, Verdict::Malformed);
    assert_eq!(run.results[1].verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::ZERO, "the over-budget transaction committed nothing");
    assert_eq!(run.depositor_nonce, 1, "only the second transaction committed");
}

#[test]
fn an_over_budget_known_allowed_original_fails_closed_without_committing() {
    let source = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed, "an approved original is not exempt");
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict commits nothing");
    assert_eq!(run.depositor_nonce, 0);
}

// ------------------------------------------------------------- synthetic-prefix exception ----

/// A synthetic ban prefix keeps its `Allow` and its commit even when over the budget, reports
/// `not_identified`, and additionally emits the separately filterable over-budget event.
///
/// The exception is the Player's decision for this change: the prefix rebuilds the base state the
/// later entries are adjudicated on, so withholding its commit would corrupt that base state. The
/// event exists because the response diagnostic reaches nobody for such an entry.
#[test]
fn an_over_budget_labelled_synthetic_ban_prefix_is_committed_and_reports_budget_exceeded() {
    in_isolated_process(
        module_path!(),
        "an_over_budget_labelled_synthetic_ban_prefix_is_committed_and_reports_budget_exceeded",
        labelled_synthetic_ban_prefix_over_budget,
    );
}

fn labelled_synthetic_ban_prefix_over_budget() {
    let prefix = format!("0x{}", "55".repeat(32));
    let kinds: BTreeMap<String, KnownAllowedKind> =
        [(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect();

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _guard = metrics::set_default_local_recorder(&recorder);

    let (run, out) = capture_tracing(|| {
        run_batch(
            &[deposit_request(&prefix, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            1,
            &[&prefix],
            Some(kinds.clone()),
            seeded_db(APPROVED_IMPL, 1_000),
        )
    });

    let s = only(&run);
    assert_eq!(s.verdict, Verdict::Allow, "the prefix contract stays unconditional");
    assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::from(1u64), "the prefix's commit is never withheld");
    assert_eq!(run.depositor_nonce, 1);

    assert_eq!(
        observed(&snapshotter),
        labelled_prefix_counters(),
        "the prefix is counted once as not_identified and nothing else"
    );
    assert_one_budget_event(&out, &prefix, &approved_hash());

    // In-budget control: the same shape at exactly 1 000 observations is identified clean and
    // emits no budget event. Its counters are not re-read: the handles stay bound to this
    // process's first recorder.
    let (control, control_out) = capture_tracing(|| {
        run_batch(
            &[deposit_request(&prefix, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            1,
            &[&prefix],
            Some(kinds),
            seeded_db(APPROVED_IMPL, 999),
        )
    });
    let c = only(&control);
    assert_eq!(c.verdict, Verdict::Allow);
    assert_eq!(c.callstack_identification, Some(CallstackIdentification::Clean));
    assert_eq!(control.proxy_slot0, U256::from(1u64));
    assert!(
        !control_out.contains("xlayer_audit_rpc::synthetic_ban_budget"),
        "an in-budget prefix emits no budget event: {control_out}"
    );
}

/// The same exception for a request that sends NO label map: the golden ban is recognised by the
/// content-binding check over the built deposit's own bytes.
#[test]
fn an_over_budget_inferred_synthetic_ban_prefix_is_committed_and_reports_budget_exceeded() {
    in_isolated_process(
        module_path!(),
        "an_over_budget_inferred_synthetic_ban_prefix_is_committed_and_reports_budget_exceeded",
        inferred_synthetic_ban_prefix_over_budget,
    );
}

/// The golden one-element ban, sent to `PROXY`. The proxy ignores its calldata and performs the
/// delegatecalls, so the classification rests on the deposit's own `source_hash` and `input`.
fn golden_ban_to_proxy() -> xlayer_rpc::audit::deposit::DepositTxRequest {
    let ban = golden_bans().into_iter().find(|ban| ban.n == 1).expect("an n = 1 vector");
    xlayer_rpc::audit::deposit::DepositTxRequest {
        data: ban.data,
        ..deposit_request(&ban.source_hash, PROXY)
    }
}

fn inferred_synthetic_ban_prefix_over_budget() {
    let req = golden_ban_to_proxy();
    let source = req.source_hash.clone();

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _guard = metrics::set_default_local_recorder(&recorder);

    let (run, out) = capture_tracing(|| {
        run_batch(
            std::slice::from_ref(&req),
            vec![proxy_impl_allowlist_rule()],
            1,
            &[&source],
            // No label map at all: the class is decided by the content-binding check.
            None,
            seeded_db(APPROVED_IMPL, 1_000),
        )
    });

    let s = only(&run);
    assert_eq!(s.verdict, Verdict::Allow, "the prefix contract stays unconditional");
    assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::from(1u64), "the prefix's commit is never withheld");
    assert_eq!(run.depositor_nonce, 1);

    assert_eq!(
        observed(&snapshotter),
        inferred_prefix_counters(),
        "the entry is recognised as a synthetic ban by the content-binding check"
    );
    assert_one_budget_event(&out, &source, &approved_hash());

    let (control, control_out) = capture_tracing(|| {
        run_batch(
            std::slice::from_ref(&req),
            vec![proxy_impl_allowlist_rule()],
            1,
            &[&source],
            None,
            seeded_db(APPROVED_IMPL, 999),
        )
    });
    let c = only(&control);
    assert_eq!(c.verdict, Verdict::Allow);
    assert_eq!(c.callstack_identification, Some(CallstackIdentification::Clean));
    assert_eq!(control.proxy_slot0, U256::from(1u64));
    assert!(
        !control_out.contains("xlayer_audit_rpc::synthetic_ban_budget"),
        "an in-budget prefix emits no budget event: {control_out}"
    );
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
        1,
        &[],
        None,
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
        1,
        &[],
        None,
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
