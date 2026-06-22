//! XLayer chain-level blacklist interception — single crate, two layers.
//!
//! - [`rules`]   — pure decision logic (zero reth/revm coupling): the single source of truth
//!   for the feature's constants and checks.
//! - [`runtime`] — node/revm/reth adapters: runtime context (chain-id dispatch + snapshot
//!   handle + metrics), block-head snapshot read, ETH-balance reconstruction, deposit state
//!   surgery, the follower-face deposit hook, and the executor builder that installs it.
//!
//! The feature is a two-check execution gate (Transfer logs + native-ETH balance), with no
//! ingress mempool filter — the execution gate is the sole interception point.

pub mod rules;
pub mod runtime;

// Pure-logic (`rules`) re-exports — stable public API for the node/builder crates.
pub use rules::eval::{BlacklistEvaluator, Hit, HitCategory};
pub use rules::inspector::{BalanceCandidate, BlacklistInspector};
pub use rules::mirror::mirror_address_for_chain;
pub use rules::snapshot::{read_snapshot_at, BlacklistSnapshot, MirrorViewCaller, ViewCallError};
pub use rules::{abi, deposit, eval, inspector, metrics, mirror, snapshot};

// Node-adapter (`runtime`) re-exports.
pub use runtime::balance::{reconstruct_balance_candidates, FeeContext, ListedBalanceChange};
pub use runtime::deposit_apply::reverted_deposit_state;
pub use runtime::executor_builder::XLayerExecutorBuilder;
pub use runtime::follower_hook::XLayerDepositBlacklistHook;
pub use runtime::view::{read_blacklist_snapshot, RethMirrorViewCaller};
pub use runtime::{BlacklistRuntimeCtx, SnapshotHandle};
