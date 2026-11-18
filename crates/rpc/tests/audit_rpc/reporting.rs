//! The request-level non-verdict diagnostics, the synthetic-prefix reporting the consumer cannot
//! see any other way, the selector-bearing rule counts and the resolution metrics.

use std::collections::BTreeMap;

use alloy_primitives::{Address, U256};
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use rcs_filter::test_support::synthetic_ban_golden::golden_bans;
use xlayer_rpc::audit::deposit::DepositTxRequest;
use xlayer_rpc::audit::metrics::SyntheticBanIdentificationMetrics;
use xlayer_rpc::audit::rpc::KnownAllowedKind;
use xlayer_rpc::audit::verdict::{CallstackIdentification, Verdict};

use crate::harness::*;

/// A synthetic-ban request body labelled for `source`, so the prefix's own reporting runs.
fn labelled_ban_request(
    source: String,
    to: Address,
    raw_rules: Vec<rcs_filter::rules::RawRule>,
    db: revm::database::InMemoryDB,
) -> BatchRun {
    run_batch(
        &[deposit_request(&source, to)],
        raw_rules,
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
        db,
    )
}

/// every classification increments its OWN counter, read back from a recorder rather than
/// restated from the code.
#[test]
fn each_classification_increments_its_own_counter() {
    in_isolated_process(module_path!(), "each_classification_increments_its_own_counter", || {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _guard = metrics::set_default_local_recorder(&recorder);

        // Clean: an approved implementation, in budget.
        labelled_ban_request(
            format!("0x{}", "11".repeat(32)),
            PROXY,
            vec![proxy_impl_allowlist_rule()],
            seeded_db(APPROVED_IMPL, 1),
        );
        // Objected: an unapproved implementation.
        labelled_ban_request(
            format!("0x{}", "22".repeat(32)),
            PROXY,
            vec![proxy_impl_allowlist_rule()],
            seeded_db(UNAPPROVED_IMPL, 1),
        );
        // Not identified: over the per-transaction budget.
        labelled_ban_request(
            format!("0x{}", "33".repeat(32)),
            PROXY,
            vec![proxy_impl_allowlist_rule()],
            seeded_db(APPROVED_IMPL, 1_000),
        );
        // Not applicable: a request that declares no call-frame selector.
        labelled_ban_request(
            format!("0x{}", "44".repeat(32)),
            PROXY,
            vec![event_only_rule()],
            seeded_db(APPROVED_IMPL, 1),
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
                ("rcs_filter.synthetic_ban_identification.clean_total".to_string(), 1),
                ("rcs_filter.synthetic_ban_identification.not_applicable_total".to_string(), 1),
                ("rcs_filter.synthetic_ban_identification.not_identified_total".to_string(), 1),
                ("rcs_filter.synthetic_ban_identification.objected_total".to_string(), 1),
            ],
            "each class lands in its own dedicated synthetic_ban series (the dotted key is \
                     the recorder's internal form; the exporter renders it with underscores)"
        );
    });
}

/// an objection on a replay prefix emits exactly the bounded fields — and none of the
/// content a rule id or an observation would carry.
#[test]
fn an_objected_prefix_emits_one_bounded_anomaly_log() {
    let prefix = format!("0x{}", "55".repeat(32));
    let (_, out) = capture_tracing(|| {
        labelled_ban_request(
            prefix,
            PROXY,
            vec![proxy_impl_allowlist_rule()],
            seeded_db(UNAPPROVED_IMPL, 1),
        )
    });

    assert_eq!(
        out.lines()
            .filter(|line| line.contains("xlayer_audit_rpc::synthetic_ban_identification"))
            .count(),
        1,
        "exactly one anomaly log line: {out}"
    );
    assert!(out.contains("ERROR"), "expected error level: {out}");
    assert!(
        out.contains("xlayer_audit_rpc::synthetic_ban_identification"),
        "expected the dedicated target: {out}"
    );
    assert!(out.contains("outcome=\"objected\""), "expected the outcome token: {out}");
    assert!(out.contains("selector_bearing_rules=1"), "expected the bounded count: {out}");
    assert!(
        !out.contains("proxy-impl-allowlist"),
        "the diagnostic must never carry a rule id: {out}"
    );
    assert!(
        !out.to_lowercase().contains("implcodehash"),
        "the diagnostic must never carry observation content: {out}"
    );
    assert!(
        !out.to_lowercase().contains(&format!("{:#x}", Address::repeat_byte(0xAA))),
        "the diagnostic must never carry an observed address: {out}"
    );
}

