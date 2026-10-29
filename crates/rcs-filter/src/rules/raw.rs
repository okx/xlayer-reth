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

use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

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
    /// The JSON shape of the **first** `callstack` root value, probed while scanning. `None` when
    /// the field is absent. A repeated `callstack` is a duplicate root key, which is reported
    /// before this is consulted, so recording the first occurrence is sufficient.
    pub callstack_shape: Option<CallstackShape>,
}

/// The JSON kind of a value, determined by a **streaming** probe.
///
/// Deliberately not `serde_json::Value`: a `Value` object arm is already a map, which is exactly
/// the fidelity loss this module exists to prevent. The probe drains nested content through
/// [`IgnoredAny`] and keeps only the discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonKind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

impl JsonKind {
    /// Wording used in a rejection reason, so the reason states what arrived as well as what was
    /// required.
    pub fn describe(self) -> &'static str {
        match self {
            JsonKind::Object => "an object",
            JsonKind::Array => "an array",
            JsonKind::String => "a string",
            JsonKind::Number => "a number",
            JsonKind::Bool => "a boolean",
            JsonKind::Null => "null",
        }
    }
}

/// The probed shape of one `callstack` value: its own kind plus, when it is an object, the kind of
/// its `direct_delegatecalls` value and the first alias whose value is not a JSON string.
///
/// This exists because serde's **derived** struct decoder also accepts the positional *sequence*
/// form, so `"callstack":[{...}]` would bind the section's field from array element 0.
/// `deny_unknown_fields` cannot catch that — it only constrains map keys. Requiring the object
/// form here, on the original bytes, rejects it for both entry points at once and with a
/// deterministic reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallstackShape {
    pub kind: JsonKind,
    /// `None` when `callstack` is not an object, or is an object without `direct_delegatecalls`.
    pub direct_delegatecalls_kind: Option<JsonKind>,
    /// First `(alias, kind)` inside `direct_delegatecalls` whose value is not a string, in input
    /// order. `None` when every value is a string (or there are none).
    pub non_string_alias: Option<(String, JsonKind)>,
}

impl CallstackShape {
    /// The raw-first verdict on this shape, or `None` when the shape is admissible and the typed
    /// decode owns whatever remains.
    ///
    /// `null` is deliberately admissible here: an explicitly null `callstack` has its own
    /// dedicated semantic reason, and pre-empting it with a type reason would make a clearer
    /// diagnostic worse.
    pub fn problem(&self) -> Option<String> {
        match self.kind {
            JsonKind::Object | JsonKind::Null => {}
            other => return Some(wrong_callstack_kind_reason(other)),
        }
        if let Some(kind) = self.direct_delegatecalls_kind
            && kind != JsonKind::Object
        {
            return Some(wrong_direct_delegatecalls_kind_reason(kind));
        }
        self.non_string_alias
            .as_ref()
            .map(|(alias, kind)| non_string_alias_value_reason(alias, *kind))
    }
}

/// Verbatim reason for a `callstack` that is not a JSON object.
pub fn wrong_callstack_kind_reason(kind: JsonKind) -> String {
    format!("callstack must be a JSON object, got {}", kind.describe())
}

/// Verbatim reason for a `direct_delegatecalls` that is not a JSON object (a map).
pub fn wrong_direct_delegatecalls_kind_reason(kind: JsonKind) -> String {
    format!("direct_delegatecalls must be a JSON object, got {}", kind.describe())
}

/// Verbatim reason for an alias whose value is not a JSON string.
pub fn non_string_alias_value_reason(alias: &str, kind: JsonKind) -> String {
    format!("direct_delegatecalls alias '{alias}' must be a string, got {}", kind.describe())
}

/// Streaming kind probe for an arbitrary JSON value.
struct JsonKindProbe(JsonKind);

impl<'de> Deserialize<'de> for JsonKindProbe {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        d.deserialize_any(JsonKindVisitor).map(JsonKindProbe)
    }
}

struct JsonKindVisitor;

