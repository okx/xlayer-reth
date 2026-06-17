//! XLayer chain-level blacklist interception (XLOP-1100) — single crate, two layers.
//!
//! - [`rules`]   — cross-client consensus pure logic (zero reth/revm coupling): the single
//!   source of truth for every 跨端常量 (cross-client constant) and the decision logic, kept
//!   byte-for-byte consistent with op-geth.
//! - [`runtime`] — node/revm/reth adapters: runtime context (chain-id dispatch + snapshot
//!   handle + metrics), block-head snapshot read, ETH-balance reconstruction, deposit state
//!   surgery, the follower-face deposit hook, and the executor builder that installs it.
//!
//! The blacklist feature was reduced cross-client to a two-check execution gate (Transfer
//! logs + native-ETH balance), with no ingress mempool filter (the execution gate is the
//! sole interception point); see the PRD/IMPL.

pub mod rules;
pub mod runtime;

// Cross-client core (`rules`) re-exports — stable public API for the node/builder crates.
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
