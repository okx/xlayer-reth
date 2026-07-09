//! Rule model, loading/validation, JSONLogic evaluator and topic0 index.
//!
//! Wire rule schema is deserialized from the RCS `GET /rules` response (contract §3);
//! validated per-rule (FR-8), compiled into [`CompiledRule`] with a precomputed `topic0`
//! per named event, and indexed by `topic0 → [rule index]` for fast candidate lookup.

mod jsonlogic;
mod model;
mod validation;

pub use jsonlogic::{truthy, Bindings};
pub use model::{
    AbiInput, Action, CompiledEvent, CompiledInput, CompiledRule, EventAbi, RawRule, RuleSet,
    TimeoutAction,
};
pub use validation::{compile_rule, load_rules};
