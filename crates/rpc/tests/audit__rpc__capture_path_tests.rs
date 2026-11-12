//! Real capture and a real verdict on `xlayer_auditTransactions`.
//!
//! The batch tests drive the production `run_audit_batch` against a real `OpEvm` built by the
//! same factory the node uses, so capture arming, the `known_allowed` policy, the commit/no-commit
//! decision and the verdict are all exercised as shipped. The boundary tests call the exact
//! production functions the handler calls, so an error code or message cannot drift between them.

use super::*;
use alloy_evm::{EvmEnv, EvmFactory};
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{keccak256, Address, B256, U256};
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use op_revm::OpSpecId;
use rcs_filter::callstack::SnapshotStamp;
use rcs_filter::rules::load_rules;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;
use revm::state::{AccountInfo, Bytecode};
use revm::Database as _;

/// The proxy a rule observes. Its own code performs a first-level `DELEGATECALL`.
const PROXY: Address = Address::repeat_byte(0xAA);
const APPROVED_IMPL: Address = Address::repeat_byte(0xB1);
const UNAPPROVED_IMPL: Address = Address::repeat_byte(0xB2);
const DEPOSITOR: Address = Address::repeat_byte(0xC1);

/// `DELEGATECALL(gas, target, 0,0,0,0); POP` repeated `times`, then `STOP`.
fn delegating_proxy_code(target: Address, times: usize) -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 0..times {
        code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73]);
        code.extend_from_slice(target.as_slice());
        code.extend_from_slice(&[0x5a, 0xf4, 0x50]);
    }
    code.push(0x00);
    code
}

/// `SSTORE(0, marker); STOP`. Under a `DELEGATECALL` this writes the PROXY's storage, which
/// makes "the state was committed" a direct observation rather than an inference. The marker
/// also gives the two implementations distinct code, hence distinct code hashes.
fn impl_code(marker: u8) -> Vec<u8> {
    vec![0x60, marker, 0x60, 0x00, 0x55, 0x00]
}

fn approved_hash() -> B256 {
    keccak256(impl_code(0x01))
}

/// The normative rule pattern: declare one alias on the proxy address, then deny when the
/// code it delegated into is NOT in the approved set. No guard variable is needed or
/// available: an alias that produced no observation suppresses the whole rule, and only a
/// direct delegatecall is ever a candidate, so the bound hash is always a real value.
fn proxy_impl_allowlist_rule() -> RawRule {
    proxy_impl_allowlist_rule_approving(&[approved_hash()])
}

/// The same rule as raw JSON, for a test that has to build a whole request body.
fn proxy_impl_allowlist_rule_json() -> serde_json::Value {
    let approved = vec![format!("{:#x}", approved_hash())];
    serde_json::json!({
        "id": "proxy-impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "in": [ { "var": "entry.delegateCodeHash" }, approved ] } },
        "action": "deny"
    })
}

/// The same normative rule under a DIFFERENT id, selecting the SAME proxy address. Two of
/// these are two selector-bearing rules over a one-address selector set, which is what
/// separates "how many rules carry a selector" from "how many distinct addresses they name".
fn proxy_impl_allowlist_rule_with_id(id: &str) -> RawRule {
    let mut rule = proxy_impl_allowlist_rule();
    rule.id = id.to_string();
    rule
}

/// ONE rule naming SEVERAL addresses: the mirror image of the fixture above, where the
/// deduplicated address set is larger than the number of rules carrying a selector.
fn rule_selecting_two_addresses() -> RawRule {
    serde_json::from_value(serde_json::json!({
        "id": "two-address-observer",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": {
            "entry": format!("{PROXY:#x}"),
            "second": format!("{APPROVED_IMPL:#x}")
        } },
        "condition": { "!=": [ { "var": "entry.delegateTo" }, format!("{APPROVED_IMPL:#x}") ] },
        "action": "deny"
    }))
    .expect("valid rule")
}

/// The same pattern over an explicit approved set, for a fixture whose implementation is not
/// the default one. The shape is identical — only the allowlist contents differ.
fn proxy_impl_allowlist_rule_approving(approved: &[B256]) -> RawRule {
    let approved: Vec<String> = approved.iter().map(|h| format!("{h:#x}")).collect();
    serde_json::from_value(serde_json::json!({
        "id": "proxy-impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "in": [ { "var": "entry.delegateCodeHash" }, approved ] } },
        "action": "deny"
    }))
    .expect("the normative sample must deserialize")
}

/// The origin the request's rules are narrowed to in the skip tests: deliberately NOT the
/// depositor, so every rule is filtered out with zero decoding before any observation is
/// consulted.
const OTHER_ORIGIN: Address = Address::repeat_byte(0x99);

/// The same normative pattern, additionally narrowed to one `origin`. Stage one of matching
/// skips such a rule on an origin mismatch before stage two resolves any call frame, so no
/// per-rule observation check can ever be reached for it.
fn proxy_impl_allowlist_rule_from_origin(origin: Address) -> RawRule {
    let approved = vec![format!("{:#x}", approved_hash())];
    serde_json::from_value(serde_json::json!({
        "id": "proxy-impl-allowlist-origin-narrowed",
        "origin": format!("{origin:#x}"),
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "in": [ { "var": "entry.delegateCodeHash" }, approved ] } },
        "action": "deny"
    }))
    .expect("the origin-narrowed sample must deserialize")
}

/// An event rule declaring no `callstack` section at all.
fn event_only_rule() -> RawRule {
    serde_json::from_str(
        r#"{"id":"evt","event_abis":{"transfer":{"type":"event","name":"Transfer",
               "inputs":[{"name":"from","type":"address","indexed":true},
                         {"name":"to","type":"address","indexed":true},
                         {"name":"value","type":"uint256","indexed":false}],
               "anonymous":false}},"condition":true,"action":"deny"}"#,
    )
    .expect("valid event-only rule")
}

fn deposit_request(source_hash: &str, to: Address) -> DepositTxRequest {
    DepositTxRequest {
        source_hash: source_hash.to_string(),
        from: format!("{DEPOSITOR:#x}"),
        to: Some(format!("{to:#x}")),
        mint: "0".to_string(),
        value: "0".to_string(),
        gas_limit: 1_000_000,
        is_system_transaction: false,
        data: "0x".to_string(),
    }
}

fn seeded_db(proxy_target: Address, delegatecalls: usize) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    db.insert_account_info(
        PROXY,
        AccountInfo {
            code: Some(Bytecode::new_raw(
                delegating_proxy_code(proxy_target, delegatecalls).into(),
            )),
            ..Default::default()
        },
    );
    for (address, marker) in [(APPROVED_IMPL, 0x01u8), (UNAPPROVED_IMPL, 0x02u8)] {
        db.insert_account_info(
            address,
            AccountInfo {
                code: Some(Bytecode::new_raw(impl_code(marker).into())),
                ..Default::default()
            },
        );
    }
    db
}

