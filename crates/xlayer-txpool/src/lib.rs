//! XLayer mempool.
//!
//! ## Current scope (M5a-c)
//!
//! - Structural validation of XLayerAA txs — cheap pre-state-access
//!   checks (time bounds, structural limits, cheap sig sanity).
//! - Validator wrapper that delegates non-AA txs to the upstream
//!   [`reth_optimism_txpool::OpTransactionValidator`] while running
//!   AA-specific checks on `0x7B` envelopes.
//! - Pool-builder wiring — wraps the standard reth pool using
//!   `XLayerPooledTxEnvelope` as the pool's consensus type so the
//!   node accepts 0x7B traffic end-to-end.
//!
//! ## Deferred to M5d
//!
//! - Dedicated 2D-nonce sub-pool (`Eip8130Pool`). The current pool
//!   uses the standard 1D-nonce heuristics, which for an AA tx
//!   means the `nonce_sequence` part is the ordering key; distinct
//!   `nonce_key`s land in separate sub-pools keyed on sender alone.
//!   Works for dev / native-verifier testing; mainnet perf needs
//!   the 2D split-and-merge.
//! - `Eip8130InvalidationIndex` — eviction on NonceManager /
//!   AccountConfig storage changes.
//! - `MergedBestTransactions` — priority-fee merge across multiple
//!   sub-pools.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub mod validator;

pub use validator::{
    validate_aa_structure, validate_pooled_structure, AAValidationError, AA_VALID_BEFORE_MIN_SECS,
};
