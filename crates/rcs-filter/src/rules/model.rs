//! Rule data model: raw (wire) shapes deserialized from RCS, and compiled shapes used by
//! the matching hot path.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use alloy_dyn_abi::DynSolType;
use alloy_primitives::{Address, B256};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// Rule action. Priority when merging: `deny > audit > allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Filter-only immediate drop via `mark_invalid`; never enters the buffer pool, never
    /// contacts RCS.
    Deny,
    /// Log explicitly needs no adjudication; cannot override a `deny`/`audit` on the same log.
    Allow,
    /// Submit the transaction to RCS for adjudication.
    Audit,
}

/// Fallback applied when RCS is unresponsive past `total_retry_timeout`.
/// `allow` = fail-open, `deny` = fail-close. Omitted `audit_timeout_action` defaults to
/// `allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeoutAction {
    Deny,
    Allow,
}

impl TimeoutAction {
    /// Returns the stricter of two timeout actions (`deny` wins), used when a tx matches
    /// several audit rules with differing `audit_timeout_action`.
    pub fn stricter(self, other: TimeoutAction) -> TimeoutAction {
        match (self, other) {
            (TimeoutAction::Deny, _) | (_, TimeoutAction::Deny) => TimeoutAction::Deny,
            _ => TimeoutAction::Allow,
        }
    }
}

/// A single ABI input declaration inside an [`EventAbi`]. `name` is `Option` so the
/// loader can detect and reject a missing name.
#[derive(Debug, Clone, Deserialize)]
pub struct AbiInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub param_type: String,
    pub indexed: bool,
}

/// A named event declaration. The map key in [`RawRule::event_abis`] is
/// the rule-local name; `name` here is the on-chain Solidity event name used for topic0.
#[derive(Debug, Clone, Deserialize)]
pub struct EventAbi {
    #[serde(rename = "type")]
    pub abi_type: String,
    pub name: String,
    pub inputs: Vec<AbiInput>,
    pub anonymous: bool,
}

fn default_audit_types() -> Vec<String> {
    vec!["quota".to_string()]
}

/// Rule-level keys this build knowingly tolerates, each paired with the recorded reason that
/// tolerating it cannot weaken a rule's constraint.
///
/// **Empty, deliberately.** Every wire key of the producer's rule — including `callstack` — is
/// modelled by [`RawRuleWire`], and no additional rule-root field has been identified that could
/// be safely ignored. A producer that tolerates case variations of a key *on read* does not mean
/// it emits those keys to this node.
///
/// Consequence: **every** unmodelled rule-root key rejects the rule. Adding an exception requires
/// per-key evidence and a fresh design review; this list is not left open "in case there is a
/// production extension".
pub const RULE_LEGACY_TOLERATED_KEYS: &[(&str, &str)] = &[];

/// Every modelled rule-root key. Used by the root-level gate, which rejects anything outside this
/// set and [`RULE_LEGACY_TOLERATED_KEYS`].
pub const MODELLED_RULE_ROOT_KEYS: &[&str] = &[
    "id",
    "contract_address",
    "origin",
    "event_abis",
    "audit_types",
    "condition",
    "action",
    "audit_timeout_action",
    "callstack",
];

/// Whether `key` is a rule-root key this build models or knowingly tolerates.
pub fn is_accepted_rule_root_key(key: &str) -> bool {
    MODELLED_RULE_ROOT_KEYS.contains(&key)
        || RULE_LEGACY_TOLERATED_KEYS.iter().any(|(tolerated, _)| *tolerated == key)
}

/// The `callstack` rule section. Under `protocol_version` 1 the **only** admissible field is
/// `direct_delegatecalls`; `deny_unknown_fields` is what makes a typo such as
/// `direct_delegatecall` or `directDelegatecalls` a rejection rather than a silent ignore.
///
/// It is applied here and not to [`RawRuleWire`]: `callstack` is new and has no deployed shapes to
/// protect, whereas the rule root is gated explicitly instead (see [`RULE_LEGACY_TOLERATED_KEYS`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCallstack {
    /// Deserialized through a duplicate-rejecting visitor, **not** straight into a map: a
    /// `BTreeMap` folds a repeated key last-wins, which would destroy the evidence the duplicate
    /// check needs.
    #[serde(deserialize_with = "alias_map_reject_duplicates")]
    pub direct_delegatecalls: BTreeMap<String, String>,
}

