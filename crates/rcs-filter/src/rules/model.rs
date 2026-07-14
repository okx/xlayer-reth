//! Rule data model: raw (wire) shapes deserialized from RCS, and compiled shapes used by
//! the matching hot path. See contract §3.

use std::collections::HashMap;

use alloy_dyn_abi::DynSolType;
use alloy_primitives::{Address, B256};
use serde::{Deserialize, Serialize};

/// Rule action (contract §3.6). Priority when merging: `deny > audit > allow`.
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

/// Fallback applied when RCS is unresponsive past `total_retry_timeout` (contract §3.6).
/// `allow` = fail-open, `deny` = fail-close. Omitted `audit_timeout_action` defaults to
/// `allow` (contract §3.6 disambiguation / ADR-0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeoutAction {
    Deny,
    Allow,
}

impl TimeoutAction {
    /// Returns the stricter of two timeout actions (`deny` wins), used when a tx matches
    /// several audit rules with differing `audit_timeout_action` (FR-6, TD §4.7).
    pub fn stricter(self, other: TimeoutAction) -> TimeoutAction {
        match (self, other) {
            (TimeoutAction::Deny, _) | (_, TimeoutAction::Deny) => TimeoutAction::Deny,
            _ => TimeoutAction::Allow,
        }
    }
}

/// A single ABI input declaration inside an [`EventAbi`]. `name` is `Option` so the
/// loader can detect the "missing name" rejection case (contract §3.1(1)/§3.2).
#[derive(Debug, Clone, Deserialize)]
pub struct AbiInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub param_type: String,
    pub indexed: bool,
}

/// A named event declaration (contract §3.2). The map key in [`RawRule::event_abis`] is
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

/// Raw rule as delivered by RCS `GET /rules` (contract §3.1). `condition` is kept as a raw
/// JSON value (may be the literal `true` or a JSONLogic object).
#[derive(Debug, Clone, Deserialize)]
pub struct RawRule {
    pub id: String,
    #[serde(default)]
    pub contract_address: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    pub event_abis: std::collections::BTreeMap<String, EventAbi>,
    #[serde(default = "default_audit_types")]
    pub audit_types: Vec<String>,
    pub condition: serde_json::Value,
    pub action: Action,
    #[serde(default)]
    pub audit_timeout_action: Option<TimeoutAction>,
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
}

/// An immutable, validated rule set snapshot plus its topic0 index. Swapped atomically on
/// hot-reload (TD §4.9); the hot path only ever reads a fully-built snapshot.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    pub protocol_version: u32,
    pub content_version: u64,
    pub rules: Vec<CompiledRule>,
    /// `topic0 → indices into `rules`` of rules declaring an event with that topic0.
    pub index: HashMap<B256, Vec<usize>>,
    /// Number of indexed topics -> rule indices declaring an anonymous event with that shape.
    pub anonymous_index: HashMap<usize, Vec<usize>>,
}

impl RuleSet {
    /// Returns the candidate rule indices for a log whose first topic is `topic0`.
    pub fn candidates_for_topic0(&self, topic0: &B256) -> &[usize] {
        self.index.get(topic0).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Returns candidates for an anonymous event with `indexed_topics` indexed inputs.
    pub fn candidates_for_anonymous(&self, indexed_topics: usize) -> &[usize] {
        self.anonymous_index.get(&indexed_topics).map(|v| v.as_slice()).unwrap_or(&[])
    }
}
