//! XLayer EIP-8130 Native Account Abstraction - Transaction Pool
//!
//! This crate implements the AA transaction pool with 2D nonce ordering,
//! dual placement into both the AA side pool and the standard OP pool,
//! and First-Access-to-Later (FAL) invalidation tracking.
//!
//! ## Architecture
//!
//! AA transactions (type `0x7B`) are placed into two pools simultaneously:
//!
//! - **Eip8130Pool (side pool)**: Stores all AA transactions keyed by the full
//!   2D identity `(sender, nonce_key, nonce_sequence)`. Manages 2D nonce
//!   ordering, expiry eviction, throughput tiers, and per-account limits.
//!
//! - **Standard OP Pool**: Also receives AA transactions (with `propagate: false`)
//!   for RPC backward compatibility — `eth_getTransactionByHash`,
//!   `txpool_content`, etc. work without modification.
//!
//! The `MergedBestTransactions` iterator interleaves ready transactions from
//! both pools during block building, sorted by effective priority fee.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// Phase 3+ dependency — suppress unused warning until maintenance task lands.
use tracing as _;

pub mod invalidation;
pub mod pool;
pub mod transaction;
pub mod validate;

#[cfg(test)]
pub(crate) mod test_utils;

// Re-export primary types for convenience.
pub use invalidation::{compute_invalidation_keys, Eip8130InvalidationIndex, InvalidationKey};
pub use pool::{
    BestEip8130Transactions, Eip8130Pool, Eip8130PoolConfig, Eip8130PoolError, SharedEip8130Pool,
    ThroughputTier, TierCheckResult,
};
pub use transaction::{AddOutcome, Eip8130Metadata, Eip8130SequenceId, Eip8130TxId};
pub use validate::{max_payer_cost, validate_aa_transaction, MempoolValidationError};