/// Rejects a duplicate alias key **before** the map folds, so a repeated alias can never be
/// silently collapsed to its last value. Identical values are still a duplicate key — a repeated
/// alias is not "harmlessly redundant", because accepting it would mean the rule the producer
/// wrote is not the rule this node enforces.
fn alias_map_reject_duplicates<'de, D>(d: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct AliasMapVisitor;

    impl<'de> Visitor<'de> for AliasMapVisitor {
        type Value = BTreeMap<String, String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            // Names the field, so a wrong-JSON-type rejection is diagnosable without the
            // reader having to know which field serde was decoding.
            f.write_str(
                "direct_delegatecalls as a map of alias name to lowercase 0x 20-byte address",
            )
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut out = BTreeMap::new();
            // Entry-at-a-time, so each key is inspected against what is already present rather
            // than being inserted over it.
            while let Some((alias, address)) = map.next_entry::<String, String>()? {
                if out.contains_key(&alias) {
                    return Err(de::Error::custom(duplicate_alias_reason(&alias)));
                }
                out.insert(alias, address);
            }
            Ok(out)
        }
    }

    d.deserialize_map(AliasMapVisitor)
}

/// Verbatim duplicate-alias rejection reason.
pub fn duplicate_alias_reason(alias: &str) -> String {
    format!("direct_delegatecalls declares alias '{alias}' more than once")
}

/// Preserves present-vs-explicit-null for `callstack`: with `#[serde(default)]`, `None` means the
/// field was absent, `Some(None)` means it was explicitly `null`, and `Some(Some(_))` means a real
/// object. Without this, `"callstack": null` would be indistinguishable from an omitted
/// `callstack`.
///
/// The error is re-emitted with the field name, because serde's own message for a wrong JSON type
/// here (for example a struct decoded from a sequence) does not mention which field it was
/// decoding, and a wrong-type rejection has to name the field to be diagnosable.
fn callstack_double_option<'de, D>(d: D) -> Result<Option<Option<RawCallstack>>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<RawCallstack>::deserialize(d) {
        Ok(value) => Ok(Some(value)),
        Err(e) => Err(de::Error::custom(format!("callstack: {e}"))),
    }
}

/// The wire shape of a rule, including the flattened catch-all that makes an unmodelled rule-root
/// key visible. Converted into [`RawRule`] by a `TryFrom` that applies the root-level gate, so
/// `unknown` never survives into the compiled pipeline.
#[derive(Debug, Clone, Deserialize)]
pub struct RawRuleWire {
    pub id: String,
    #[serde(default)]
    pub contract_address: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    /// Defaulted so a direct-delegatecall-only rule may omit it. A rule declaring **neither**
    /// section is still rejected, with its original reason.
    #[serde(default)]
    pub event_abis: BTreeMap<String, EventAbi>,
    #[serde(default = "default_audit_types")]
    pub audit_types: Vec<String>,
    pub condition: serde_json::Value,
    pub action: Action,
    #[serde(default)]
    pub audit_timeout_action: Option<TimeoutAction>,
    #[serde(default, deserialize_with = "callstack_double_option")]
    pub callstack: Option<Option<RawCallstack>>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// Raw rule as delivered by RCS `GET /rules`. `condition` is kept as a raw
/// JSON value (may be the literal `true` or a JSONLogic object).
///
/// Produced only through [`RawRuleWire`], whose conversion applies the root-level gate: an
/// unmodelled rule-root key rejects the rule instead of being silently dropped. Before this
/// gate existed, a rule carrying a valid `event_abis` **and** a misspelled `callStack` loaded as a
/// weakened event-only rule.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawRuleWire")]
pub struct RawRule {
    pub id: String,
    pub contract_address: Option<String>,
    pub origin: Option<String>,
    pub event_abis: BTreeMap<String, EventAbi>,
    pub audit_types: Vec<String>,
    pub condition: serde_json::Value,
    pub action: Action,
    pub audit_timeout_action: Option<TimeoutAction>,
    /// `None` = absent, `Some(None)` = explicitly `null` (rejected at compile), `Some(Some(_))` = a
    /// real object.
    pub callstack: Option<Option<RawCallstack>>,
}

impl TryFrom<RawRuleWire> for RawRule {
    type Error = String;

