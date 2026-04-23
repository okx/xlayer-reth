//! XLayerAA (EIP-8130) execution-side glue.
//!
//! # Naming convention
//!
//! - [`TxEip8130`] — the universal wire struct per EIP-8130. Lives in
//!   `op_alloy_consensus::eip8130` (vendored patch; see
//!   `deps/optimism/rust/op-alloy/crates/consensus/src/transaction/eip8130/`).
//!   Re-exported here for ergonomic downstream use.
//! - `XLayerAA*` types (e.g. [`XLayerAAGasSchedule`], `XLayerAAHandler` in
//!   `xlayer-revm`) — XLayer's **specific application** of the EIP: the
//!   predeploys, verifier address set, handler branches, activation
//!   schedule. Branded to distinguish XLayer's choices from the
//!   universal spec.
//!
//! The split is deliberate: the wire format is universal, the runtime
//! pipeline is ours. Files under this module are XLayer-specific
//! (`build`, `gas_schedule`); the wire codec and native verifier helpers
//! come straight from op-alloy.

mod build;
mod gas_schedule;

pub use build::{build_aa_parts, BuildError, BuiltAaParts};
pub use gas_schedule::XLayerAAGasSchedule;

// --- Re-exports from op_alloy_consensus::eip8130 ---------------------------
//
// Downstream code (xlayer-revm, xlayer-txpool, xlayer-rpc, xlayer-builder)
// imports the wire types through this module to keep import sites stable if
// the vendored path ever changes.

pub use op_alloy_consensus::eip8130::{
    auth_verifier_kind, config_change_digest, create2_address, delegate_recover, deployment_code,
    deployment_header, derive_account_address, effective_salt, k1_owner_id, k1_recover,
    native_verify, p256_raw_recover, p256_webauthn_recover, parse_sender_auth,
    payer_signature_hash, sender_signature_hash, verifier_kind, AccountChangeEntry, Call,
    ConfigChangeEntry, CreateEntry, DelegationEntry, NativeVerifier, NativeVerifyError,
    NativeVerifyResult, Owner, OwnerChange, OwnerScope, ParsedSenderAuth, TxEip8130, VerifierKind,
    AA_BASE_COST, AA_PAYER_TYPE, AA_TX_TYPE_ID, BYTECODE_BASE_GAS, BYTECODE_PER_BYTE_GAS,
    CHANGE_TYPE_CONFIG, CHANGE_TYPE_CREATE, CHANGE_TYPE_DELEGATION, CONFIG_CHANGE_OP_GAS,
    CONFIG_CHANGE_SKIP_GAS, CUSTOM_VERIFIER_GAS_CAP, DELEGATE_VERIFIER_ADDRESS,
    DEPLOYMENT_HEADER_SIZE, EOA_AUTH_GAS, K1_VERIFIER_ADDRESS, MAX_ACCOUNT_CHANGES_PER_TX,
    MAX_CALLS_PER_TX, MAX_CONFIG_OPS_PER_TX, MAX_SIGNATURE_SIZE, NONCE_KEY_COLD_GAS, NONCE_KEY_MAX,
    NONCE_KEY_WARM_GAS, OP_AUTHORIZE_OWNER, OP_REVOKE_OWNER, P256_RAW_VERIFIER_ADDRESS,
    P256_WEBAUTHN_VERIFIER_ADDRESS, SLOAD_GAS,
};
