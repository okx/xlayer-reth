//! Rule data model: raw (wire) shapes deserialized from RCS, and compiled shapes used by
//! the matching hot path.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use alloy_dyn_abi::DynSolType;
use alloy_primitives::{keccak256, Address, B256};
use serde::{Deserialize, Serialize};

use crate::callstack::SnapshotStamp;

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

/// Wire keys a level does not declare, captured verbatim by `#[serde(flatten)]`.
///
/// serde's default is to discard an unrecognized key, which turns a misspelled security
/// constraint into a silently absent one. Capturing them makes the loader able to name the
/// offending key; the map exists **only** to be rejected and is never consulted for matching.
pub type UnknownFields = BTreeMap<String, serde_json::Value>;

/// A single ABI input declaration inside an [`EventAbi`]. `name` is `Option` so the
/// loader can detect and reject a missing name.
#[derive(Debug, Clone, Deserialize)]
pub struct AbiInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub param_type: String,
    pub indexed: bool,
    /// Every wire key that is not one of this level's declared fields. Collected solely so the
    /// loader can reject it by name; nothing ever reads a value out of this map.
    #[serde(flatten)]
    pub unknown: UnknownFields,
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
    /// See [`AbiInput::unknown`].
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

fn default_audit_types() -> Vec<String> {
    vec!["quota".to_string()]
}

/// Raw rule as delivered by RCS `GET /rules`. `condition` is kept as a raw
/// JSON value (may be the literal `true` or a JSONLogic object).
#[derive(Debug, Clone, Deserialize)]
pub struct RawRule {
    pub id: String,
    #[serde(default)]
    pub contract_address: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    /// Named event declarations. Defaults to empty so a rule may select purely on call frames;
    /// a rule declaring neither events nor callstack selectors could never match and is rejected
    /// at load.
    #[serde(default)]
    pub event_abis: BTreeMap<String, EventAbi>,
    /// The optional `callstack` rule section.
    ///
    /// Present-vs-explicit-null is preserved deliberately: `None` is an absent field, which is a
    /// legitimate event-only rule, while `Some(None)` is an explicit `"callstack": null` and is
    /// rejected at load. Collapsing the two would make a null-valued section read as "the author
    /// declared no call-frame constraint", which is exactly the silent-absence failure the rest of
    /// this schema is built to prevent.
    #[serde(default, deserialize_with = "double_option")]
    pub callstack: Option<Option<RawCallstack>>,
    #[serde(default = "default_audit_types")]
    pub audit_types: Vec<String>,
    pub condition: serde_json::Value,
    pub action: Action,
    #[serde(default)]
    pub audit_timeout_action: Option<TimeoutAction>,
    /// See [`AbiInput::unknown`].
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

/// The `callstack` rule section as delivered on the wire.
///
/// Exactly one field is admissible, `direct_delegatecalls`. `deny_unknown_fields` is what makes
/// a near-miss spelling — `direct_delegatecall`, `directDelegatecalls`, `Direct_DelegateCalls` —
/// a loud rejection instead of a silently ignored key, so a constraint the author believes is
/// active can never be absent.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCallstack {
    /// Rule-local alias -> the proxy address whose own first-level `DELEGATECALL`s are observed.
    ///
    /// Deserialized through [`deserialize_unique_aliases`] rather than straight into a map so a
    /// repeated alias can never be silently collapsed to its last value.
    #[serde(deserialize_with = "deserialize_unique_aliases")]
    pub direct_delegatecalls: BTreeMap<String, String>,
}

/// Distinguishes an absent field from an explicitly null one.
///
/// With `#[serde(default)]`, an absent key yields `None` and a present key yields `Some(inner)`,
/// where `inner` is itself `None` for a JSON `null`. A plain `Option` cannot express that
/// difference, and the two must not be merged (see [`RawRule::callstack`]).
fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// Deserializes the `direct_delegatecalls` map while rejecting a repeated alias.
///
/// A plain `BTreeMap` silently keeps the last of several equal keys. Because every alias
/// contributes flat condition variables under its own name, a duplicate would quietly discard one
/// author-visible alias, so it is surfaced as a deserialization error and the document is
/// rejected whole.
fn deserialize_unique_aliases<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, MapAccess, Visitor};

    struct UniqueAliases;

    impl<'de> Visitor<'de> for UniqueAliases {
        type Value = BTreeMap<String, String>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a map of unique direct_delegatecalls alias names")
        }

        fn visit_map<A: MapAccess<'de>>(
            self,
            mut access: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            // Key and value are read separately so a wrong value type can be reported against
            // the alias it belongs to. `next_entry` would surface serde's bare "expected a
            // string", which does not say WHICH alias is malformed.
            while let Some(alias) = access.next_key::<String>()? {
                let address: String = access.next_value().map_err(|error| {
                    A::Error::custom(format!(
                        "direct_delegatecalls alias '{alias}' must be a lowercase 0x 20-byte address string: {error}"
                    ))
                })?;
                if out.insert(alias.clone(), address).is_some() {
                    return Err(A::Error::custom(format!(
                        "direct_delegatecalls declares alias '{alias}' more than once"
                    )));
                }
            }
            Ok(out)
        }
    }

    deserializer.deserialize_map(UniqueAliases)
}

