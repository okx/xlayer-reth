//! Shared XLayerAA owner authorization policy helpers.

use revm::primitives::Address;

/// Effective in-transaction owner state produced by ordered config changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingOwnerState {
    /// Owner is authorized with the given verifier and scope.
    Authorized {
        /// Verifier address associated with this owner.
        verifier: Address,
        /// Scope bitmask granted to this owner.
        scope: u8,
    },
    /// Owner is explicitly revoked.
    Revoked,
}

/// Validation errors for pending owner overlay checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingOwnerValidationError {
    /// Owner is explicitly revoked in pending overrides.
    Revoked,
    /// Pending verifier does not match the verifier used for authorization.
    VerifierMismatch {
        /// Verifier used by the authentication path.
        expected: Address,
        /// Verifier present in the pending owner overlay.
        actual: Address,
    },
    /// Pending owner lacks the required scope.
    MissingScope {
        /// Scope bit required by the auth role being validated.
        required_scope: u8,
    },
}

/// Returns `true` if a scope authorizes the required bitmask.
///
/// Scope `0` means unrestricted.
pub fn owner_scope_allows(scope: u8, required_scope: u8) -> bool {
    scope == 0 || (scope & required_scope) != 0
}

/// Validates an owner against pending in-transaction overrides.
pub fn validate_pending_owner_state(
    state: &PendingOwnerState,
    expected_verifier: Address,
    required_scope: u8,
) -> Result<(), PendingOwnerValidationError> {
    match state {
        PendingOwnerState::Revoked => Err(PendingOwnerValidationError::Revoked),
        PendingOwnerState::Authorized { verifier, scope } => {
            if *verifier != expected_verifier {
                return Err(PendingOwnerValidationError::VerifierMismatch {
                    expected: expected_verifier,
                    actual: *verifier,
                });
            }
            if !owner_scope_allows(*scope, required_scope) {
                return Err(PendingOwnerValidationError::MissingScope { required_scope });
            }
            Ok(())
        }
    }
}

/// Returns the pending owner state represented by an owner change operation.
///
/// - `0x01` => authorize
/// - `0x02` => revoke
/// - others => ignored (`None`)
pub fn pending_owner_state_for_change(
    change_type: u8,
    verifier: Address,
    scope: u8,
) -> Option<PendingOwnerState> {
    match change_type {
        0x01 => Some(PendingOwnerState::Authorized { verifier, scope }),
        0x02 => Some(PendingOwnerState::Revoked),
        _ => None,
    }
}
