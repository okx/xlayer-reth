//! XLayer JSON-RPC response types.
//!
//! Currently houses the EIP-8130 (XLayerAA) receipt extension —
//! [`Eip8130ReceiptFields`] and [`XLayerTransactionReceipt`], a thin
//! wrapper over [`op_alloy_rpc_types::OpTransactionReceipt`] that
//! flattens the AA fields into the same JSON payload that
//! `eth_getTransactionReceipt` already returns for standard OP
//! transactions.
//!
//! Scoped deliberately narrow: no RPC servers, no handlers, no
//! state-provider wiring — those live in `xlayer-rpc` / `xlayer-reth-node`.
//! This crate is pure types so it can be depended on by client-side
//! tooling (the future XLayer SDK, dev scripts) without pulling in
//! the reth / op-reth server stack.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod receipt;

pub use receipt::{Eip8130ReceiptFields, XLayerTransactionReceipt};
