//! Rule model, loading/validation, JSONLogic evaluator and topic0 index.
//!
//! Wire rule schema is deserialized from the RCS `GET /rules` response, validated per-rule,
//! and compiled into [`CompiledRule`] with a precomputed `topic0`
//! per named event, and indexed by `topic0 → [rule index]` for fast candidate lookup.

mod jsonlogic;
mod model;
mod validation;

/// Maximum named event aliases in one rule. Production snapshots currently use at most three;
/// eight leaves room for richer joins while placing a hard bound on recursive binding depth.
pub const MAX_EVENTS_PER_RULE: usize = 8;

/// Maximum `direct_delegatecalls` aliases in one rule, following [`MAX_EVENTS_PER_RULE`] so a
/// rule's recursive candidate-binding depth stays bounded by rule configuration rather than by
/// transaction shape.
pub const MAX_DIRECT_DELEGATECALL_ALIASES: usize = 8;

/// Default maximum complete event bindings evaluated across all candidate rules for one
/// transaction. This is only a default: the authoritative per-transaction limit is configured
/// (`FilterConfig::max_event_bindings_per_tx`) and threaded into the matcher, so callers without a
/// config (e.g. the audit RPC path) fall back to this value.
pub const DEFAULT_MAX_EVENT_BINDINGS_PER_TX: usize = 10_000;

/// Crate-internal: the compiled-condition evaluator used by the matching hot path.
pub(crate) use jsonlogic::eval_compiled;
pub use jsonlogic::{truthy, Bindings};
pub use model::{
    AbiInput, Action, CompiledCondition, CompiledDirectDelegatecall, CompiledEvent, CompiledInput,
    CompiledRule, EventAbi, RawCallstack, RawRule, RejectedRule, RuleSet, TimeoutAction,
    UnknownFields,
};
/// The unknown-field gate. Used by the `GET /rules` response wire type so the response root is
/// as strict as every rule level below it, and exported under the clearer name
/// `reject_unknown_fields` for a consumer that owns its own request wire type (the audit RPC)
/// and must be exactly as strict about a speculative or misspelled field.
pub use validation::reject_unknown;
pub use validation::reject_unknown as reject_unknown_fields;
pub(crate) use validation::RULES_RESPONSE_LOCATION;
pub use validation::{compile_rule, load_rules};