/// an unidentifiable replay prefix emits the same bounded log under its own outcome token.
#[test]
fn an_unidentified_prefix_emits_one_bounded_anomaly_log() {
    let prefix = format!("0x{}", "55".repeat(32));
    let (_, out) = capture_tracing(|| {
        labelled_ban_request(
            prefix,
            PROXY,
            vec![proxy_impl_allowlist_rule()],
            seeded_db(APPROVED_IMPL, 1_000),
        )
    });

    assert!(
        out.lines()
            .filter(|line| line.contains("xlayer_audit_rpc::synthetic_ban_identification"))
            .count()
            == 1,
        "exactly one anomaly log line: {out}"
    );
    assert!(out.contains("ERROR"), "expected error level: {out}");
    assert!(out.contains("outcome=\"not_identified\""), "expected the outcome token: {out}");
    assert!(out.contains("selector_bearing_rules=1"), "expected the bounded count: {out}");
}

/// a clean (and a not-applicable) prefix is counted and stays SILENT: the log is for
/// anomalies, so an ordinary replay must not fill an operator's log with one line per prefix.
#[test]
fn a_clean_prefix_is_counted_without_an_anomaly_log() {
    let source = format!("0x{}", "55".repeat(32));
    let (_, out) = capture_tracing(|| {
        labelled_ban_request(
            source.clone(),
            PROXY,
            vec![proxy_impl_allowlist_rule()],
            seeded_db(APPROVED_IMPL, 1),
        );
        labelled_ban_request(
            format!("0x{}", "66".repeat(32)),
            PROXY,
            vec![event_only_rule()],
            seeded_db(APPROVED_IMPL, 1),
        );
    });
    assert!(!out.contains("synthetic_ban_identification"), "no anomaly, so no log: {out}");
}

/// the classification → series mapping itself, asserted on the handles so it cannot drift
/// into reporting one class under another's counter.
#[test]
fn the_counter_mapping_is_one_series_per_class() {
    let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
    assert!(std::ptr::eq(metrics.counter(CallstackIdentification::Clean), &metrics.clean_total));
    assert!(std::ptr::eq(
        metrics.counter(CallstackIdentification::Objected),
        &metrics.objected_total
    ));
    assert!(std::ptr::eq(
        metrics.counter(CallstackIdentification::NotIdentified),
        &metrics.not_identified_total
    ));
    assert!(std::ptr::eq(
        metrics.counter(CallstackIdentification::NotApplicable),
        &metrics.not_applicable_total
    ));
}