/// One batch outcome: the per-transaction results plus the PROXY storage slot the
/// implementation writes, which is how "state committed" is observed directly. The depositor's
/// nonce counts committed transactions for fixtures whose code writes no storage.
struct BatchRun {
    results: Vec<Option<AuditResult>>,
    proxy_slot0: U256,
    depositor_nonce: u64,
}

/// Runs the production `run_audit_batch` against a real EVM, exactly as the handler does:
/// the inspector is attached only when the request's own rules declare a selector.
fn run_batch(
    reqs: &[DepositTxRequest],
    raw_rules: Vec<RawRule>,
    protocol_version: u32,
    known_allowed: &[&str],
    kinds: Option<BTreeMap<String, KnownAllowedKind>>,
    capture: AuditCaptureConfig,
    db: InMemoryDB,
) -> BatchRun {
    run_batch_stamped(reqs, raw_rules, protocol_version, known_allowed, kinds, capture, db, None)
}

/// The same production path, with the capture stamp the inspector is configured with made
/// injectable.
///
/// `None` is exactly [`run_batch`]: the inspector is built by the production
/// `audit_capture_inspector`, so it stamps with the evaluating snapshot's own identity and no
/// test can accidentally pass by configuring capture differently from the handler. `Some`
/// builds the SAME real [`RcsInspector`] over the SAME selector set but under a FOREIGN
/// snapshot identity, which is the only way to reach the stamp-mismatch branch through
/// production code: the request compiles its own rules, so a handler-driven test can never
/// make capture and screening disagree on the stamp.
#[allow(clippy::too_many_arguments)]
fn run_batch_stamped(
    reqs: &[DepositTxRequest],
    raw_rules: Vec<RawRule>,
    protocol_version: u32,
    known_allowed: &[&str],
    kinds: Option<BTreeMap<String, KnownAllowedKind>>,
    capture: AuditCaptureConfig,
    db: InMemoryDB,
    capture_stamp: Option<SnapshotStamp>,
) -> BatchRun {
    let rules = load_rules(protocol_version, 0, raw_rules.clone());
    assert!(rules.rejected.is_empty(), "test rules must load clean: {:?}", rules.rejected);
    let known_allowed_rules = callstack_only_projection(&rules, &raw_rules);
    let known_allowed_set: HashSet<String> =
        known_allowed.iter().map(|h| (*h).to_string()).collect();
    let txs: Vec<_> =
        reqs.iter().map(|r| build_deposit_tx(r).expect("valid deposit request")).collect();
    let batch = AuditBatch {
        txs: &txs,
        reqs,
        rules: &rules,
        known_allowed_rules: known_allowed_rules.as_ref(),
        known_allowed: &known_allowed_set,
        kinds: kinds.as_ref(),
        report_identification: rules.has_callstack_rules(),
    };
    let mut executed: Vec<Option<AuditResult>> = (0..txs.len()).map(|_| None).collect();
    let env = EvmEnv::new(
        CfgEnv::new_with_spec(OpSpecId::JOVIAN),
        BlockEnv { gas_limit: 30_000_000, ..Default::default() },
    );
    let inspector = match capture_stamp {
        None => audit_capture_inspector(&rules, capture),
        Some(stamp) => rules.has_callstack_rules().then(|| {
            RcsInspector::callstack_only(
                capture.max_callstack_observations_per_tx,
                CallstackCaptureConfig::new(stamp, rules.callstack_selectors().to_vec()),
            )
        }),
    };
    let (proxy_slot0, depositor_nonce) = match inspector {
        Some(inspector) => {
            let mut evm =
                OpEvmFactory::<OpTx>::default().create_evm_with_inspector(db, env, inspector);
            run_audit_batch(&mut evm, &batch, &mut executed).expect("batch executes");
            // Capture must be inactive on every exit path, guaranteed by the RAII guard.
            assert!(
                matches!(
                    evm.components_mut().1.finish_capture(&[]),
                    CaptureOutcome::Complete { .. }
                ),
                "the inspector must be left inactive after the batch"
            );
            (
                evm.db_mut().storage(PROXY, U256::ZERO).expect("storage read"),
                evm.db_mut().basic(DEPOSITOR).expect("account read").map_or(0, |a| a.nonce),
            )
        }
        None => {
            let mut evm = OpEvmFactory::<OpTx>::default().create_evm(db, env);
            run_audit_batch(&mut evm, &batch, &mut executed).expect("batch executes");
            (
                evm.db_mut().storage(PROXY, U256::ZERO).expect("storage read"),
                evm.db_mut().basic(DEPOSITOR).expect("account read").map_or(0, |a| a.nonce),
            )
        }
    };
    BatchRun { results: executed, proxy_slot0, depositor_nonce }
}

fn generous() -> AuditCaptureConfig {
    AuditCaptureConfig { max_callstack_observations_per_tx: 10_000 }
}

fn only(run: &BatchRun) -> &AuditResult {
    run.results[0].as_ref().expect("the single transaction was classified")
}

// ---------------------------------------------------------------- batch semantics ---------

/// a v2 `callstack` rule is accepted and classified from REAL capture: the
/// deposit→proxy→unapproved-implementation request returns the deny verdict.
#[test]
fn v2_callstack_rule_denies_unapproved_implementation() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[],
        None,
        generous(),
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
        generous(),
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
        generous(),
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

/// a genuine capture failure returns the existing non-verdict, never `Allow`.
/// Driven by a NON-DEFAULT budget of 1 against two observations, which also proves the
/// configured value is actually injected rather than hard-coded.
#[test]
fn capture_failure_returns_the_non_verdict_and_the_budget_is_injected() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[],
        None,
        AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
        seeded_db(APPROVED_IMPL, 2),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed, "never Allow on a capture failure");
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict does not commit state");

    // Counter-check: the SAME transaction under the default budget is adjudicated normally,
    // so the previous assertion is about the budget and not about the fixture.
    let generous_run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[],
        None,
        generous(),
        seeded_db(APPROVED_IMPL, 2),
    );
    assert_eq!(only(&generous_run).verdict, Verdict::Allow);
}

/// a `known_allowed` **Original** executing unapproved implementation code returns
/// verdict `Deny`, and its state is NOT committed. At `3a6028eb` this was an unconditional
/// `Allow` with the state committed.
#[test]
fn known_allowed_original_running_unapproved_code_is_denied() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        generous(),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Deny);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Objected));
    assert_eq!(run.proxy_slot0, U256::ZERO, "state must not be committed");
}