impl<'de> Visitor<'de> for JsonKindVisitor {
    type Value = JsonKind;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        // Drained, not collected: the bytes stay the source of truth.
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(JsonKind::Object)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(JsonKind::Array)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(JsonKind::String)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(JsonKind::Bool)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(JsonKind::Number)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(JsonKind::Number)
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<Self::Value, E> {
        Ok(JsonKind::Number)
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<Self::Value, E> {
        Ok(JsonKind::Number)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(JsonKind::Number)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(JsonKind::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(JsonKind::Null)
    }
}

/// Streaming shape probe for one `callstack` value — see [`CallstackShape`].
struct CallstackShapeProbe(CallstackShape);

impl<'de> Deserialize<'de> for CallstackShapeProbe {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        d.deserialize_any(CallstackShapeVisitor).map(CallstackShapeProbe)
    }
}

struct CallstackShapeVisitor;

impl<'de> Visitor<'de> for CallstackShapeVisitor {
    type Value = CallstackShape;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the callstack rule section as a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut direct_delegatecalls_kind = None;
        let mut non_string_alias = None;
        while let Some(key) = map.next_key::<String>()? {
            if key == "direct_delegatecalls" && direct_delegatecalls_kind.is_none() {
                // Probed one level deeper, so a wrong alias-value type is named here too. An
                // unknown key inside `callstack` is NOT this probe's business: the typed decode
                // owns that rejection, and taking it over would change which reason is reported.
                let aliases: AliasKindsProbe = map.next_value()?;
                direct_delegatecalls_kind = Some(aliases.kind);
                non_string_alias = aliases.non_string;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(CallstackShape { kind: JsonKind::Object, direct_delegatecalls_kind, non_string_alias })
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(CallstackShape::of_kind(JsonKind::Array))
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(CallstackShape::of_kind(JsonKind::String))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(CallstackShape::of_kind(JsonKind::Bool))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(CallstackShape::of_kind(JsonKind::Number))
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(CallstackShape::of_kind(JsonKind::Number))
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<Self::Value, E> {
        Ok(CallstackShape::of_kind(JsonKind::Number))
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<Self::Value, E> {
        Ok(CallstackShape::of_kind(JsonKind::Number))
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(CallstackShape::of_kind(JsonKind::Number))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(CallstackShape::of_kind(JsonKind::Null))
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(CallstackShape::of_kind(JsonKind::Null))
    }
}

impl CallstackShape {
    /// A shape with nothing probed below the top level, for every non-object kind.
    fn of_kind(kind: JsonKind) -> Self {
        CallstackShape { kind, direct_delegatecalls_kind: None, non_string_alias: None }
    }
}

/// The probed shape of a `direct_delegatecalls` value: its kind, and the first alias whose value
/// is not a string.
struct AliasKindsProbe {
    kind: JsonKind,
    non_string: Option<(String, JsonKind)>,
}

impl<'de> Deserialize<'de> for AliasKindsProbe {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        d.deserialize_any(AliasKindsVisitor)
    }
}

struct AliasKindsVisitor;

impl<'de> Visitor<'de> for AliasKindsVisitor {
    type Value = AliasKindsProbe;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("direct_delegatecalls as a JSON object of alias name to address string")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut non_string: Option<(String, JsonKind)> = None;
        // Every entry is visited in input order and nothing is folded, so a duplicate alias is
        // still intact for the typed decode that owns rejecting it.
        while let Some(alias) = map.next_key::<String>()? {
            let value: JsonKindProbe = map.next_value()?;
            if value.0 != JsonKind::String && non_string.is_none() {
                non_string = Some((alias, value.0));
            }
        }
        Ok(AliasKindsProbe { kind: JsonKind::Object, non_string })
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(AliasKindsProbe { kind: JsonKind::Array, non_string: None })
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(AliasKindsProbe { kind: JsonKind::String, non_string: None })
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(AliasKindsProbe { kind: JsonKind::Bool, non_string: None })
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(AliasKindsProbe { kind: JsonKind::Number, non_string: None })
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(AliasKindsProbe { kind: JsonKind::Number, non_string: None })
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<Self::Value, E> {
        Ok(AliasKindsProbe { kind: JsonKind::Number, non_string: None })
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<Self::Value, E> {
        Ok(AliasKindsProbe { kind: JsonKind::Number, non_string: None })
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(AliasKindsProbe { kind: JsonKind::Number, non_string: None })
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(AliasKindsProbe { kind: JsonKind::Null, non_string: None })
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(AliasKindsProbe { kind: JsonKind::Null, non_string: None })
    }
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
        let mut callstack_shape: Option<CallstackShape> = None;

        while let Some(key) = map.next_key::<String>()? {
            if key == "id" {
                // Lift `id` for reporting only. A non-string `id` is left as `None` here; the
                // typed decode owns rejecting it.
                let value: serde_json::Value = map.next_value()?;
                if id_occurrences == 0 {
                    id = value.as_str().map(str::to_owned);
                }
                id_occurrences += 1;
            } else if key == "callstack" && callstack_shape.is_none() {
                let probe: CallstackShapeProbe = map.next_value()?;
                callstack_shape = Some(probe.0);
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

        Ok(RuleRootScan { keys, id, callstack_shape })
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
    fn root_scan_probes_the_callstack_json_kind_without_building_a_map() {
        let object =
            scan_rule_root(r#"{"id":"r","callstack":{"direct_delegatecalls":{"p":"0x11"}}}"#)
                .unwrap();
        let shape = object.callstack_shape.expect("probed");
        assert_eq!(shape.kind, JsonKind::Object);
        assert_eq!(shape.direct_delegatecalls_kind, Some(JsonKind::Object));
        assert_eq!(shape.non_string_alias, None);
        assert_eq!(shape.problem(), None, "a well-formed section has no raw-first problem");

        for (value, kind) in [
            (r#"[{"p":"0x11"}]"#, JsonKind::Array),
            ("[]", JsonKind::Array),
            ("7", JsonKind::Number),
            ("-1", JsonKind::Number),
            ("1.5", JsonKind::Number),
            (r#""text""#, JsonKind::String),
            ("true", JsonKind::Bool),
            ("null", JsonKind::Null),
        ] {
            let scan = scan_rule_root(&format!(r#"{{"id":"r","callstack":{value}}}"#)).unwrap();
            assert_eq!(scan.callstack_shape.expect("probed").kind, kind, "for {value}");
        }

        // Absent is distinct from any kind.
        assert_eq!(scan_rule_root(r#"{"id":"r"}"#).unwrap().callstack_shape, None);
    }

    #[test]
    fn callstack_shape_rejects_every_non_object_kind_except_null() {
        for (value, needle) in [
            (r#"[{"p":"0x11"}]"#, "callstack must be a JSON object, got an array"),
            ("7", "callstack must be a JSON object, got a number"),
            (r#""text""#, "callstack must be a JSON object, got a string"),
            ("true", "callstack must be a JSON object, got a boolean"),
        ] {
            let scan = scan_rule_root(&format!(r#"{{"id":"r","callstack":{value}}}"#)).unwrap();
            let problem = scan.callstack_shape.unwrap().problem().expect("must be a problem");
            assert_eq!(problem, needle);
        }

        // `null` is deliberately admissible here: it keeps its own dedicated semantic reason.
        let nulled = scan_rule_root(r#"{"id":"r","callstack":null}"#).unwrap();
        assert_eq!(nulled.callstack_shape.unwrap().problem(), None);
    }

    #[test]
    fn callstack_shape_names_a_non_map_section_and_a_non_string_alias() {
        let section = scan_rule_root(r#"{"id":"r","callstack":{"direct_delegatecalls":[]}}"#)
            .unwrap()
            .callstack_shape
            .unwrap();
        assert_eq!(
            section.problem().unwrap(),
            "direct_delegatecalls must be a JSON object, got an array"
        );

        let alias = scan_rule_root(r#"{"id":"r","callstack":{"direct_delegatecalls":{"p":7}}}"#)
            .unwrap()
            .callstack_shape
            .unwrap();
        assert_eq!(
            alias.problem().unwrap(),
            "direct_delegatecalls alias 'p' must be a string, got a number"
        );

        // The FIRST offending alias in input order is the one reported.
        let first = scan_rule_root(
            r#"{"id":"r","callstack":{"direct_delegatecalls":{"a":"0x11","b":null,"c":7}}}"#,
        )
        .unwrap()
        .callstack_shape
        .unwrap();
        assert_eq!(first.non_string_alias, Some(("b".to_string(), JsonKind::Null)));
    }

    #[test]
    fn the_callstack_probe_does_not_fold_a_duplicate_alias_away() {
        // The probe streams and discards; the bytes stay the source of truth, so the typed decode
        // still sees both entries. This is the property that lets the probe run first safely.
        let src = r#"{"id":"r","callstack":{"direct_delegatecalls":{"p":"0x11","p":"0x22"}}}"#;
        let scan = scan_rule_root(src).unwrap();
        let shape = scan.callstack_shape.expect("probed");
        assert_eq!(shape.problem(), None, "a duplicate alias is not a shape problem");
        assert_eq!(src.matches(r#""p""#).count(), 2, "both entries still present in the bytes");
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
