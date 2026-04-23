//! XLayer consensus-layer execution glue.
//!
//! Houses the XLayer-specific glue between the universal EIP-8130 wire
//! format (now in `op_alloy_consensus::eip8130`) and the execution-time
//! pipeline in `xlayer-revm` — primarily:
//!
//! - [`aa::build_aa_parts`] — "wire → exec env" boundary: reads a
//!   `TxEip8130` + chain-spec + state, verifies native auth, returns the
//!   [`xlayer_revm::transaction::XLayerAAParts`] the handler consumes.
//! - [`aa::XLayerAAGasSchedule`] — fork-bound intrinsic-gas table for AA
//!   txs, resolved per `OpSpecId`.
//!
//! Wire-level types ([`aa::TxEip8130`], `AccountChangeEntry`, …) are
//! re-exported from `aa` for ergonomic downstream use. They live in the
//! vendored op-alloy submodule so the `0x7B` variant ships alongside
//! `OpTxEnvelope` without a newtype wrapper.
//!
//! See `docs/xlayer-aa.md` for the protocol reference and a running log of
//! design-choice mistakes collected during the port.
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

#[cfg(not(feature = "std"))]
extern crate alloc as std;

pub mod aa;

pub use aa::{
    build_aa_parts, AccountChangeEntry, BuildError, BuiltAaParts, Call, ConfigChangeEntry,
    CreateEntry, DelegationEntry, Owner, OwnerChange, OwnerScope, TxEip8130, XLayerAAGasSchedule,
    AA_BASE_COST, AA_PAYER_TYPE, AA_TX_TYPE_ID, BYTECODE_BASE_GAS, BYTECODE_PER_BYTE_GAS,
    CHANGE_TYPE_CONFIG, CHANGE_TYPE_CREATE, CHANGE_TYPE_DELEGATION, CONFIG_CHANGE_OP_GAS,
    CONFIG_CHANGE_SKIP_GAS, CUSTOM_VERIFIER_GAS_CAP, DEPLOYMENT_HEADER_SIZE, EOA_AUTH_GAS,
    MAX_ACCOUNT_CHANGES_PER_TX, MAX_CALLS_PER_TX, MAX_CONFIG_OPS_PER_TX, MAX_SIGNATURE_SIZE,
    NONCE_KEY_COLD_GAS, NONCE_KEY_MAX, NONCE_KEY_WARM_GAS, OP_AUTHORIZE_OWNER, OP_REVOKE_OWNER,
    SLOAD_GAS,
};
