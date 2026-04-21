//! EIP-8130 account-abstraction transaction — wire-level types and
//! wire-to-execution conversion.

mod address;
mod build;
mod constants;
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
