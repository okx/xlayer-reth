//! The request-level non-verdict gate, and the synthetic-prefix reporting the consumer cannot see
//! any other way.

use super::*;
use alloy_primitives::{Address, B256};
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use rcs_filter::callstack::SnapshotStamp;
use rcs_filter::rules::load_rules;

/// Runs `f` with every `tracing` event captured, so a diagnostic's fields can be asserted —
/// including the fields it must NOT carry.
pub(super) fn capture_tracing<R>(f: impl FnOnce() -> R) -> (R, String) {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl tracing_subscriber::fmt::MakeWriter<'_> for BufWriter {
        type Writer = BufWriter;
        fn make_writer(&self) -> BufWriter {
            self.clone()
        }
    }

    let buf = BufWriter(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_target(true)
        .with_writer(buf.clone())
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (out, logged)
}

/// An alias-bearing rule set, compiled exactly as the handler compiles a request's own.
fn selector_bearing_rules() -> RuleSet {
    let rule: RawRule = serde_json::from_value(serde_json::json!({
        "id": "proxy-impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": {
            "entry": format!("{:#x}", Address::repeat_byte(0xAA))
        } },
        "condition": { "!=": [ { "var": "entry.delegateTo" }, null ] },
        "action": "deny"
    }))
    .expect("the fixture rule deserializes");
    let rules = load_rules(1, 0, vec![rule]);
    assert!(rules.rejected.is_empty(), "the fixture rule must load clean");
    rules
}

/// An event rule declaring no call-frame alias.
fn selector_free_rules() -> RuleSet {
    let rule: RawRule = serde_json::from_str(
        r#"{"id":"evt","event_abis":{"transfer":{"type":"event","name":"Transfer",
               "inputs":[{"name":"from","type":"address","indexed":true}],
               "anonymous":false}},"condition":true,"action":"deny"}"#,
    )
    .expect("valid event-only rule");
    load_rules(1, 0, vec![rule])
}

/// the gate refuses an observation set produced under a different rule snapshot, accepts a
/// genuinely empty set stamped by the evaluating one, and leaves a request that declares no
/// selector completely untouched.
#[test]
fn the_request_level_gate_separates_unavailable_from_empty() {
    let rules = selector_bearing_rules();
    let foreign = SnapshotStamp { selector_digest: B256::repeat_byte(0xEE), ..rules.stamp() };

    assert_eq!(
        unadjudicable_observations(&rules, Observations::available(foreign, &[])),
        Some(ObservationUnavailable::SnapshotMismatch),
        "a foreign stamp cannot be read as captured-and-empty"
    );
    assert_eq!(
        unadjudicable_observations(&rules, Observations::available(rules.stamp(), &[])),
        None,
        "capture ran and genuinely observed nothing: adjudicable"
    );
    assert_eq!(
        unadjudicable_observations(&rules, Observations::NOT_CAPTURED),
        Some(ObservationUnavailable::NotCaptured)
    );
    assert_eq!(
        unadjudicable_observations(
            &rules,
            Observations::Unavailable { reason: ObservationUnavailable::BudgetExceeded }
        ),
        Some(ObservationUnavailable::BudgetExceeded),
        "the reason is preserved, not collapsed"
    );
    assert_eq!(
        unadjudicable_observations(&selector_free_rules(), Observations::NOT_CAPTURED),
        None,
        "a request declaring no selector keeps its pre-existing path byte for byte"
    );
}

/// every classification increments its OWN counter, read back from a recorder rather than
/// restated from the code.
#[test]
fn each_classification_increments_its_own_counter() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _guard = metrics::set_default_local_recorder(&recorder);

    // `new_with_labels` registers fresh handles against the recorder installed above, where
    // `default()` would hand back a cached instance bound to whichever recorder was active
    // when some other test first built it.
    let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
    for identification in [
        CallstackIdentification::Clean,
        CallstackIdentification::Objected,
        CallstackIdentification::NotIdentified,
        CallstackIdentification::NotApplicable,
    ] {
        report_synthetic_ban_identification(
            &metrics,
            B256::repeat_byte(0x7A),
            "0xsrc",
            identification,
            3,
        );
    }

    let mut counters: Vec<(String, u64)> = snapshotter
        .snapshot()
        .into_vec()
        .into_iter()
        .filter_map(|(key, _, _, value)| match value {
            DebugValue::Counter(count) => Some((key.key().name().to_string(), count)),
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
        "each class lands in its own dedicated synthetic_ban series (the dotted key is the \
             recorder's internal form; the exporter renders it with underscores)"
    );
}

/// an objection on a replay prefix emits exactly the bounded fields — and none of the
/// content a rule id or an observation would carry.
#[test]
fn an_objected_prefix_emits_one_bounded_anomaly_log() {
    let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
    let (_, out) = capture_tracing(|| {
        report_synthetic_ban_identification(
            &metrics,
            B256::repeat_byte(0x7A),
            "0xsrc",
            CallstackIdentification::Objected,
            3,
        );
    });

    assert_eq!(out.lines().count(), 1, "exactly one log line: {out}");
    assert!(out.contains("ERROR"), "expected error level: {out}");
    assert!(
        out.contains("xlayer_audit_rpc::synthetic_ban_identification"),
        "expected the dedicated target: {out}"
    );
    assert!(out.contains("outcome=\"objected\""), "expected the outcome token: {out}");
    assert!(out.contains("selector_bearing_rules=3"), "expected the bounded count: {out}");
    assert!(out.contains("0xsrc"), "expected the source hash: {out}");
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
    let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
    let (_, out) = capture_tracing(|| {
        report_synthetic_ban_identification(
            &metrics,
            B256::repeat_byte(0x7A),
            "0xsrc",
            CallstackIdentification::NotIdentified,
            1,
        );
    });

    assert_eq!(out.lines().count(), 1, "exactly one log line: {out}");
    assert!(out.contains("ERROR"), "expected error level: {out}");
    assert!(out.contains("outcome=\"not_identified\""), "expected the outcome token: {out}");
    assert!(out.contains("selector_bearing_rules=1"), "expected the bounded count: {out}");
}

/// a clean (and a not-applicable) prefix is counted and stays SILENT: the log is for
/// anomalies, so an ordinary replay must not fill an operator's log with one line per prefix.
#[test]
fn a_clean_prefix_is_counted_without_an_anomaly_log() {
    let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
    let (_, out) = capture_tracing(|| {
        report_synthetic_ban_identification(
            &metrics,
            B256::repeat_byte(0x7A),
            "0xsrc",
            CallstackIdentification::Clean,
            3,
        );
        report_synthetic_ban_identification(
            &metrics,
            B256::repeat_byte(0x7B),
            "0xsrc2",
            CallstackIdentification::NotApplicable,
            0,
        );
    });
    assert!(out.is_empty(), "no anomaly, so no log: {out}");
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
