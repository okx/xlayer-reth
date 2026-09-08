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

/// Maximum complete event bindings evaluated across all candidate rules for one transaction.
/// 4,096 permits, for example, every combination of four matching logs across six aliases while
/// bounding synchronous Filter and Audit RPC work to a predictable order of magnitude.
pub const MAX_COMPLETE_EVENT_BINDINGS_PER_EVALUATION: usize = 4_096;

/// Crate-internal: the compiled-condition evaluator used by the matching hot path.
pub(crate) use jsonlogic::eval_compiled;
pub use jsonlogic::{truthy, Bindings};
pub use model::{
    AbiInput, Action, CompiledCondition, CompiledEvent, CompiledInput, CompiledRule, EventAbi,
    RawRule, RejectedRule, RuleSet, TimeoutAction,
};
pub use validation::{compile_rule, load_rules};