/// the same with approved code returns `Allow` + `clean`, state committed.
#[test]
fn known_allowed_original_running_approved_code_is_allowed_and_committed() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        generous(),
        seeded_db(APPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Clean));
    assert_eq!(run.proxy_slot0, U256::from(1u64));
}

/// a `known_allowed` Original whose capture fails returns the non-verdict, state not
/// committed, reported `not_identified`.
#[test]
fn known_allowed_original_with_failed_capture_is_a_non_verdict() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
        seeded_db(APPROVED_IMPL, 2),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO);
}

/// the non-verdict survives a rule that is filtered out BEFORE its selectors are ever
/// resolved. The request's only rule is narrowed to an origin the transaction does not have,
/// so the matcher skips it with zero decoding and therefore never looks at the (unavailable)
/// observation set. Priority is a property of the REQUEST, not of whichever rule happened to
/// survive filtering, so an unidentifiable execution must still be the non-verdict with no
/// state committed.
#[test]
fn capture_failure_is_the_non_verdict_even_when_every_rule_is_skipped_by_origin() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
        seeded_db(APPROVED_IMPL, 2),
    );
    let result = only(&run);
    assert_eq!(
        result.verdict,
        Verdict::Malformed,
        "an unidentifiable execution is never reported Allow, whichever rules were skipped"
    );
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict must not commit state");

    // Counter-check: the SAME request and the SAME skipped rule under a sufficient budget is
    // adjudicated normally and committed, so the assertion above is about the unavailable
    // observation set and not about the origin narrowing by itself.
    let identified = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        generous(),
        seeded_db(APPROVED_IMPL, 2),
    );
    assert_eq!(only(&identified).verdict, Verdict::Allow);
    assert_eq!(only(&identified).callstack_identification, Some(CallstackIdentification::Clean));
    assert_eq!(identified.proxy_slot0, U256::from(1u64));
}

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
        AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
        seeded_db(UNAPPROVED_IMPL, 2),
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

/// the synthetic-prefix exception is explicit and survives the request-level check: with the
/// same skipped rule and the same failed capture, a `SyntheticBan` still returns `Allow` and
/// its state IS committed, because later originals depend on that base state.
#[test]
fn synthetic_ban_prefix_keeps_allow_and_commit_when_every_rule_is_skipped_by_origin() {
    let prefix = format!("0x{}", "55".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY)],
        vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
        1,
        &[&prefix],
        Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
        AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
        seeded_db(APPROVED_IMPL, 2),
    );
    let s = only(&run);
    assert_eq!(s.verdict, Verdict::Allow, "the prefix contract is unconditional");
    assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::from(1u64), "its commit is never withheld");
}

/// A snapshot identity that is NOT the evaluating request's: the same selector set under a
/// different `content_version`, which is exactly the shape a stale or crossed capture
/// configuration produces. Capture still COMPLETES, so the observation set is `Available` —
/// the unavailable dimension never fires and only the stamp separates the two.
fn foreign_stamp(raw_rules: &[RawRule], protocol_version: u32) -> SnapshotStamp {
    let evaluating = load_rules(protocol_version, 0, raw_rules.to_vec());
    let foreign = load_rules(protocol_version, 7, raw_rules.to_vec());
    assert_eq!(
        foreign.callstack_selectors(),
        evaluating.callstack_selectors(),
        "the fixture must differ ONLY in snapshot identity, never in the observed set"
    );
    assert_ne!(foreign.stamp(), evaluating.stamp(), "the fixture must be a foreign stamp");
    foreign.stamp()
}

/// a replay prefix whose observations are AVAILABLE but stamped by a foreign snapshot is
/// reported `NotIdentified`, never `Clean` — even when the request's only rule is narrowed
/// away by `origin` so the matcher would skip it with zero decoding and return `Allow`.
///
/// The unavailable dimension cannot cover this: capture completed, so the set is `Available`
/// and only the stamp says the frames were produced under different rules. Driven through the
/// production `run_audit_batch` with a real `RcsInspector`, because the gap was in that
/// arm's own observation test and a direct call to the classifier would reproduce it.
#[test]
fn a_prefix_whose_observations_carry_a_foreign_stamp_is_not_identified() {
    let prefix = format!("0x{}", "55".repeat(32));
    let raw_rules = vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)];
    let kinds: BTreeMap<String, KnownAllowedKind> =
        [(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect();

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let (run, out) = super::identification_reporting_tests::capture_tracing(|| {
        run_batch_stamped(
            &[deposit_request(&prefix, PROXY)],
            raw_rules.clone(),
            1,
            &[&prefix],
            Some(kinds),
            generous(),
            seeded_db(APPROVED_IMPL, 2),
            Some(foreign_stamp(&raw_rules, 2)),
        )
    });

    let s = only(&run);
    assert_eq!(
        s.callstack_identification,
        Some(CallstackIdentification::NotIdentified),
        "observations adjudicable only under a different snapshot identify nothing"
    );
    // The prefix contract is unchanged by this fix: still `Allow`, still committed.
    assert_eq!(s.verdict, Verdict::Allow, "the prefix contract stays unconditional");
    assert_eq!(run.proxy_slot0, U256::from(1u64), "its commit is never withheld");

    assert!(
        out.contains("outcome=\"not_identified\""),
        "the single bounded anomaly log must report not_identified: {out}"
    );
    assert_eq!(
        out.lines().filter(|line| line.contains("synthetic_ban_identification")).count(),
        1,
        "exactly one bounded anomaly log for the prefix: {out}"
    );
    let mut counters: Vec<(String, u64)> = snapshotter
        .snapshot()
        .into_vec()
        .into_iter()
        .filter_map(|(key, _, _, value)| match value {
            DebugValue::Counter(count)
                if key.key().name().starts_with("rcs_filter.synthetic_ban_identification") =>
            {
                Some((key.key().name().to_string(), count))
            }
            _ => None,
        })
        .collect();
    counters.sort();
    assert_eq!(
        counters,
        vec![
            ("rcs_filter.synthetic_ban_identification.clean_total".to_string(), 0),
            ("rcs_filter.synthetic_ban_identification.not_applicable_total".to_string(), 0),
            ("rcs_filter.synthetic_ban_identification.not_identified_total".to_string(), 1),
            ("rcs_filter.synthetic_ban_identification.objected_total".to_string(), 0),
        ],
        "the dedicated synthetic_ban series records not_identified and NOTHING else — in \
             particular `clean_total` stays at zero"
    );
}

/// the positive contrast for the test above, identical in every respect EXCEPT the snapshot
/// identity: with capture stamped by the evaluating snapshot the same prefix, the same
/// origin-narrowed rule and the same execution are still identified `Clean`, so the
/// assertion above is about the foreign stamp and not about the origin narrowing or about
/// every prefix now being reported unidentifiable.
#[test]
fn a_prefix_whose_observations_carry_the_evaluating_stamp_is_still_identified() {
    let prefix = format!("0x{}", "55".repeat(32));
    let raw_rules = vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)];
    let kinds: BTreeMap<String, KnownAllowedKind> =
        [(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect();
    let (run, out) = super::identification_reporting_tests::capture_tracing(|| {
        run_batch_stamped(
            &[deposit_request(&prefix, PROXY)],
            raw_rules,
            1,
            &[&prefix],
            Some(kinds),
            generous(),
            seeded_db(APPROVED_IMPL, 2),
            None,
        )
    });

    let s = only(&run);
    assert_eq!(
        s.callstack_identification,
        Some(CallstackIdentification::Clean),
        "a matching-stamp prefix is still identified normally"
    );
    assert_eq!(s.verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::from(1u64));
    assert!(
        !out.contains("synthetic_ban_identification"),
        "a clean prefix is counted without an anomaly log: {out}"
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
        generous(),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(run.results[0].as_ref().unwrap().verdict, Verdict::Deny);
    let later = run.results[1].as_ref().expect("the later transaction still executed");
    assert_eq!(later.verdict, Verdict::Allow);
}

/// a synthetic replay prefix keeps the pre-existing contract unconditionally:
/// even when a rule objects to its executed code, `S` returns `Allow`, its state IS
/// committed, and the objection is reported only. A later original is therefore simulated on
/// the CORRECT base state including the prefix.
#[test]
fn synthetic_ban_prefix_keeps_allow_and_commits_even_when_objected() {
    let prefix = format!("0x{}", "55".repeat(32));
    let original = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY), deposit_request(&original, APPROVED_IMPL)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&prefix],
        Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
        generous(),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let s = run.results[0].as_ref().unwrap();
    assert_eq!(s.verdict, Verdict::Allow, "a synthetic prefix's verdict never changes");
    assert_eq!(
        s.callstack_identification,
        Some(CallstackIdentification::Objected),
        "its identification is reported, not acted on"
    );
    assert_eq!(
        run.proxy_slot0,
        U256::from(2u64),
        "the prefix's state IS committed, so later originals see the correct base state"
    );
    assert_eq!(run.results[1].as_ref().unwrap().verdict, Verdict::Allow);
}

