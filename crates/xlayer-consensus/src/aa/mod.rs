//! EIP-8130 account-abstraction transaction — wire-level types and
//! wire-to-execution conversion.
//!
//! # Naming convention
//!
//! - [`TxEip8130`] (this module) — the literal wire struct per
//!   EIP-8130. Named by EIP number to match the upstream
//!   `alloy_consensus` pattern (`TxEip1559`, `TxEip7702`, …) and
//!   signal that the encoding is the standard, not XLayer-specific.
//!   If Base (the reference-implementation maintainer) or any other
//!   chain ships EIP-8130, their wire struct will also be
//!   `TxEip8130` with the same bytes on the wire.
//! - `XLayerAA*` types ([`XLayerAAHandler`], [`XLayerAAEvm`],
//!   [`XLayerAATransaction`], [`XLayerHardfork::XLayerAA`],
//!   `xlayer-dev` chainspec, etc.) — XLayer's **specific
//!   application** of EIP-8130: the 7 predeploys, the verifier
//!   address set, the handler branches, the activation schedule.
//!   Branded to distinguish XLayer's choices from the universal
//!   spec.
//!
//! The split is deliberate: the wire format is universal, the
//! runtime pipeline is ours.
//!
//! [`XLayerAAHandler`]: https://docs.rs/xlayer-revm/latest/xlayer_revm/struct.XLayerAAHandler.html
//! [`XLayerAAEvm`]: https://docs.rs/xlayer-revm/latest/xlayer_revm/struct.XLayerAAEvm.html
//! [`XLayerAATransaction`]: https://docs.rs/xlayer-revm/latest/xlayer_revm/struct.XLayerAATransaction.html
//! [`XLayerHardfork::XLayerAA`]: https://docs.rs/xlayer-chainspec/latest/xlayer_chainspec/enum.XLayerHardfork.html#variant.XLayerAA

mod address;
mod build;
mod constants;
mod encoding;
mod gas_schedule;
mod native;
mod signature;
mod tx;
mod types;
mod verifier;

pub use address::{
    create2_address, deployment_code, deployment_header, derive_account_address, effective_salt,
};
pub use build::{build_aa_parts, BuildError, BuiltAaParts};
pub use constants::{
    AA_BASE_COST, AA_PAYER_TYPE, AA_TX_TYPE_ID, BYTECODE_BASE_GAS, BYTECODE_PER_BYTE_GAS,
    CONFIG_CHANGE_OP_GAS, CONFIG_CHANGE_SKIP_GAS, CUSTOM_VERIFIER_GAS_CAP, DEPLOYMENT_HEADER_SIZE,
    EOA_AUTH_GAS, MAX_ACCOUNT_CHANGES_PER_TX, MAX_CALLS_PER_TX, MAX_CONFIG_OPS_PER_TX,
    MAX_SIGNATURE_SIZE, NONCE_KEY_COLD_GAS, NONCE_KEY_MAX, NONCE_KEY_WARM_GAS, SLOAD_GAS,
};
pub use gas_schedule::XLayerAAGasSchedule;
pub use native::{
    delegate_recover, k1_owner_id, k1_recover, native_verify, p256_raw_recover,
    p256_webauthn_recover, NativeVerifyError, NativeVerifyResult,
};
pub use signature::{
    config_change_digest, parse_sender_auth, payer_signature_hash, sender_signature_hash,
    ParsedSenderAuth,
};
pub use tx::TxEip8130;
pub use types::{
    AccountChangeEntry, Call, ConfigChangeEntry, CreateEntry, DelegationEntry, Owner, OwnerChange,
    OwnerScope, CHANGE_TYPE_CONFIG, CHANGE_TYPE_CREATE, CHANGE_TYPE_DELEGATION, OP_AUTHORIZE_OWNER,
    OP_REVOKE_OWNER,
};
pub use verifier::{
    auth_verifier_kind, verifier_kind, NativeVerifier, VerifierKind, DELEGATE_VERIFIER_ADDRESS,
    K1_VERIFIER_ADDRESS, P256_RAW_VERIFIER_ADDRESS, P256_WEBAUTHN_VERIFIER_ADDRESS,
};
