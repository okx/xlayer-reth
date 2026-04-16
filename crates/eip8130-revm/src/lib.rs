//! XLayer EIP-8130 Native Account Abstraction - EVM Execution Engine Extensions
//!
//! This crate extends the EVM execution engine with AA transaction handling,
//! including the handler, precompiles, and execution policy.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod constants;
pub mod eip8130_parts;
pub mod eip8130_policy;
pub mod handler;
pub mod precompiles;
pub mod tx_context;
