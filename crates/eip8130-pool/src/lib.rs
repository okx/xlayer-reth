//! XLayer EIP-8130 Native Account Abstraction - Transaction Pool
//!
//! This crate implements the AA transaction pool with 2D nonce ordering,
//! dual placement into both the AA side pool and the standard OP pool,
//! and First-Access-to-Later (FAL) invalidation tracking.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// Deps used in upcoming modules (Phase 3+), suppress unused warnings during skeleton phase.
use alloy_primitives as _;
use reth_transaction_pool as _;
use tracing as _;
use xlayer_eip8130_consensus as _;
use xlayer_eip8130_revm as _;