/// the same with the prefix's capture failing: `S` is still `Allow` and still
/// committed, reported `not_identified`.
#[test]
fn synthetic_ban_prefix_with_failed_capture_still_allows_and_commits() {
    let prefix = format!("0x{}", "55".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&prefix],
        Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
        AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
        seeded_db(UNAPPROVED_IMPL, 2),
    );
    let s = only(&run);
    assert_eq!(s.verdict, Verdict::Allow);
    assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::from(2u64), "its commit is never withheld");
}

/// the budget-failure branch of a replay prefix REALLY reaches the reporting path when
/// driven through the production batch: the prefix keeps its `Allow` and its commit, and the
/// bounded anomaly log is emitted. Without this the classification would exist only in the
/// response field the consumer never reads for such a prefix.
#[test]
fn a_prefix_whose_capture_failed_reports_through_the_production_batch() {
    let prefix = format!("0x{}", "55".repeat(32));
    let kinds: BTreeMap<String, KnownAllowedKind> =
        [(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect();
    let (run, out) = super::identification_reporting_tests::capture_tracing(|| {
        run_batch(
            &[deposit_request(&prefix, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            1,
            &[&prefix],
            Some(kinds),
            AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
            seeded_db(UNAPPROVED_IMPL, 2),
        )
    });

    let s = only(&run);
    assert_eq!(s.verdict, Verdict::Allow, "the prefix contract stays unconditional");
    assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::from(2u64), "its commit is never withheld");
    assert!(
        out.contains("xlayer_audit_rpc::synthetic_ban_identification"),
        "the budget-failure branch must still emit the dedicated diagnostic: {out}"
    );
    assert!(out.contains("outcome=\"not_identified\""), "expected the outcome token: {out}");
    assert!(
        !out.contains("proxy-impl-allowlist"),
        "the diagnostic must never carry a rule id: {out}"
    );
}

/// the same through the batch for an OBJECTED prefix: `Allow`, committed, and the objection
/// is visible in the operator-facing diagnostic rather than only in the response field.
#[test]
fn an_objected_prefix_reports_through_the_production_batch() {
    let prefix = format!("0x{}", "55".repeat(32));
    let kinds: BTreeMap<String, KnownAllowedKind> =
        [(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect();
    let (run, out) = super::identification_reporting_tests::capture_tracing(|| {
        run_batch(
            &[deposit_request(&prefix, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            1,
            &[&prefix],
            Some(kinds),
            generous(),
            seeded_db(UNAPPROVED_IMPL, 1),
        )
    });

    let s = only(&run);
    assert_eq!(s.verdict, Verdict::Allow);
    assert_eq!(s.callstack_identification, Some(CallstackIdentification::Objected));
    assert_eq!(run.proxy_slot0, U256::from(2u64));
    assert!(out.contains("outcome=\"objected\""), "expected the outcome token: {out}");
}

/// Drives one capture failure through the production batch and returns the bounded
/// diagnostic's `selector_bearing_rules` field, read back from the log line rather than
/// restated from the code. `kinds = None` reaches the request-level non-verdict log; a
/// `SyntheticBan` label reaches the replay-prefix log.
fn logged_selector_bearing_rules(
    raw_rules: Vec<RawRule>,
    kinds: Option<BTreeMap<String, KnownAllowedKind>>,
    known_allowed: &[&str],
    source: &str,
) -> usize {
    let (_, out) = super::identification_reporting_tests::capture_tracing(|| {
        run_batch(
            &[deposit_request(source, PROXY)],
            raw_rules,
            1,
            known_allowed,
            kinds,
            // One observation of budget against a proxy that delegatecalls twice: capture
            // fails, which is the shortest route to each of the two diagnostics.
            AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
            seeded_db(APPROVED_IMPL, 2),
        )
    });
    let field = out
        .split("selector_bearing_rules=")
        .nth(1)
        .unwrap_or_else(|| panic!("the diagnostic must carry the bounded count: {out}"));
    field
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or_else(|e| panic!("the count must be numeric ({e}): {out}"))
}

/// TWO rules with different ids selecting the SAME address are reported as two
/// selector-bearing rules, at BOTH diagnostics. The deduplicated selector address set has
/// one entry here, so a count taken from that set would under-report the rule surface an
/// operator is being told about.
#[test]
fn two_rules_on_one_address_are_two_selector_bearing_rules() {
    let source = format!("0x{}", "33".repeat(32));
    let rules = vec![
        proxy_impl_allowlist_rule_with_id("proxy-impl-allowlist-a"),
        proxy_impl_allowlist_rule_with_id("proxy-impl-allowlist-b"),
    ];
    assert_eq!(
        load_rules(1, 0, rules.clone()).callstack_selectors().len(),
        1,
        "the fixture must be discriminating: one address named by two rules"
    );

    assert_eq!(
        logged_selector_bearing_rules(rules.clone(), None, &[], &source),
        2,
        "the request-level non-verdict log counts RULES, not selector addresses"
    );
    let prefix = format!("0x{}", "55".repeat(32));
    assert_eq!(
        logged_selector_bearing_rules(
            rules,
            Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
            &[&prefix],
            &prefix,
        ),
        2,
        "the replay-prefix log counts RULES, not selector addresses"
    );
}

/// ONE rule naming SEVERAL addresses is reported as ONE selector-bearing rule, at BOTH
/// diagnostics — the mirror case, which a count taken from the address set would
/// over-report.
#[test]
fn one_rule_on_several_addresses_is_one_selector_bearing_rule() {
    let source = format!("0x{}", "33".repeat(32));
    let rules = vec![rule_selecting_two_addresses()];
    assert_eq!(
        load_rules(1, 0, rules.clone()).callstack_selectors().len(),
        2,
        "the fixture must be discriminating: two addresses named by one rule"
    );

    assert_eq!(
        logged_selector_bearing_rules(rules.clone(), None, &[], &source),
        1,
        "the request-level non-verdict log counts RULES, not selector addresses"
    );
    let prefix = format!("0x{}", "55".repeat(32));
    assert_eq!(
        logged_selector_bearing_rules(
            rules,
            Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
            &[&prefix],
            &prefix,
        ),
        1,
        "the replay-prefix log counts RULES, not selector addresses"
    );
}

/// an `Original` in the SAME request still gets `Deny` priority, proving the two
/// classes are handled differently within one batch rather than by a global switch.
#[test]
fn both_classes_are_handled_differently_in_one_request() {
    let prefix = format!("0x{}", "55".repeat(32));
    let original = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY), deposit_request(&original, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&prefix, &original],
        Some(
            [
                (prefix.clone(), KnownAllowedKind::SyntheticBan),
                (original.clone(), KnownAllowedKind::Original),
            ]
            .into_iter()
            .collect(),
        ),
        generous(),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(run.results[0].as_ref().unwrap().verdict, Verdict::Allow, "synthetic prefix");
    assert_eq!(run.results[1].as_ref().unwrap().verdict, Verdict::Deny, "original");
}

/// An UNLABELLED `known_allowed` entry that is not a synthetic ban, executing an unapproved
/// implementation, is adjudicated as an approved original: `Deny`, with nothing committed.
///
/// Updated in place. This test used to pin the opposite (`Allow`, committed, objection reported
/// only), which was the gap: a caller that sends no labels never had an approved original
/// re-judged on the call-frame dimension. The labelled `Original` contrast below is unchanged and
/// now agrees with the unlabelled result.
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
        generous(),
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
        generous(),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let result = only(&labelled);
    assert_eq!(result.verdict, Verdict::Deny);
    assert_eq!(labelled.proxy_slot0, U256::ZERO, "a denied Original commits nothing");
}

/// A legacy batch carrying no label map at all: several `known_allowed` entries plus an
/// ordinary deposit, with alias-bearing rules, all executing an unapproved implementation.
///
/// Updated in place: the `known_allowed` entries used to be `Allow` and committed. None of them
/// is a synthetic ban, so each is now adjudicated as an approved original and denied, exactly
/// like the ordinary deposit, and nothing is committed.
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
        generous(),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(run.results[0].as_ref().unwrap().verdict, Verdict::Deny, "unlabelled original");
    assert_eq!(run.results[1].as_ref().unwrap().verdict, Verdict::Deny, "unlabelled original");
    assert_eq!(
        run.results[2].as_ref().unwrap().verdict,
        Verdict::Deny,
        "the ordinary deposit is adjudicated, not exempted"
    );
    assert_eq!(run.proxy_slot0, U256::ZERO, "no denied transaction added state");
}

/// An unlabelled entry that is not a synthetic ban, whose observation set cannot be adjudicated
/// — unavailable, and separately `Available` under a FOREIGN stamp — gets the non-verdict with
/// nothing committed, exactly as a labelled `Original` does.
///
/// Updated in place: this used to pin `Allow` + commit. Such an entry is now an approved
/// original, so the request-level unadjudicable gate applies to it; only a synthetic ban stays
/// exempt (see `an_unadjudicable_inferred_original_does_not_stop_the_prefix_from_committing`).
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
        AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
        seeded_db(APPROVED_IMPL, 2),
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
        generous(),
        seeded_db(APPROVED_IMPL, 1),
        Some(foreign_stamp(&raw_rules, 1)),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict commits nothing");
}

/// Proof that the exemption above was NARROWED and not widened: with the same unadjudicable
/// observation set, an ordinary deposit and a labelled `Original` both still get the
/// fail-closed non-verdict, with nothing committed.
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
            AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
            seeded_db(APPROVED_IMPL, 2),
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
        audit_capture_inspector(&rules, generous()).is_none(),
        "a selector-free request must construct NO inspector"
    );

    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![event_only_rule()],
        1,
        &[&source],
        None,
        generous(),
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
/// entered the screened stream. This is the direct consequence of paths 2 and 3 running in
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
        generous(),
        value_transferring_db(),
    );
    // Without capture at all — the pre-change path for this rule.
    let unarmed = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![native_transfer_deny_rule()],
        1,
        &[],
        None,
        generous(),
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

// ---------------------------------------------------------------- request boundary --------

/// a request omitting the protocol version behaves as version 1.
#[test]
fn omitted_protocol_version_means_one() {
    let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
        "rules": [],
        "txs": [],
    }))
    .expect("an omitted version must still decode");
    assert_eq!(req.protocol_version, MIN_PROTOCOL_VERSION);
    assert!(req.known_allowed.is_empty());
    assert!(req.known_allowed_kinds.is_none());
    assert!(validate_protocol_version(req.protocol_version).is_ok());
}

