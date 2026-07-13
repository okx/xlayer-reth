//! XLayer Filter — rule-driven, per-transaction risk-control interception for the XLayer
//! block builder (requirement XLOP-1142 / OKONE Mainnet RSC).
//!
//! The component screens every transaction produced during block building through an
//! `event match → JSONLogic eval → action merge (deny > audit > allow)` pipeline. `deny`
//! transactions are excluded from the block, `audit` transactions are submitted to the
//! external risk-control service (RCS) and only packaged once approved and a pre-package
//! consistency check passes. The risk policy itself lives entirely in RCS-delivered rules;
//! this crate carries no concrete business semantics.
//!
//! Authoritative wire/rule contract: `2026-07-09-rcs-filter-api-contract.md` (Binding).
//! Design: A-03 Technical Design (XLayer Filter).
//!
//! Key design principles (TD §3.4):
//! 1. The synchronous entry [`FilterHandle::screen_tx`] performs zero network IO — only
//!    in-memory dedup / matching / merge on the block-building hot path.
//! 2. All RCS network IO happens on background tokio workers ([`worker`]).
//! 3. Rules are hot-swapped atomically ([`Arc<RwLock<Arc<RuleSet>>>`]).

pub mod client;
pub mod clock;
pub mod config;
pub mod error;
pub mod handle;
pub mod matching;
pub mod pool;
pub mod quota_hash;
pub mod rules;
pub mod test_support;
pub mod worker;

#[cfg(test)]
mod integration_tests;

pub use client::{
    ActionItem, QueryParams, QueryResponse, QueryTx, RcsClient, ReqwestRcsClient, RulesResponse,
    SubmitRequest, SubmitResponse, SubmitTx, VersionResponse,
};
pub use clock::{Clock, SystemClock};
pub use config::{FilterConfig, SUPPORTED_PROTOCOL_VERSIONS};
pub use error::{FilterError, Result};
pub use handle::{FilterHandle, Screen, ScreenInput};
pub use pool::{BufferPool, BufferStatus};
pub use rules::{Action, CompiledRule, RuleSet, TimeoutAction};
