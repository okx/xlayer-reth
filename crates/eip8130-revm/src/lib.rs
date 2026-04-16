//! XLayer EIP-8130 Native Account Abstraction - EVM Execution Engine Extensions
//!
//! This crate extends the EVM execution engine with AA transaction handling,
//! including the handler, precompiles, and execution policy.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// Deps used in upcoming modules (Phase 4+), suppress unused warnings during skeleton phase.
use alloy_primitives as _;
use revm as _;
use xlayer_eip8130_consensus as _;
