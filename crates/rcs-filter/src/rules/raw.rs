//! Raw-input fidelity helpers shared by both rule entry points.
//!
//! A rule's original JSON bytes must survive untouched until duplicate detection has run. A
//! `serde_json::Value` is untyped but **not raw**: its object arm is already a map, so a repeated
//! key is folded last-wins before any validation can see it. Everything in this module therefore
//! works on the original `&str` slice and never materialises a map for a rule.
//!
//! `#[serde(flatten)]` cannot be used on an envelope that keeps its rules raw: flatten buffers
//! every field through serde's internal content representation, which a raw value cannot survive.
//! The envelope visitors are hand-written for that reason, which also lets them report duplicate
//! and unknown top-level keys by name.

use std::fmt;

use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::Deserializer;

/// Result of the streaming root-key scan over one rule's original bytes.
///
/// `keys` holds every root key **in input order with duplicates preserved**, because a folded map
/// would be exactly the evidence loss this scan exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRootScan {
    pub keys: Vec<String>,
    /// The rule's `id`, lifted before any typed decode so a rule can still be named when the
    /// typed decode would fail. `None` when `id` is absent, repeated, or not a string — in which
    /// case the rule is reported by its request index instead, never by a fabricated id.
    pub id: Option<String>,
}

impl RuleRootScan {
    /// Root keys that appear more than once, each reported once, in first-appearance order.
    pub fn duplicate_keys(&self) -> Vec<&str> {
        let mut seen = std::collections::BTreeSet::new();
        let mut reported = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for key in &self.keys {
            if !seen.insert(key.as_str()) && reported.insert(key.as_str()) {
                out.push(key.as_str());
            }
        }
        out
    }
}

struct RuleRootScanVisitor;

impl<'de> Visitor<'de> for RuleRootScanVisitor {
    type Value = RuleRootScan;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object describing one rule")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys: Vec<String> = Vec::new();
        let mut id: Option<String> = None;
        let mut id_occurrences = 0usize;

        while let Some(key) = map.next_key::<String>()? {
            if key == "id" {
                // Lift `id` for reporting only. A non-string `id` is left as `None` here; the
                // typed decode owns rejecting it.
                let value: serde_json::Value = map.next_value()?;
                if id_occurrences == 0 {
                    id = value.as_str().map(str::to_owned);
                }
                id_occurrences += 1;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
            keys.push(key);
        }

        // A repeated `id` cannot identify the rule: the duplicate is itself a defect, and picking
        // either occurrence would be a guess.
        if id_occurrences > 1 {
            id = None;
        }

        Ok(RuleRootScan { keys, id })
    }
}

/// Streaming root-key scan over one rule's **original bytes**. Builds no map, so duplicate keys
/// are preserved for the duplicate-root-key check.
///
/// A rule element that is not a JSON object is rejected here, explicitly: a raw value preserves
/// any valid JSON value and does not by itself guarantee an object.
pub fn scan_rule_root(src: &str) -> Result<RuleRootScan, String> {
    let mut de = serde_json::Deserializer::from_str(src);
    let scan = (&mut de)
        .deserialize_any(RuleRootScanVisitor)
        .map_err(|e| format!("rule is not a readable JSON object: {e}"))?;
    de.end().map_err(|e| format!("rule has trailing content after the JSON object: {e}"))?;
    Ok(scan)
}

/// Verbatim `-32602` detail naming every unknown top-level key, so both entry points report an
/// envelope-shaped problem identically.
pub fn unknown_envelope_keys_reason(keys: &[String]) -> String {
    format!("request declares unknown top-level field(s): {}", keys.join(", "))
}

/// Verbatim `-32602` detail for an unsupported `protocol_version`, naming the received value and
/// the supported set.
pub fn unsupported_protocol_reason(received: u32) -> String {
    format!(
        "unsupported protocol_version {received}; supported: {:?}",
        crate::config::SUPPORTED_PROTOCOL_VERSIONS
    )
}

/// Verbatim reason for a duplicate top-level envelope key.
pub fn duplicate_envelope_key_reason(key: &str) -> String {
    format!("request declares top-level key '{key}' more than once")
}

/// Tracks envelope key occurrences for a hand-written envelope visitor: which keys were unknown,
/// and which appeared more than once. Shared so both entry points agree on the Phase A verdict.
#[derive(Debug, Default)]
pub struct EnvelopeKeyTracker {
    seen: Vec<String>,
    unknown: Vec<String>,
    duplicates: Vec<String>,
}

impl EnvelopeKeyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one key. `known` is this envelope's modelled field list.
    pub fn record(&mut self, key: &str, known: &[&str]) {
        if self.seen.iter().any(|s| s == key) && !self.duplicates.iter().any(|d| d == key) {
            self.duplicates.push(key.to_owned());
        }
        if !self.seen.iter().any(|s| s == key) {
            self.seen.push(key.to_owned());
        }
        if !known.contains(&key) && !self.unknown.iter().any(|u| u == key) {
            self.unknown.push(key.to_owned());
        }
    }

    /// Phase A verdict, in the normative order: unknown top-level keys first, then duplicates.
    /// Returns `None` when the envelope's key set is acceptable.
    pub fn problem(&self) -> Option<String> {
        if !self.unknown.is_empty() {
            return Some(unknown_envelope_keys_reason(&self.unknown));
        }
        self.duplicates.first().map(|key| duplicate_envelope_key_reason(key))
    }

    /// Whether `key` has already been consumed, so a visitor can skip a repeated field's value
    /// without overwriting the first occurrence.
    pub fn already_seen(&self, key: &str) -> bool {
        self.seen.iter().any(|s| s == key)
    }
}

