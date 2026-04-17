//! XLayer EIP-8130 Native Account Abstraction - Consensus Types and Logic
//!
//! This crate contains the core types, constants, and logic for EIP-8130 Native AA support.
//! It serves as the foundation layer that other AA crates depend on.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// Phase 3+ dependencies — suppress unused warnings until those modules land.
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
    DELEGATE_VERIFIER_ADDRESS, DEPLOYED_SYSTEM_CONTRACT_ADDRESSES, EXTERNAL_CALLER_VERIFIER,
    K1_VERIFIER_ADDRESS, NONCE_MANAGER_ADDRESS, P256_RAW_VERIFIER_ADDRESS,
    P256_WEBAUTHN_VERIFIER_ADDRESS, REVOKED_VERIFIER, TX_CONTEXT_ADDRESS,
};

pub mod system_bytecodes;

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

#[cfg(feature = "evm")]
mod accessors;
#[cfg(feature = "evm")]
pub use accessors::{
    increment_nonce_op, is_owner_authorized, read_change_sequence, read_lock_state, read_nonce,
    read_owner_config, write_owner_config_op, LockState,
};

#[cfg(feature = "evm")]
mod execution;
#[cfg(feature = "evm")]
pub use execution::{
    auto_delegation_code, build_execution_calls, config_change_sequence, config_change_writes,
    gas_refund, max_execution_gas_cost, nonce_increment_write, owner_registration_writes,
    CodePlacement, ExecutionCall, PhaseResult, SequenceUpdateInfo, StorageWrite, TxContextValues,
};

#[cfg(feature = "evm")]
mod precompiles;
#[cfg(feature = "evm")]
pub use precompiles::{
    handle_nonce_manager, handle_tx_context, PrecompileError, NONCE_MANAGER_GAS, TX_CONTEXT_GAS,
};

#[cfg(feature = "evm")]
mod validation;
#[cfg(feature = "evm")]
pub use validation::{
    check_lock_state, check_payer_authorization, check_sender_authorization, decode_verify_return,
    encode_verify_call, implicit_eoa_owner_id, resolve_sender, validate_config_change_sequences,
    validate_expiry, validate_nonce, validate_structure, ValidationError,
};

#[cfg(feature = "native-verifier")]
mod native_verifier;
#[cfg(feature = "native-verifier")]
pub use native_verifier::{try_native_verify, NativeVerifyError, NativeVerifyResult};

/// Returns `true` if the given transaction type byte is an AA transaction.
pub const fn is_aa_tx_type(tx_type: u8) -> bool {
    tx_type == AA_TX_TYPE_ID
}

/// Validates that a block does not contain AA transactions (type 0x7B)
/// when the Native AA hardfork is not yet active.
///
/// Returns `Ok(())` if the block is valid, or `Err` with the index of the
/// first offending transaction.
///
/// # Arguments
/// * `is_aa_active` — whether the Native AA fork is active at the block's timestamp.
///   The caller is responsible for computing this (e.g. via `xlayer_chainspec::is_native_aa_active`).
/// * `tx_types` — an iterator over the EIP-2718 type bytes of transactions in the block body.
pub fn validate_block_no_aa_tx(
    is_aa_active: bool,
    tx_types: impl Iterator<Item = u8>,
) -> Result<(), AaTxBeforeActivationError> {
    if is_aa_active {
        return Ok(());
    }
    for (idx, ty) in tx_types.enumerate() {
        if is_aa_tx_type(ty) {
            return Err(AaTxBeforeActivationError { tx_index: idx });
        }
    }
    Ok(())
}

/// Error returned when a block contains an AA transaction (type 0x7B)
/// before the Native AA hardfork is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaTxBeforeActivationError {
    /// Index of the first AA transaction in the block body.
    pub tx_index: usize,
}

impl core::fmt::Display for AaTxBeforeActivationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "block contains AA transaction (type 0x7B) at index {} before NativeAA hardfork activation",
            self.tx_index
        )
    }
}

impl std::error::Error for AaTxBeforeActivationError {}

#[cfg(test)]
mod tests {
    use super::*;

    const EIP1559: u8 = 0x02;

    #[test]
    fn validate_block_no_aa_tx_empty_block() {
        assert!(validate_block_no_aa_tx(false, std::iter::empty()).is_ok());
    }

    #[test]
    fn validate_block_no_aa_tx_standard_txs_before_activation() {
        let types = vec![EIP1559, EIP1559, 0x03];
        assert!(validate_block_no_aa_tx(false, types.into_iter()).is_ok());
    }

    #[test]
    fn validate_block_no_aa_tx_rejects_aa_before_activation() {
        let types = vec![EIP1559, AA_TX_TYPE_ID, EIP1559];
        let err = validate_block_no_aa_tx(false, types.into_iter()).unwrap_err();
        assert_eq!(err.tx_index, 1);
        assert!(err.to_string().contains("index 1"));
    }

    #[test]
    fn validate_block_no_aa_tx_reports_first_offending() {
        let types = vec![AA_TX_TYPE_ID, EIP1559, AA_TX_TYPE_ID];
        let err = validate_block_no_aa_tx(false, types.into_iter()).unwrap_err();
        assert_eq!(err.tx_index, 0);
    }

    #[test]
    fn validate_block_no_aa_tx_allows_aa_after_activation() {
        let types = vec![EIP1559, AA_TX_TYPE_ID, AA_TX_TYPE_ID];
        assert!(validate_block_no_aa_tx(true, types.into_iter()).is_ok());
    }

    #[test]
    fn validate_block_no_aa_tx_allows_empty_after_activation() {
        assert!(validate_block_no_aa_tx(true, std::iter::empty()).is_ok());
    }
}
