//! XLayer consensus-layer wire types.
//!
//! Houses the [`TxEip8130`] transaction (tx type byte `0x7B`) along with the
//! sub-types (`Call`, `AccountChangeEntry`, …) that appear in the RLP
//! payload. The crate is deliberately scoped to **wire-level** concerns —
//! RLP encoding / EIP-2718 framing / `alloy_consensus::Transaction` trait
//! impls — so it can be reused by every layer that needs to parse an AA
//! transaction without pulling in execution, mempool, or crypto code.
//!
//! Downstream responsibilities (separate crates):
//!
//! - crypto verification of `sender_auth` / `payer_auth` — left to a later
//!   milestone;
//! - execution-time parts (`XLayerAAParts`) — lives in `xlayer-revm`;
//! - EVM handler / precompile interception — also `xlayer-revm`.
//!
//! See `docs/xlayer-aa.md` for the protocol reference and a running log of
//! design-choice mistakes collected during the port.
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

#[cfg(not(feature = "std"))]
extern crate alloc as std;

pub mod aa;
pub mod envelope;

pub use envelope::XLayerTxEnvelope;

pub use aa::{
    AccountChangeEntry, Call, ConfigChangeEntry, CreateEntry, DelegationEntry, Owner, OwnerChange,
    OwnerScope, TxEip8130, AA_BASE_COST, AA_PAYER_TYPE, AA_TX_TYPE_ID, BYTECODE_BASE_GAS,
    BYTECODE_PER_BYTE_GAS, CHANGE_TYPE_CONFIG, CHANGE_TYPE_CREATE, CHANGE_TYPE_DELEGATION,
    CONFIG_CHANGE_OP_GAS, CONFIG_CHANGE_SKIP_GAS, CUSTOM_VERIFIER_GAS_CAP, DEPLOYMENT_HEADER_SIZE,
    EOA_AUTH_GAS, MAX_ACCOUNT_CHANGES_PER_TX, MAX_CALLS_PER_TX, MAX_CONFIG_OPS_PER_TX,
    MAX_SIGNATURE_SIZE, NONCE_KEY_COLD_GAS, NONCE_KEY_MAX, NONCE_KEY_WARM_GAS, OP_AUTHORIZE_OWNER,
    OP_REVOKE_OWNER, SLOAD_GAS,
};
