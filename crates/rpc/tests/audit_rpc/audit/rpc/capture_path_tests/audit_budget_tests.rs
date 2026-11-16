//! The audit RPC's own call-frame observation budget: the default value, the 1 000 / 1 001
//! boundary, per-transaction isolation, the synthetic-prefix exception and the event-binding
//! budget. Every batch below drives the production `run_audit_batch` and observes the commit
//! directly in the EVM database.

use super::*;
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use rcs_filter::test_support::synthetic_ban_golden::golden_bans;

/// Marks a re-executed child of this test binary; its value is the one test the child runs.
const ISOLATED_TEST_ENV: &str = "XLAYER_RPC_AUDIT_BUDGET_ISOLATED_TEST";

/// A second proxy the request's rules do not observe.
const PROXY_B: Address = Address::repeat_byte(0xAB);

/// The contract that emits the `LOG0`s the event-binding fixture binds.
const LOG_TARGET: Address = Address::repeat_byte(0xDD);

/// Runs `body` as the first metrics user of a fresh process.
///
/// In the parent (marker unset) this re-executes the current test binary with
/// `--exact <full test name>` and the marker set to that name, and fails unless the child exits
/// successfully having run exactly that one test; the child's output is part of the failure. In
/// the child (marker == the name) it runs `body` directly, so there is no further re-execution.
/// The same path is taken under `cargo test` (all tests in one process) and nextest.
fn in_isolated_process(test: &str, body: impl FnOnce()) {
    let module = module_path!().split_once("::").map_or(module_path!(), |(_, rest)| rest);
    let name = format!("{module}::{test}");
    match std::env::var_os(ISOLATED_TEST_ENV) {
        Some(marker) if marker == name.as_str() => return body(),
        Some(marker) => panic!("child for {marker:?} reached {name}"),
        None => {}
    }
    let exe = std::env::current_exe().expect("the running test binary");
    let output = std::process::Command::new(exe)
        .args([name.as_str(), "--exact", "--test-threads=1"])
        .env(ISOLATED_TEST_ENV, &name)
        .output()
        .expect("spawn the isolated child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "isolated child for {name} failed ({}):\n{stdout}\n{stderr}",
        output.status
    );
}

/// The `(name, count)` pairs the recorder registered for the two audit families.
///
/// Only valid inside [`in_isolated_process`]: the metrics derive caches each metrics struct from
/// `Default` in a process-global `OnceLock`, so handles created before this process's recorder was
/// installed stay bound to the previous recorder and would report nothing.
fn observed(snapshotter: &Snapshotter) -> Vec<(String, u64)> {
    let mut counters: Vec<(String, u64)> = snapshotter
        .snapshot()
        .into_vec()
        .into_iter()
        .filter_map(|(key, _, _, value)| match value {
            DebugValue::Counter(count) if is_audit_family(key.key().name()) => {
                Some((key.key().name().to_string(), count))
            }
            _ => None,
        })
        .collect();
    counters.sort();
    counters
}

fn is_audit_family(name: &str) -> bool {
    name.starts_with("rcs_filter.synthetic_ban_identification")
        || name.starts_with("rcs_filter.known_allowed_resolution")
}

/// The two audit families with the labelled synthetic prefix's one `not_identified` and one
/// `labelled` increment, everything else at zero.
fn labelled_prefix_counters() -> Vec<(String, u64)> {
    vec![
        ("rcs_filter.known_allowed_resolution.inferred_original_total".to_string(), 0),
        ("rcs_filter.known_allowed_resolution.inferred_synthetic_ban_total".to_string(), 0),
        ("rcs_filter.known_allowed_resolution.labelled_total".to_string(), 1),
        ("rcs_filter.synthetic_ban_identification.clean_total".to_string(), 0),
        ("rcs_filter.synthetic_ban_identification.not_applicable_total".to_string(), 0),
        ("rcs_filter.synthetic_ban_identification.not_identified_total".to_string(), 1),
        ("rcs_filter.synthetic_ban_identification.objected_total".to_string(), 0),
    ]
}

/// The same with the class inferred by the content-binding check instead of labelled.
fn inferred_prefix_counters() -> Vec<(String, u64)> {
    vec![
        ("rcs_filter.known_allowed_resolution.inferred_original_total".to_string(), 0),
        ("rcs_filter.known_allowed_resolution.inferred_synthetic_ban_total".to_string(), 1),
        ("rcs_filter.known_allowed_resolution.labelled_total".to_string(), 0),
        ("rcs_filter.synthetic_ban_identification.clean_total".to_string(), 0),
        ("rcs_filter.synthetic_ban_identification.not_applicable_total".to_string(), 0),
        ("rcs_filter.synthetic_ban_identification.not_identified_total".to_string(), 1),
        ("rcs_filter.synthetic_ban_identification.objected_total".to_string(), 0),
    ]
}