/// Reports a rule by its `id` when one could be lifted, and otherwise by its explicit 0-based
/// request index. An id is never fabricated and never inferred.
pub fn rule_locator(id: Option<&str>, index: usize) -> String {
    match id {
        Some(id) => id.to_owned(),
        None => format!("<no id; request index {index}>"),
    }
}

/// Missing-field reason used by the hand-written envelope visitors.
pub fn missing_envelope_field<E: de::Error>(field: &'static str) -> E {
    de::Error::missing_field(field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_scan_preserves_duplicate_keys_in_order() {
        let scan = scan_rule_root(r#"{"id":"r","event_abis":{},"event_abis":{}}"#).unwrap();
        assert_eq!(scan.keys, vec!["id", "event_abis", "event_abis"]);
        assert_eq!(scan.duplicate_keys(), vec!["event_abis"]);
        assert_eq!(scan.id.as_deref(), Some("r"));
    }

    #[test]
    fn root_scan_reports_each_duplicate_key_once() {
        let scan = scan_rule_root(r#"{"a":1,"a":2,"a":3,"b":1,"b":2}"#).unwrap();
        assert_eq!(scan.duplicate_keys(), vec!["a", "b"]);
    }

    #[test]
    fn root_scan_sees_a_duplicate_alias_inside_callstack_as_one_root_key() {
        // The duplicate is one level down, so it is NOT a duplicate ROOT key: the typed decode
        // owns that rejection. This pins that the two checks stay distinct.
        let src = r#"{"id":"r","callstack":{"direct_delegatecalls":{"p":"0x1","p":"0x2"}}}"#;
        let scan = scan_rule_root(src).unwrap();
        assert_eq!(scan.keys, vec!["id", "callstack"]);
        assert!(scan.duplicate_keys().is_empty());
    }

    #[test]
    fn root_scan_lifts_id_before_any_typed_decode() {
        // The rule is typed-decode-invalid (`action` is a number), yet the id is still available.
        let scan = scan_rule_root(r#"{"id":"named","action":7}"#).unwrap();
        assert_eq!(scan.id.as_deref(), Some("named"));
    }

    #[test]
    fn root_scan_refuses_to_name_a_missing_repeated_or_non_string_id() {
        assert_eq!(scan_rule_root(r#"{"action":"deny"}"#).unwrap().id, None);
        assert_eq!(scan_rule_root(r#"{"id":"a","id":"b"}"#).unwrap().id, None);
        assert_eq!(scan_rule_root(r#"{"id":7}"#).unwrap().id, None);
        assert_eq!(scan_rule_root(r#"{"id":null}"#).unwrap().id, None);
    }

    #[test]
    fn root_scan_rejects_a_rule_element_that_is_not_an_object() {
        for src in ["[]", "7", "\"text\"", "null", "true"] {
            let err = scan_rule_root(src).expect_err("non-object rule must be rejected");
            assert!(err.contains("not a readable JSON object"), "unexpected reason: {err}");
        }
    }

    #[test]
    fn root_scan_rejects_trailing_content() {
        assert!(scan_rule_root(r#"{"id":"r"} {"id":"s"}"#).is_err());
    }

    #[test]
    fn rule_locator_never_fabricates_an_id() {
        assert_eq!(rule_locator(Some("real-id"), 3), "real-id");
        let anonymous = rule_locator(None, 2);
        assert!(anonymous.contains("index 2"));
        assert!(anonymous.contains("no id"));
    }

    #[test]
    fn envelope_tracker_reports_unknown_before_duplicate() {
        let known = ["protocol_version", "rules"];
        let mut tracker = EnvelopeKeyTracker::new();
        tracker.record("rules", &known);
        tracker.record("rules", &known);
        tracker.record("surprise", &known);
        let problem = tracker.problem().expect("both problems present");
        assert!(problem.contains("surprise"), "unknown key takes precedence: {problem}");
    }

    #[test]
    fn envelope_tracker_accepts_a_clean_key_set_and_flags_duplicates_alone() {
        let known = ["protocol_version", "rules"];
        let mut clean = EnvelopeKeyTracker::new();
        clean.record("protocol_version", &known);
        clean.record("rules", &known);
        assert!(clean.problem().is_none());

        let mut duplicated = EnvelopeKeyTracker::new();
        duplicated.record("rules", &known);
        duplicated.record("rules", &known);
        let problem = duplicated.problem().expect("duplicate must be reported");
        assert!(problem.contains("more than once"), "unexpected reason: {problem}");
    }

    #[test]
    fn phase_a_reasons_name_the_value_and_the_supported_set() {
        let reason = unsupported_protocol_reason(2);
        assert!(reason.contains('2'));
        assert!(reason.contains('1'), "must name the supported set: {reason}");
    }
}