/// A validated direct-delegatecall alias: a rule-local name and the proxy address it observes.
///
/// There are no narrowing constraint fields. Under this contract the `condition` does all the
/// narrowing, against the two variables the alias binds (`<alias>.delegateTo` and
/// `<alias>.delegateCodeHash`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledDirectDelegatecall {
    /// Rule-local alias; the `<alias>` prefix of this alias's two condition variables.
    pub alias: String,
    /// Proxy address whose own direct `DELEGATECALL`s are observed.
    pub address: Address,
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

/// A rule that passed load validation and is ready for matching.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub id: String,
    pub contract_address: Option<Address>,
    pub origin: Option<Address>,
    pub events: Vec<CompiledEvent>,
    /// Direct-delegatecall aliases in declared (alias) order. Empty for every rule that does not
    /// use the callstack section, which keeps those rules on the unchanged matching path.
    pub callstack: Vec<CompiledDirectDelegatecall>,
    pub audit_types: Vec<String>,
    pub condition: serde_json::Value,
    pub action: Action,
    /// Always resolved (defaults to `allow` when omitted for an `audit` rule).
    pub audit_timeout_action: TimeoutAction,
    /// Compiled form of `condition`, evaluated on the matching hot path.
    pub compiled_condition: CompiledCondition,
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
    /// Sorted, deduplicated set of every address named by an active rule's callstack selectors.
    /// An observation producer observes only frames touching this set, so observation cost scales
    /// with operator-controlled rule configuration rather than with transaction call depth. Empty
    /// means the whole callstack path is inert.
    pub callstack_selector_addresses: Vec<Address>,
}

impl RuleSet {
    /// Identity of this snapshot for stamping an observation set: the full rule identity
    /// (`protocol_version` + `content_version`) plus a digest of the selector addresses.
    ///
    /// The digest alone is deliberately not the identity. Two snapshots can name exactly the same
    /// addresses while evaluating different conditions against them, so observations captured
    /// under one must not be adjudicated by the other.
    pub fn stamp(&self) -> SnapshotStamp {
        SnapshotStamp {
            protocol_version: self.protocol_version,
            content_version: self.content_version,
            selector_digest: selector_set_digest(&self.callstack_selector_addresses),
        }
    }

    /// The capture selector set: every address named by an active rule's callstack selectors.
    pub fn callstack_selectors(&self) -> &[Address] {
        &self.callstack_selector_addresses
    }

    /// Whether any active rule declares callstack selectors. `false` means observation capture
    /// can be skipped entirely and every rule follows the pre-existing matching path.
    pub fn has_callstack_rules(&self) -> bool {
        !self.callstack_selector_addresses.is_empty()
    }

    /// How many active rules actually carry at least one call-frame selector.
    ///
    /// Deliberately NOT the length of [`Self::callstack_selectors`]: that is the sorted,
    /// deduplicated ADDRESS set, which answers "how much does capture observe" and is what the
    /// capture configuration and the snapshot digest are built from. This answers the different
    /// question an operator diagnostic asks — "how large is the rule surface that could have
    /// adjudicated this" — and the two diverge in both directions: several rules may name one
    /// address, and one rule may name several.
    pub fn selector_bearing_rule_count(&self) -> usize {
        self.rules.iter().filter(|rule| !rule.callstack.is_empty()).count()
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

/// `keccak256` over the sorted, deduplicated selector addresses, in 20-byte big-endian order.
///
/// Callers must pass an already sorted and deduplicated slice (as [`RuleSet`] stores it) so the
/// digest depends only on set membership, never on rule declaration order.
pub(crate) fn selector_set_digest(addresses: &[Address]) -> B256 {
    let mut buf = Vec::with_capacity(addresses.len() * Address::len_bytes());
    for address in addresses {
        buf.extend_from_slice(address.as_slice());
    }
    keccak256(&buf)
}