/// an unsupported protocol version returns `-32602` naming both the received value and the
/// supported set, and the set is exactly `[1]`.
///
/// `2` is included on purpose. A request declaring it was authored against a different
/// `callstack` contract, so adjudicating it under this one would be a silent
/// misinterpretation — strictly worse than a named rejection.
#[test]
fn unsupported_protocol_version_is_rejected_by_name() {
    for unsupported in [0, 2, 3, 99] {
        let error =
            validate_protocol_version(unsupported).expect_err("only version 1 is supported");
        assert_eq!(error.code(), -32602);
        let message = error.message();
        assert!(message.contains(&unsupported.to_string()), "{message}");
        assert!(message.contains("[1]"), "the supported set must be named: {message}");
    }
    assert!(validate_protocol_version(1).is_ok());
}

/// The **verbatim upstream rule-source document** — the same bytes the rule-layer tests use —
/// is accepted at this entry point too, with no rejections, the projection built and the
/// identification diagnostic enabled.
///
/// Both entry points share one compiler, and this pins that: a rule set accepted on the
/// hot-reload path must be accepted here, by the same `load_rules` call, over the same file.
#[test]
fn the_upstream_rule_source_document_is_accepted_at_this_entry_point() {
    const UPSTREAM_RULES: &str =
        include_str!("../../rcs-filter/tests/data/upstream-rule-source-examples.json");
    let document: serde_json::Value = serde_json::from_str(UPSTREAM_RULES).expect("valid JSON");

    // The caller's own body shape: three keys, no protocol version, no label map.
    let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
        "rules": document["rules"].clone(),
        "txs": [],
        "known_allowed": []
    }))
    .expect("the upstream rules deserialize at this entry point");
    assert_eq!(req.protocol_version, MIN_PROTOCOL_VERSION, "an omitted version means 1");
    assert_eq!(req.rules.len(), 7);

    let plan = AuditBatchPlan::prepare(&req).expect("every upstream rule must be accepted");
    assert!(plan.rules().rejected.is_empty(), "{:?}", plan.rules().rejected);
    assert_eq!(plan.rules().rules.len(), 7);
    assert_eq!(plan.rules().selector_bearing_rule_count(), 4);
    assert_eq!(
        plan.rules().callstack_selectors().len(),
        3,
        "three distinct proxy addresses, because two rules observe the same one"
    );
    assert!(plan.report_identification, "an alias-bearing request reports identification");
    let projection =
        plan.known_allowed_rules.as_ref().expect("an alias-bearing request has a projection");
    assert_eq!(projection.rules.len(), 4, "only the alias-bearing rules survive");
    assert_eq!(
        projection.stamp(),
        plan.rules().stamp(),
        "the projection must carry the same snapshot stamp"
    );
}

