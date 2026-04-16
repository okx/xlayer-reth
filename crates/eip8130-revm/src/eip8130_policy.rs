//! Execution-time policy enforcement for EIP-8130 AA transactions.
//!
//! Validates that the authenticated owner has the required permissions
//! (scope bits) for each operation the transaction requests:
//!
//! - **SENDER** scope to act as the transaction sender
//! - **PAYER** scope to sponsor gas
//! - **CONFIG** scope to authorise config changes
//! - **SIGNATURE** scope for signing
//!
//! The handler calls [`validate_sender_policy`] and [`validate_payer_policy`]
//! before entering the execution pipeline, and [`validate_config_policy`] before
//! processing each `ConfigChange` entry.

use alloy_primitives::{Address, B256};
use xlayer_eip8130_consensus::OwnerScope;

/// Errors from policy enforcement.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PolicyError {
    /// The sender's owner lacks the SENDER scope bit.
    #[error("sender owner {owner_id} on account {account} lacks SENDER scope (has {scope:#04x})")]
    SenderScopeMissing {
        /// The account address.
        account: Address,
        /// The owner ID.
        owner_id: B256,
        /// The owner's actual scope.
        scope: u8,
    },

    /// The payer's owner lacks the PAYER scope bit.
    #[error("payer owner {owner_id} on account {payer} lacks PAYER scope (has {scope:#04x})")]
    PayerScopeMissing {
        /// The payer address.
        payer: Address,
        /// The owner ID.
        owner_id: B256,
        /// The owner's actual scope.
        scope: u8,
    },

    /// The authorizer owner lacks the CONFIG scope bit.
    #[error(
        "authorizer owner {owner_id} on account {account} lacks CONFIG scope (has {scope:#04x})"
    )]
    ConfigScopeMissing {
        /// The account address.
        account: Address,
        /// The owner ID.
        owner_id: B256,
        /// The owner's actual scope.
        scope: u8,
    },

    /// The owner referenced by the config change is revoked.
    #[error("owner {owner_id} on account {account} is revoked")]
    OwnerRevoked {
        /// The account address.
        account: Address,
        /// The owner ID.
        owner_id: B256,
    },
}

/// Validates that the sender's authenticated owner has the SENDER scope.
///
/// Returns `Ok(())` if the owner's scope includes SENDER permission, or
/// if the scope is UNRESTRICTED (0x00).
pub fn validate_sender_policy(
    account: Address,
    owner_id: B256,
    scope: u8,
) -> Result<(), PolicyError> {
    if !OwnerScope::has(scope, OwnerScope::SENDER) {
        return Err(PolicyError::SenderScopeMissing { account, owner_id, scope });
    }
    Ok(())
}

/// Validates that the payer's authenticated owner has the PAYER scope.
///
/// Returns `Ok(())` if the owner's scope includes PAYER permission, or
/// if the scope is UNRESTRICTED (0x00).
pub fn validate_payer_policy(payer: Address, owner_id: B256, scope: u8) -> Result<(), PolicyError> {
    if !OwnerScope::has(scope, OwnerScope::PAYER) {
        return Err(PolicyError::PayerScopeMissing { payer, owner_id, scope });
    }
    Ok(())
}

/// Validates that the authorizer owner has the CONFIG scope for
/// processing a config change entry.
///
/// Returns `Ok(())` if the owner's scope includes CONFIG permission, or
/// if the scope is UNRESTRICTED (0x00).
pub fn validate_config_policy(
    account: Address,
    owner_id: B256,
    scope: u8,
) -> Result<(), PolicyError> {
    if !OwnerScope::has(scope, OwnerScope::CONFIG) {
        return Err(PolicyError::ConfigScopeMissing { account, owner_id, scope });
    }
    Ok(())
}