    fn try_from(wire: RawRuleWire) -> Result<Self, Self::Error> {
        // The root-level gate. Naming every offending key makes a misspelled safety field
        // fail loudly instead of degrading the rule.
        let offending: Vec<&str> = wire
            .unknown
            .keys()
            .map(String::as_str)
            .filter(|key| !is_accepted_rule_root_key(key))
            .collect();
        if !offending.is_empty() {
            return Err(unknown_rule_keys_reason(&offending));
        }

        Ok(RawRule {
            id: wire.id,
            contract_address: wire.contract_address,
            origin: wire.origin,
            event_abis: wire.event_abis,
            audit_types: wire.audit_types,
            condition: wire.condition,
            action: wire.action,
            audit_timeout_action: wire.audit_timeout_action,
            callstack: wire.callstack,
        })
    }
}

/// Verbatim unknown-rule-key rejection reason.
pub fn unknown_rule_keys_reason(keys: &[&str]) -> String {
    format!("rule declares unknown field(s): {}", keys.join(", "))
}

/// Verbatim duplicate-root-key rejection reason — a duplicate rule-root key is the same
/// fidelity defect one
/// level up: it is folded last-wins and could hide a modelled field.
pub fn duplicate_rule_root_key_reason(key: &str) -> String {
    format!("rule declares root key '{key}' more than once")
}

impl RawRule {
    /// The rule's `direct_delegatecalls` map, or an empty map when `callstack` is absent or null.
    /// Callers that must distinguish those cases read [`RawRule::callstack`] directly.
    pub fn direct_delegatecalls(&self) -> Option<&BTreeMap<String, String>> {
        self.callstack.as_ref()?.as_ref().map(|cs| &cs.direct_delegatecalls)
    }
}

/// A compiled ABI input: the resolved [`DynSolType`] plus indexed flag and name.
#[derive(Debug, Clone)]
pub struct CompiledInput {
    pub name: String,
    pub sol_type: DynSolType,
    pub indexed: bool,
}

/// A compiled named event: precomputed `topic0` + resolved input types, ready for the
/// matching hot path.
#[derive(Debug, Clone)]
pub struct CompiledEvent {
    /// Rule-local name (the `event_abis` map key), used as the `<name>` prefix for
    /// condition variables and as `ActionItem.name` on submission.
    pub var_name: String,
    /// On-chain Solidity event name.
    pub abi_name: String,
    pub inputs: Vec<CompiledInput>,
    pub anonymous: bool,
    /// keccak256 of the canonical event signature.
    pub topic0: B256,
}

/// A condition compiled once at rule load and evaluated on the matching hot path in place of
/// walking the raw JSONLogic value. The tree is a hybrid: it specializes only the boolean
/// combinators needed to reach an accelerable membership node plus the fast membership node
/// itself; every other subtree is stored verbatim as [`CompiledCondition::Raw`] and evaluated by
/// the existing raw-value evaluator, so non-specialized operators keep byte-for-byte semantics.
#[derive(Debug, Clone)]
pub enum CompiledCondition {
    /// Static all-address `in` membership resolved to an O(1) set lookup. `orig` is the original
    /// `{"in":[needle, [..]]}` value, retained for an exact linear fallback when the needle is not
    /// a parseable address.
    AddressSetIn {
        needle: serde_json::Value,
        set: Arc<HashSet<Address>>,
        orig: serde_json::Value,
    },
    And(Vec<CompiledCondition>),
    Or(Vec<CompiledCondition>),
    Not(Box<CompiledCondition>),
    /// Any other node — evaluated by the existing raw-value evaluator, unchanged.
    Raw(serde_json::Value),
}

/// One compiled `direct_delegatecalls` entry: a rule-local alias bound to the selected address
/// whose own direct `DELEGATECALL`s the rule observes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledDirectDelegatecall {
    /// Rule-local alias; the `<alias>` prefix of the four condition variables.
    pub alias: String,
    pub address: Address,
}

/// How a rule binds execution facts, decided once at compile time. The three classes exist
/// because they cannot share one candidate rule without either breaking deployed semantics or
/// creating a production-wide false deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleBindingClass {
    /// `event_abis` only. **Unchanged behaviour:** a missing event contributes one null candidate
    /// and the rule is still evaluated, which is what preserves Emergency Deny-All.
    EventOnly,
    /// `direct_delegatecalls` only. Every declared alias must have at least one observation, or
    /// the rule is not evaluated at all.
    DirectOnly,
    /// Both sections. Every declared alias must have an observation **and** every declared event
    /// must have a physical binding, or the rule is not evaluated.
    Mixed,
}