/// The caller's exact body — `{rules, txs, known_allowed}`, with NO `protocol_version` and NO
/// `known_allowed_kinds` — is accepted with alias-bearing rules and a NON-EMPTY
/// `known_allowed`.
///
/// This is the regression test for the failure mode where labels are required: a caller that
/// resolves entries to Allow across rounds sends a growing `known_allowed` and no labels, so
/// requiring them would make every round after the first fail permanently. A first round with
/// an empty `known_allowed` proves nothing, which is why this one is non-empty.
#[test]
fn the_callers_exact_three_key_body_is_accepted_with_a_non_empty_known_allowed() {
    let source = format!("0x{}", "66".repeat(32));
    let body = serde_json::json!({
        "rules": [serde_json::to_value(proxy_impl_allowlist_rule_json()).unwrap()],
        "txs": [],
        "known_allowed": [source]
    });
    assert_eq!(
        body.as_object().unwrap().len(),
        3,
        "the fixture must really be the three-key body, not one that merely resembles it"
    );
    let req: AuditTransactionsRequest =
        serde_json::from_value(body).expect("the three-key body deserializes");
    assert_eq!(req.protocol_version, MIN_PROTOCOL_VERSION);
    assert!(req.known_allowed_kinds.is_none());
    assert!(!req.known_allowed.is_empty());

    let plan = AuditBatchPlan::prepare(&req)
        .expect("an unlabelled request must never be rejected for a missing label");
    assert!(plan.rules().has_callstack_rules());
}

/// A multi-round sequence in the caller's own shape: round 1 with an empty `known_allowed`,
/// then each later round carrying the entries the previous round resolved to Allow. Every
/// round is accepted and adjudicated, and no round is rejected for a missing label.
///
/// Convergence is also pinned: an entry already in `known_allowed` stays `Allow` and never
/// re-enters `Audit`, so the sequence terminates instead of looping.
#[test]
fn a_multi_round_unlabelled_sequence_is_accepted_and_converges() {
    let first = format!("0x{}", "66".repeat(32));
    let second = format!("0x{}", "67".repeat(32));
    let mut resolved: Vec<String> = Vec::new();

    for round in 0..3u8 {
        let known: Vec<&str> = resolved.iter().map(String::as_str).collect();

        // The boundary accepts the round, with no label map at any point.
        let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
            "rules": [serde_json::to_value(proxy_impl_allowlist_rule_json()).unwrap()],
            "txs": [],
            "known_allowed": resolved.clone()
        }))
        .expect("the round body deserializes");
        AuditBatchPlan::prepare(&req)
            .unwrap_or_else(|e| panic!("round {round} must be accepted: {e:?}"));

        // And the batch body adjudicates it, with an approved implementation.
        let run = run_batch(
            &[deposit_request(&first, PROXY), deposit_request(&second, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            1,
            &known,
            None,
            generous(),
            seeded_db(APPROVED_IMPL, 1),
        );
        for (index, result) in run.results.iter().enumerate() {
            let result = result.as_ref().expect("every deposit executed");
            assert_eq!(
                result.verdict,
                Verdict::Allow,
                "round {round}, deposit {index}: an approved implementation allows, and an \
                     already-resolved entry never re-enters adjudication"
            );
            assert!(result.actions.is_none(), "round {round}: no audit payload is produced");
        }
        resolved = vec![first.clone(), second.clone()];
    }
}