/// Full policy check for the sender + payer + config in one pass.
///
/// Validates:
/// 1. Sender has SENDER scope
/// 2. Payer has PAYER scope (skipped if self-pay)
/// 3. If there are config changes, the authorizer has CONFIG scope
///
/// Returns `Ok(())` if all checks pass.
#[allow(clippy::too_many_arguments)]
pub fn validate_full_policy(
    sender: Address,
    sender_owner_id: B256,
    sender_scope: u8,
    payer: Address,
    payer_owner_id: B256,
    payer_scope: u8,
    has_config_changes: bool,
    authorizer_owner_id: B256,
    authorizer_scope: u8,
) -> Result<(), PolicyError> {
    validate_sender_policy(sender, sender_owner_id, sender_scope)?;

    // Only check payer scope for sponsored transactions.
    if sender != payer {
        validate_payer_policy(payer, payer_owner_id, payer_scope)?;
    }

    if has_config_changes {
        validate_config_policy(sender, authorizer_owner_id, authorizer_scope)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: Address = Address::repeat_byte(0xAA);
    const PAYER_ADDR: Address = Address::repeat_byte(0xBB);
    const OWNER_ID: B256 = B256::repeat_byte(0x11);

    #[test]
    fn unrestricted_scope_passes_all() {
        // UNRESTRICTED = 0x00 passes any check.
        assert!(validate_sender_policy(ACCOUNT, OWNER_ID, OwnerScope::UNRESTRICTED).is_ok());
        assert!(validate_payer_policy(PAYER_ADDR, OWNER_ID, OwnerScope::UNRESTRICTED).is_ok());
        assert!(validate_config_policy(ACCOUNT, OWNER_ID, OwnerScope::UNRESTRICTED).is_ok());
    }

    #[test]
    fn sender_scope_required() {
        // SENDER bit set → ok.
        assert!(validate_sender_policy(ACCOUNT, OWNER_ID, OwnerScope::SENDER).is_ok());
        // SENDER + CONFIG → ok (superset).
        assert!(validate_sender_policy(ACCOUNT, OWNER_ID, OwnerScope::SENDER | OwnerScope::CONFIG)
            .is_ok());
    }

    #[test]
    fn sender_scope_missing_fails() {
        // Only PAYER bit → sender check fails.
        let err = validate_sender_policy(ACCOUNT, OWNER_ID, OwnerScope::PAYER).unwrap_err();
        assert!(matches!(err, PolicyError::SenderScopeMissing { .. }));
    }

    #[test]
    fn payer_scope_required() {
        assert!(validate_payer_policy(PAYER_ADDR, OWNER_ID, OwnerScope::PAYER).is_ok());
    }

    #[test]
    fn payer_scope_missing_fails() {
        let err = validate_payer_policy(PAYER_ADDR, OWNER_ID, OwnerScope::SENDER).unwrap_err();
        assert!(matches!(err, PolicyError::PayerScopeMissing { .. }));
    }

    #[test]
    fn config_scope_required() {
        assert!(validate_config_policy(ACCOUNT, OWNER_ID, OwnerScope::CONFIG).is_ok());
    }

    #[test]
    fn config_scope_missing_fails() {
        let err = validate_config_policy(ACCOUNT, OWNER_ID, OwnerScope::SENDER).unwrap_err();
        assert!(matches!(err, PolicyError::ConfigScopeMissing { .. }));
    }

    #[test]
    fn full_policy_self_pay_skips_payer() {
        // Self-pay: sender == payer, so payer scope is NOT checked.
        // The payer_scope here has 0 bits (would fail if checked),
        // but since sender == payer, it's skipped.
        let result = validate_full_policy(
            ACCOUNT,
            OWNER_ID,
            OwnerScope::SENDER,
            ACCOUNT, // same as sender
            OWNER_ID,
            OwnerScope::SIGNATURE, // would fail payer check
            false,
            OWNER_ID,
            0,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn full_policy_sponsored_checks_payer() {
        // Sponsored: sender != payer, payer scope IS checked.
        let result = validate_full_policy(
            ACCOUNT,
            OWNER_ID,
            OwnerScope::SENDER,
            PAYER_ADDR,
            OWNER_ID,
            OwnerScope::SIGNATURE, // missing PAYER bit
            false,
            OWNER_ID,
            0,
        );
        assert!(matches!(result, Err(PolicyError::PayerScopeMissing { .. })));
    }

    #[test]
    fn full_policy_with_config_changes() {
        let result = validate_full_policy(
            ACCOUNT,
            OWNER_ID,
            OwnerScope::SENDER,
            ACCOUNT,
            OWNER_ID,
            0,
            true, // has config changes
            OWNER_ID,
            OwnerScope::SENDER, // missing CONFIG bit
        );
        assert!(matches!(result, Err(PolicyError::ConfigScopeMissing { .. })));
    }

    #[test]
    fn full_policy_all_pass() {
        let result = validate_full_policy(
            ACCOUNT,
            OWNER_ID,
            OwnerScope::SENDER | OwnerScope::CONFIG,
            PAYER_ADDR,
            OWNER_ID,
            OwnerScope::PAYER,
            true,
            OWNER_ID,
            OwnerScope::CONFIG,
        );
        assert!(result.is_ok());
    }
}