/// The single bounded over-budget event, asserted on the line itself: fixed tokens only, no rule
/// id, no code hash and no observation content.
fn assert_one_budget_event(out: &str, source_hash: &str, code_hash: &B256) {
    let lines: Vec<&str> = out
        .lines()
        .filter(|line| line.contains("xlayer_audit_rpc::synthetic_ban_budget"))
        .collect();
    assert_eq!(lines.len(), 1, "exactly one budget event: {out}");
    let line = lines[0];
    assert!(line.contains("WARN"), "expected warn level: {line}");
    assert!(line.contains("reason=\"budget_exceeded\""), "{line}");
    assert!(line.contains("outcome=\"not_identified\""), "{line}");
    assert!(line.contains("committed=true"), "{line}");
    assert!(line.contains(source_hash), "the request's source hash: {line}");
    assert!(line.contains("selector_bearing_rules=1"), "{line}");
    assert!(!line.contains(&format!("{code_hash:#x}")), "no code hash: {line}");
}

// ---------------------------------------------------------------- default and boundary --------

#[test]
fn the_audit_default_budget_is_1000_and_independent_of_the_filter_config() {
    assert_eq!(AuditCaptureConfig::DEFAULT_MAX_CALLSTACK_OBSERVATIONS_PER_TX, 1_000);
    assert_eq!(
        AuditCaptureConfig::default().max_callstack_observations_per_tx,
        AuditCaptureConfig::DEFAULT_MAX_CALLSTACK_OBSERVATIONS_PER_TX,
        "the handler's own default is the associated constant"
    );
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
        AuditCaptureConfig::default(),
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
    let (run, out) = super::super::identification_reporting_tests::capture_tracing(|| {
        run_batch(
            &[deposit_request(&source, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            1,
            &[],
            None,
            AuditCaptureConfig::default(),
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
        AuditCaptureConfig::default(),
        seeded_db(APPROVED_IMPL, 999),
    );
    assert_eq!(run.results[0].as_ref().unwrap().verdict, Verdict::Allow);
    assert_eq!(run.results[1].as_ref().unwrap().verdict, Verdict::Allow);
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
        AuditCaptureConfig::default(),
        db,
    );
    assert_eq!(run.results[0].as_ref().unwrap().verdict, Verdict::Malformed);
    assert_eq!(run.results[1].as_ref().unwrap().verdict, Verdict::Allow);
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
        AuditCaptureConfig::default(),
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

    let (run, out) = super::super::identification_reporting_tests::capture_tracing(|| {
        run_batch(
            &[deposit_request(&prefix, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            1,
            &[&prefix],
            Some(kinds.clone()),
            AuditCaptureConfig::default(),
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
    let (control, control_out) =
        super::super::identification_reporting_tests::capture_tracing(|| {
            run_batch(
                &[deposit_request(&prefix, PROXY)],
                vec![proxy_impl_allowlist_rule()],
                1,
                &[&prefix],
                Some(kinds),
                AuditCaptureConfig::default(),
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
        "an_over_budget_inferred_synthetic_ban_prefix_is_committed_and_reports_budget_exceeded",
        inferred_synthetic_ban_prefix_over_budget,
    );
}

/// The golden one-element ban, sent to `PROXY`. The proxy ignores its calldata and performs the
/// delegatecalls, so the classification rests on the deposit's own `source_hash` and `input`.
fn golden_ban_to_proxy() -> DepositTxRequest {
    let ban = golden_bans().into_iter().find(|ban| ban.n == 1).expect("an n = 1 vector");
    DepositTxRequest { data: ban.data, ..deposit_request(&ban.source_hash, PROXY) }
}

fn inferred_synthetic_ban_prefix_over_budget() {
    let req = golden_ban_to_proxy();
    let source = req.source_hash.clone();

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _guard = metrics::set_default_local_recorder(&recorder);

    let (run, out) = super::super::identification_reporting_tests::capture_tracing(|| {
        run_batch(
            std::slice::from_ref(&req),
            vec![proxy_impl_allowlist_rule()],
            1,
            &[&source],
            // No label map at all: the class is decided by the content-binding check.
            None,
            AuditCaptureConfig::default(),
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

    let (control, control_out) =
        super::super::identification_reporting_tests::capture_tracing(|| {
            run_batch(
                std::slice::from_ref(&req),
                vec![proxy_impl_allowlist_rule()],
                1,
                &[&source],
                None,
                AuditCaptureConfig::default(),
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
        AuditCaptureConfig::default(),
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
        AuditCaptureConfig::default(),
        log0_db(101),
    );
    assert_eq!(
        only(&over).verdict,
        Verdict::Malformed,
        "101 logs × 101 logs = 10 201 bindings exceeds the budget before enumeration"
    );
    assert_eq!(over.depositor_nonce, 0, "an over-budget transaction commits nothing");
}
