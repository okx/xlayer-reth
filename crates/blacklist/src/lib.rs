//! XLayer chain-level blacklist interception — cross-client consensus core (op-reth).
//!
//! This crate is the single source of truth for the cross-client constants and the
//! pure decision logic of the "emergency freeze" blacklist feature (XLOP-1100). It is
//! consumed by the three execution faces (builder / follower newPayload / flashblocks)
//! and the ingress mempool validator. Every value the PRD/TD marks `跨端常量`
//! (cross-client constant) is defined here exactly once so the op-reth and op-geth
//! clients stay byte-for-byte consistent.
//!
//! The crate is deliberately **standalone** (no reth/op-reth/revm coupling), mirroring
//! [`xlayer-bridge-intercept`]. The revm `Inspector` trait impl, the EVM-config /
//! executor wrappers, and the mempool validator wrapper are thin adapters wired in the
//! node/builder crates; they call into the pure logic exposed here.
//!
//! Module map (TD §3.3):
//! - [`mirror`]   — `chain_id → hardcoded mirror address` dispatch (FR-6)
//! - [`abi`]      — `getBlacklist(uint256,uint256)` selector + encode/decode (FR-4)
//! - [`snapshot`] — block-head paginated enumeration + fail-open (FR-4)
//! - [`inspector`]— execution-time observation accumulator (call frames / balance / selfdestruct) (FR-2)
//! - [`eval`]     — three-check evaluator `call > log > balance` (FR-2)
//! - [`deposit`]  — included-as-reverted receipt/state field rules + exempt senders (FR-3)
//! - [`metrics`]  — Prometheus metrics (FR-7)
//! - [`error`]    — pool reject code + fixed message (FR-7)

pub mod abi;
pub mod deposit;
pub mod error;
pub mod eval;
pub mod inspector;
pub mod metrics;
pub mod mirror;
pub mod snapshot;

pub use error::{POOL_REJECT_CODE, POOL_REJECT_MESSAGE};
pub use eval::{BlacklistEvaluator, Hit, HitCategory};
pub use inspector::{BalanceCandidate, BlacklistInspector, CallFrameRecord};
pub use mirror::mirror_address_for_chain;
pub use snapshot::{read_snapshot_at, BlacklistSnapshot, MirrorViewCaller, ViewCallError};
