//! XLayer EIP-8130 Native Account Abstraction - Consensus Types and Logic
//!
//! This crate contains the core types, constants, and logic for EIP-8130 Native AA support.
//! It serves as the foundation layer that other AA crates depend on.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// Phase 2+ dependencies — suppress unused warnings until those modules land.
use alloy_consensus as _;
use alloy_eips as _;

mod verifier;
pub use verifier::{auth_verifier_kind, verifier_kind, NativeVerifier, VerifierKind};

mod constants;
pub use constants::{
    VerifierGasCosts, AA_BASE_COST, AA_PAYER_TYPE, AA_TX_TYPE_ID, BYTECODE_BASE_GAS,
    BYTECODE_PER_BYTE_GAS, CONFIG_CHANGE_OP_GAS, CONFIG_CHANGE_SKIP_GAS, CUSTOM_VERIFIER_GAS_CAP,
    DEPLOYMENT_HEADER_SIZE, EOA_AUTH_GAS, EXPIRING_NONCE_GAS, EXPIRING_NONCE_SET_CAPACITY,
    MAX_ACCOUNT_CHANGES_PER_TX, MAX_CALLS_PER_TX, MAX_CONFIG_OPS_PER_TX, MAX_SIGNATURE_SIZE,
    NONCE_FREE_MAX_EXPIRY_WINDOW, NONCE_KEY_COLD_GAS, NONCE_KEY_MAX, NONCE_KEY_WARM_GAS, SLOAD_GAS,
};

mod types;
pub use types::{
    AccountChangeEntry, Call, ConfigChangeEntry, CreateEntry, DelegationEntry, Owner, OwnerChange,
    OwnerScope, CHANGE_TYPE_CONFIG, CHANGE_TYPE_CREATE, CHANGE_TYPE_DELEGATION, OP_AUTHORIZE_OWNER,
    OP_REVOKE_OWNER,
};

mod tx;
pub use tx::TxEip8130;

mod signature;
pub use signature::{
    config_change_digest, parse_sender_auth, payer_signature_hash, sender_signature_hash,
    ParsedSenderAuth,
};

mod gas;
pub use gas::{
    account_change_units, account_changes_cost, authorizer_verification_gas, bytecode_cost,
    delegate_inner_verifier, intrinsic_gas, intrinsic_gas_with_costs, nonce_key_cost,
    payer_auth_cost, payer_verification_gas, sender_auth_cost, sender_verification_gas,
    total_verification_gas, tx_payload_cost,
};

mod address;
pub use address::{
    create2_address, deployment_code, deployment_header, derive_account_address, effective_salt,
};

mod abi;
pub use abi::{
    CallTuple, ConfigOpTuple, IAccountConfig, INonceManager, ITxContext, IVerifier, OwnerTuple,
};

mod predeploys;
pub use predeploys::{
    is_account_config_known_deployed, is_native_verifier, mark_account_config_deployed,
    ACCOUNT_CONFIG_ADDRESS, DEFAULT_ACCOUNT_ADDRESS, DEFAULT_HIGH_RATE_ACCOUNT_ADDRESS,
    DELEGATE_VERIFIER_ADDRESS, EXTERNAL_CALLER_VERIFIER, K1_VERIFIER_ADDRESS,
    NONCE_MANAGER_ADDRESS, P256_RAW_VERIFIER_ADDRESS, P256_WEBAUTHN_VERIFIER_ADDRESS,
    REVOKED_VERIFIER, TX_CONTEXT_ADDRESS,
};

mod storage;
pub use storage::{
    account_state_slot, encode_account_state, encode_owner_config, expiring_ring_slot,
    expiring_seen_slot, lock_slot, nonce_slot, owner_config_slot, parse_account_state,
    parse_owner_config, read_sequence, sequence_base_slot, write_sequence, AccountState,
    ACCOUNT_STATE_BASE_SLOT, EXPIRING_RING_BASE_SLOT, EXPIRING_RING_PTR_SLOT,
    EXPIRING_SEEN_BASE_SLOT, LOCK_BASE_SLOT, NONCE_BASE_SLOT, OWNER_CONFIG_BASE_SLOT,
    SEQUENCE_BASE_SLOT,
};

mod purity;
pub use purity::{PurityScanner, PurityVerdict, PurityViolation, ViolationCategory};

/// Returns `true` if the given transaction type byte is an AA transaction.
pub const fn is_aa_tx_type(tx_type: u8) -> bool {
    tx_type == AA_TX_TYPE_ID
}