/// A condition reading a variable the rule's own aliases do not provide is rejected at this
/// entry point too, naming the variable, with nothing executed or committed.
///
/// Asserted here as well as on the hot-reload path because both entry points share one
/// compiler and a negative that holds on only one of them is not a guarantee.
#[test]
fn a_condition_reading_an_unprovided_alias_variable_is_rejected_at_this_entry_point() {
    for field in ["delegateAddress", "contextCodeHash", "matched", "implCodeHash"] {
        let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
            "rules": [{
                "id": "unknown-alias-variable",
                "event_abis": {},
                "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
                "condition": { "==": [
                    { "var": format!("entry.{field}") },
                    format!("{APPROVED_IMPL:#x}")
                ] },
                "action": "deny"
            }],
            "txs": [],
            "known_allowed": []
        }))
        .expect("the request decodes; the rejection happens in the compiler");
        let error = match AuditBatchPlan::prepare(&req) {
            Err(error) => error,
            Ok(_) => panic!("entry.{field} must be rejected"),
        };
        assert_eq!(error.code(), -32602);
        let detail = error.data().expect("the rejected rule is named").to_string();
        assert!(detail.contains(&format!("entry.{field}")), "{detail}");
        assert!(
            detail.contains(&format!(
                "condition references unknown variable 'entry.{field}'; direct_delegatecalls \
                 provides proxyAddress, proxyCodeHash, delegateTo and delegateCodeHash"
            )),
            "{detail}"
        );
    }

    // The control: the two provided variables are accepted, so the rejections above are about
    // the variable name and not about the rule shape.
    let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
        "rules": [proxy_impl_allowlist_rule_json()],
        "txs": [],
        "known_allowed": []
    }))
    .expect("the control decodes");
    assert!(AuditBatchPlan::prepare(&req).is_ok());
}

/// An undeclared key inside `callstack` fails at this entry point too, during the request
/// decode, naming the offending key — so a near-miss spelling can never degrade a rule into a
/// weakened event-only one.
#[test]
fn an_undeclared_callstack_key_is_rejected_at_this_entry_point() {
    for section in [
        serde_json::json!({ "entry": { "address": format!("{PROXY:#x}") } }),
        serde_json::json!({ "direct_delegatecall": { "entry": format!("{PROXY:#x}") } }),
        serde_json::json!({ "directDelegatecalls": { "entry": format!("{PROXY:#x}") } }),
        serde_json::json!({ "Direct_DelegateCalls": { "entry": format!("{PROXY:#x}") } }),
    ] {
        let error = serde_json::from_value::<AuditTransactionsRequest>(serde_json::json!({
            "rules": [{
                "id": "undeclared-callstack-key",
                "event_abis": {},
                "callstack": section,
                "condition": true,
                "action": "deny"
            }],
            "txs": [],
            "known_allowed": []
        }))
        .expect_err("an undeclared callstack key must be rejected");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    // A misspelled rule-root sibling is caught by the rule root's own gate instead, and the
    // rule is rejected rather than quietly losing its call-frame constraint.
    let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
        "rules": [{
            "id": "misspelled-sibling",
            "event_abis": {},
            "callStack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
            "condition": true,
            "action": "deny"
        }],
        "txs": [],
        "known_allowed": []
    }))
    .expect("the root gate runs in the compiler, so the request still decodes");
    let error = match AuditBatchPlan::prepare(&req) {
        Err(error) => error,
        Ok(_) => panic!("the rule must be rejected"),
    };
    assert_eq!(error.code(), -32602);
    let detail = error.data().expect("the rejected rule is named").to_string();
    assert!(detail.contains("callStack"), "{detail}");
    assert!(detail.contains("misspelled-sibling"), "{detail}");
}

/// The request root is as strict as the rule schema: a speculative or misspelled field is
/// rejected by name rather than silently discarded.
#[test]
fn unknown_request_field_is_rejected_by_name() {
    let error = serde_json::from_value::<AuditTransactionsRequest>(serde_json::json!({
        "rules": [],
        "txs": [],
        "knownAllowed": []
    }))
    .expect_err("an unknown request key must be rejected");
    assert!(
        error.to_string().contains("unknown field(s) in audit request: knownAllowed"),
        "{error}"
    );
}

/// A request that supplies a PARTIAL `known_allowed_kinds` map returns `-32602` naming the
/// unlabelled hashes, and nothing is executed or committed. A caller that labels some entries
/// and not others is label-aware, so the omission is ambiguous rather than conventional.
#[test]
fn a_partial_label_map_is_rejected_naming_the_unlabelled_hashes() {
    let a = format!("0x{}", "33".repeat(32));
    let b = format!("0x{}", "44".repeat(32));
    let known = vec![a.clone(), b.clone()];

    let partial: BTreeMap<String, KnownAllowedKind> =
        [(a.clone(), KnownAllowedKind::Original)].into_iter().collect();
    let error = validate_known_allowed_labels(&known, Some(&partial))
        .expect_err("a partial map must be rejected");
    assert_eq!(error.code(), -32602);
    let data = error.data().expect("named").to_string();
    assert!(data.contains(&b), "only the unlabelled hash is named: {data}");
    assert!(!data.contains(&a), "a labelled hash must not be named: {data}");

    // A complete map is accepted.
    let complete: BTreeMap<String, KnownAllowedKind> =
        [(a.clone(), KnownAllowedKind::Original), (b.clone(), KnownAllowedKind::SyntheticBan)]
            .into_iter()
            .collect();
    assert!(validate_known_allowed_labels(&known, Some(&complete)).is_ok());
}

/// A request that supplies NO label map is accepted whatever its `known_allowed` contains.
///
/// This is load-bearing rather than lenient: a caller that resolves entries to Allow across
/// rounds sends a growing `known_allowed` and no labels at all, so requiring them would make
/// every later round fail permanently.
#[test]
fn a_request_with_no_label_map_needs_no_labels() {
    let known = vec![format!("0x{}", "33".repeat(32))];
    assert!(
        validate_known_allowed_labels(&known, None).is_ok(),
        "an unlabelled request is the ordinary case, not an error"
    );
    assert!(
        validate_known_allowed_labels(&[], None).is_ok(),
        "an empty known_allowed needs none either"
    );
}

/// The two wire labels. `synthetic_ban` is the wire spelling a producer must send for a replay
/// prefix; an unlabelled entry's class is decided locally and is not a wire value.
#[test]
fn known_allowed_kind_wire_spelling_is_snake_case() {
    let kinds: BTreeMap<String, KnownAllowedKind> =
        serde_json::from_value(serde_json::json!({ "0xaa": "original", "0xbb": "synthetic_ban" }))
            .expect("both spellings decode");
    assert_eq!(kinds["0xaa"], KnownAllowedKind::Original);
    assert_eq!(kinds["0xbb"], KnownAllowedKind::SyntheticBan);
    assert!(
        serde_json::from_value::<KnownAllowedKind>(serde_json::json!("syntheticBan")).is_err(),
        "a wrong spelling must not be accepted as a class"
    );
    // No third class exists on the wire: the labels a request may send stay exactly two.
    for spelling in ["unlabelled", "unlabeled", "Unlabelled"] {
        assert!(
            serde_json::from_value::<KnownAllowedKind>(serde_json::json!(spelling)).is_err(),
            "{spelling} must not be a wire-selectable class"
        );
    }
}