/// A rule that passed load validation and is ready for matching.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub id: String,
    pub contract_address: Option<Address>,
    pub origin: Option<Address>,
    pub events: Vec<CompiledEvent>,
    pub audit_types: Vec<String>,
    pub condition: serde_json::Value,
    pub action: Action,
    /// Always resolved (defaults to `allow` when omitted for an `audit` rule).
    pub audit_timeout_action: TimeoutAction,
    /// Compiled form of `condition`, evaluated on the matching hot path.
    pub compiled_condition: CompiledCondition,
    /// Declared direct-delegatecall aliases, in rule-declared (alias-sorted) order. Empty for an
    /// event-only rule, which is what keeps that rule's cost and semantics unchanged.
    pub direct_delegatecalls: Vec<CompiledDirectDelegatecall>,
}

impl CompiledRule {
    /// This rule's binding class, derived from which sections it declares.
    pub fn binding_class(&self) -> RuleBindingClass {
        match (self.events.is_empty(), self.direct_delegatecalls.is_empty()) {
            (false, true) => RuleBindingClass::EventOnly,
            (true, false) => RuleBindingClass::DirectOnly,
            // A rule declaring neither section is rejected at load, so it never reaches
            // matching; classifying it as Mixed makes any future leak fail closed (suppressed)
            // rather than evaluate an unconstrained condition.
            _ => RuleBindingClass::Mixed,
        }
    }

    /// Whether this rule declares at least one direct-delegatecall alias.
    pub fn declares_alias(&self) -> bool {
        !self.direct_delegatecalls.is_empty()
    }
}

/// One rule dropped during [`super::load_rules`] (per-rule id + why it failed), surfaced
/// so callers can decide whether a partially-invalid batch is still safe to install rather than
/// silently running with fewer active rules than the source actually declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedRule {
    pub id: String,
    pub reason: String,
}

/// An immutable, validated rule set snapshot plus its topic0 index. Swapped atomically on
/// hot-reload; the hot path only ever reads a fully-built snapshot.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    pub protocol_version: u32,
    pub content_version: u64,
    pub rules: Vec<CompiledRule>,
    /// `topic0 → indices into `rules`` of rules declaring an event with that topic0.
    pub index: HashMap<B256, Vec<usize>>,
    /// Number of indexed topics -> rule indices declaring an anonymous event with that shape.
    pub anonymous_index: HashMap<usize, Vec<usize>>,
    /// Rules from the source batch that failed validation and were left out of `rules` above.
    /// Non-empty means this snapshot is a *strict subset* of what was requested — callers that
    /// require all-or-nothing semantics (hot-reload install, the audit RPC) must check this
    /// before trusting `rules` as complete.
    pub rejected: Vec<RejectedRule>,
}

impl RuleSet {
    /// Whether any rule in this snapshot declares a direct-delegatecall alias.
    ///
    /// This is the switch for the whole feature: when it is `false`, capture is installed inert,
    /// no frame work happens, and screening ignores an unavailable observation set — so an
    /// event-only deployment keeps its exact existing behaviour and pays nothing.
    pub fn declares_any_alias(&self) -> bool {
        self.rules.iter().any(CompiledRule::declares_alias)
    }

    /// Every selected address across the snapshot, deduplicated. Drives the capture alias set and
    /// its order-independent digest.
    pub fn alias_addresses(&self) -> Vec<Address> {
        let mut out: Vec<Address> = self
            .rules
            .iter()
            .flat_map(|rule| rule.direct_delegatecalls.iter().map(|dd| dd.address))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The full identity of this snapshot, as stamped onto any observation set its alias
    /// configuration produced. A digest alone is never sufficient, because two different
    /// protocol/content versions can select the same addresses.
    pub fn snapshot_stamp(&self) -> crate::callstack::SnapshotStamp {
        crate::callstack::SnapshotStamp {
            protocol_version: self.protocol_version,
            content_version: self.content_version,
            alias_set_digest: crate::callstack::alias_set_digest(self.alias_addresses()),
        }
    }

    /// Returns the candidate rule indices for a log whose first topic is `topic0`.
    pub fn candidates_for_topic0(&self, topic0: &B256) -> &[usize] {
        self.index.get(topic0).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Returns candidates for an anonymous event with `indexed_topics` indexed inputs.
    pub fn candidates_for_anonymous(&self, indexed_topics: usize) -> &[usize] {
        self.anonymous_index.get(&indexed_topics).map(|v| v.as_slice()).unwrap_or(&[])
    }
}