/// The budget-failure branch of a replay prefix REALLY reaches the reporting path when
/// driven through the production batch: the prefix keeps its `Allow` and its commit, and the
/// bounded anomaly log is emitted. Without this the classification would exist only in the
/// response field the consumer never reads for such a prefix.
#[test]
fn a_prefix_whose_capture_failed_reports_through_the_production_batch() {
    let prefix = format!("0x{}", "55".repeat(32));
    let (run, out) = capture_tracing(|| {
        labelled_ban_request(
            prefix.clone(),
            PROXY,
            vec![proxy_impl_allowlist_rule()],
            seeded_db(UNAPPROVED_IMPL, 1_000),
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
    let (run, out) = capture_tracing(|| {
        labelled_ban_request(
            prefix,
            PROXY,
            vec![proxy_impl_allowlist_rule()],
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
    raw_rules: Vec<rcs_filter::rules::RawRule>,
    kinds: Option<BTreeMap<String, KnownAllowedKind>>,
    known_allowed: &[&str],
    source: &str,
) -> usize {
    let (_, out) = capture_tracing(|| {
        run_batch(
            &[deposit_request(source, PROXY)],
            raw_rules,
            1,
            known_allowed,
            kinds,
            // One observation of budget against a proxy that delegatecalls 1 000 times: capture
            // fails, which is the shortest route to each of the two diagnostics.
            seeded_db(APPROVED_IMPL, 1_000),
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
        rcs_filter::rules::load_rules(1, 0, rules.clone()).callstack_selectors().len(),
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
        rcs_filter::rules::load_rules(1, 0, rules.clone()).callstack_selectors().len(),
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

/// The golden one-element ban, sent to `PROXY`. The proxy ignores its calldata and performs the
/// delegatecalls, so the classification rests on the deposit's own `source_hash` and `input`.
fn golden_ban_to_proxy() -> DepositTxRequest {
    let ban = golden_bans().into_iter().find(|ban| ban.n == 1).expect("an n = 1 vector");
    DepositTxRequest { data: ban.data, ..deposit_request(&ban.source_hash, PROXY) }
}

/// One mixed batch: a ban prefix, two approved originals and an ordinary deposit.
fn mixed_batch() -> (Vec<DepositTxRequest>, Vec<String>) {
    let prefix = golden_ban_to_proxy();
    let first = format!("0x{}", "66".repeat(32));
    let second = format!("0x{}", "67".repeat(32));
    let ordinary = format!("0x{}", "68".repeat(32));
    let known = vec![prefix.source_hash.clone(), first.clone(), second.clone()];
    let txs = vec![
        prefix,
        deposit_request(&first, PROXY),
        deposit_request(&second, PROXY),
        deposit_request(&ordinary, PROXY),
    ];
    (txs, known)
}

#[test]
fn resolution_metrics_count_each_inferred_entry_once() {
    in_isolated_process(
        module_path!(),
        "resolution_metrics_count_each_inferred_entry_once",
        || {
            let (txs, known) = mixed_batch();
            let known: Vec<&str> = known.iter().map(String::as_str).collect();
            let (_, counters) = counters_during(|| {
                run_batch(
                    &txs,
                    vec![proxy_impl_allowlist_rule()],
                    1,
                    &known,
                    None,
                    seeded_db(APPROVED_IMPL, 1),
                )
            });
            let resolution =
                |name| counter(&counters, &format!("rcs_filter.known_allowed_resolution.{name}"));
            assert_eq!(resolution("labelled_total"), 0, "{counters:?}");
            assert_eq!(resolution("inferred_synthetic_ban_total"), 1, "{counters:?}");
            assert_eq!(resolution("inferred_original_total"), 2, "{counters:?}");
        },
    );
}

#[test]
fn resolution_metrics_count_each_labelled_entry_once() {
    in_isolated_process(
        module_path!(),
        "resolution_metrics_count_each_labelled_entry_once",
        || {
            let (txs, known_owned) = mixed_batch();
            let known: Vec<&str> = known_owned.iter().map(String::as_str).collect();
            let kinds: BTreeMap<String, KnownAllowedKind> = [
                (known_owned[0].clone(), KnownAllowedKind::SyntheticBan),
                (known_owned[1].clone(), KnownAllowedKind::Original),
                (known_owned[2].clone(), KnownAllowedKind::Original),
            ]
            .into_iter()
            .collect();
            let (_, counters) = counters_during(|| {
                run_batch(
                    &txs,
                    vec![proxy_impl_allowlist_rule()],
                    1,
                    &known,
                    Some(kinds),
                    seeded_db(APPROVED_IMPL, 1),
                )
            });
            let resolution =
                |name| counter(&counters, &format!("rcs_filter.known_allowed_resolution.{name}"));
            assert_eq!(resolution("labelled_total"), 3, "{counters:?}");
            assert_eq!(resolution("inferred_synthetic_ban_total"), 0, "{counters:?}");
            assert_eq!(resolution("inferred_original_total"), 0, "{counters:?}");
        },
    );
}