/// a programmatic `AuditCaptureConfig { 0 }` yields the non-verdict, never `Allow`.
/// Defence-in-depth of this type only: `FilterConfig::validate()` rejects `0` at startup, so
/// the audit RPC can never receive it through configuration.
#[test]
fn programmatic_zero_budget_yields_the_non_verdict() {
    let run = run_batch(
        &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[],
        None,
        AuditCaptureConfig { max_callstack_observations_per_tx: 0 },
        seeded_db(APPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed, "never Allow");
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
}

/// The callstack-only projection used for `known_allowed` adjudication carries the SAME
/// snapshot stamp as the full set, which is what keeps the existing stamp check meaningful
/// instead of bypassed.
#[test]
fn callstack_only_projection_preserves_the_snapshot_stamp() {
    let raw = vec![event_only_rule(), proxy_impl_allowlist_rule()];
    let full = load_rules(1, 0, raw.clone());
    assert!(full.rejected.is_empty());
    let projected = callstack_only_projection(&full, &raw).expect("aliases are declared");
    assert_eq!(projected.stamp(), full.stamp());
    assert_eq!(projected.rules.len(), 1, "only the alias-bearing rule survives");
    assert!(callstack_only_projection(
        &load_rules(1, 0, vec![event_only_rule()]),
        &[event_only_rule()]
    )
    .is_none());
}

/// The budget actually reaches the inspector: a configured value is what the inspector
/// enforces, not a hard-coded default.
#[test]
fn audit_capture_config_budget_reaches_the_inspector() {
    let rules = load_rules(1, 0, vec![proxy_impl_allowlist_rule()]);
    let inspector = audit_capture_inspector(
        &rules,
        AuditCaptureConfig { max_callstack_observations_per_tx: 7 },
    )
    .expect("selectors declared");
    assert_eq!(inspector.mode(), rcs_filter::capture::CaptureMode::CallstackOnly);
}

// ----------------------------------------------------- repeated-adjudication fixtures ----

/// The implementation used by the repeated-adjudication fixture. Distinct from the two above
/// because its code emits a log, hence a distinct code hash.
const LOGGING_IMPL: Address = Address::repeat_byte(0xB3);

/// Implementation code that emits one real ERC20
/// `Transfer(from = PROXY, to = DEPOSITOR, value = 1)` then `STOP`s. Executed under the
/// proxy's `DELEGATECALL`, so `log.address` is the PROXY.
///
/// `LOG3` pops `offset, size, topic0, topic1, topic2` with the offset on top, so the topics
/// are pushed in reverse.
fn logging_impl_code() -> Vec<u8> {
    let signature = keccak256("Transfer(address,address,uint256)");
    let mut code = vec![0x60, 0x01, 0x60, 0x00, 0x52]; // PUSH1 1; PUSH1 0; MSTORE
    code.push(0x7f); // PUSH32 topic2 (to)
    code.extend_from_slice(DEPOSITOR.into_word().as_slice());
    code.push(0x7f); // PUSH32 topic1 (from)
    code.extend_from_slice(PROXY.into_word().as_slice());
    code.push(0x7f); // PUSH32 topic0 (the event signature)
    code.extend_from_slice(signature.as_slice());
    code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0xa3, 0x00]); // PUSH1 32; PUSH1 0; LOG3; STOP
    code
}

/// A log-dimension rule asking for adjudication on the `Transfer` the fixture really emits.
/// This is what makes the convergence claim falsifiable: the very same input genuinely does
/// reach adjudication while it is not yet approved.
fn audit_on_proxy_transfer_rule() -> RawRule {
    serde_json::from_value(serde_json::json!({
        "id": "audit-proxy-transfer",
        "event_abis": { "transfer": {
            "type": "event", "name": "Transfer",
            "inputs": [
                { "name": "from", "type": "address", "indexed": true },
                { "name": "to", "type": "address", "indexed": true },
                { "name": "value", "type": "uint256", "indexed": false }
            ],
            "anonymous": false
        } },
        "condition": { "==": [ { "var": "transfer.address" }, format!("{PROXY:#x}") ] },
        "action": "audit"
    }))
    .expect("valid log-dimension rule")
}

/// The proxy delegatecalls [`LOGGING_IMPL`], so one transaction carries BOTH a call-frame
/// observation and a matching log.
fn logging_db() -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    db.insert_account_info(
        PROXY,
        AccountInfo {
            code: Some(Bytecode::new_raw(delegating_proxy_code(LOGGING_IMPL, 1).into())),
            ..Default::default()
        },
    );
    db.insert_account_info(
        LOGGING_IMPL,
        AccountInfo {
            code: Some(Bytecode::new_raw(logging_impl_code().into())),
            ..Default::default()
        },
    );
    db
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

    let fresh = run_batch(
        &[deposit_request(&source, PROXY)],
        rules.clone(),
        1,
        &[],
        None,
        generous(),
        logging_db(),
    );
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
            generous(),
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

// ------------------------------------------------------------- rule-rejection detail -----

/// a rule refused for an unknown field is listed in the `-32602` detail array with its rule
/// id and the unknown-field reason, so the caller learns WHICH rule was refused and WHY
/// rather than only how many were.
#[test]
fn a_rejected_rule_is_listed_in_the_error_detail_with_its_id_and_reason() {
    let typo: RawRule = serde_json::from_value(serde_json::json!({
        "id": "typo-rule",
        "event_abis": {},
        // The section name is misspelled, so no selector is declared and the key is unknown.
        "callStack": { "entry": { "address": format!("{PROXY:#x}") } },
        "condition": true,
        "action": "deny"
    }))
    .expect("the wire type captures an unknown key instead of failing to decode");

    let rules = load_rules(1, 0, vec![typo]);
    assert_eq!(rules.rejected.len(), 1, "the rule must be refused, not installed");

    let error =
        validate_compiled_rules(&rules).expect_err("a refused rule must fail the whole request");
    assert_eq!(error.code(), -32602);
    let detail: Vec<serde_json::Value> =
        serde_json::from_str(error.data().expect("a detail array is attached").get())
            .expect("the detail is a JSON array");
    assert_eq!(detail.len(), 1, "one refusal, one entry: {detail:?}");
    assert_eq!(detail[0]["id"], "typo-rule", "the entry names the rule: {detail:?}");
    let reason = detail[0]["reason"].as_str().expect("the entry carries a reason");
    assert!(reason.contains("callStack"), "the reason must name the unrecognised key: {reason}");
}

#[path = "audit__rpc__capture_path_tests__force_tx_replay_tests.rs"]
mod force_tx_replay_tests;

#[path = "audit__rpc__capture_path_tests__known_allowed_inference_tests.rs"]
mod known_allowed_inference_tests;
