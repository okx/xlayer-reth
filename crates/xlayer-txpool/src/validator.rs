//! Structural (pre-state-access) validation for XLayerAA txs.
//!
//! What this module does:
//!
//! - Enforces hard structural limits (call count, account-change
//!   count, signature size, nonce-key range) against the constants
//!   declared in [`xlayer_consensus::aa::constants`].
//! - Rejects transactions whose `expiry` has already passed at the
//!   current block timestamp, with a small propagation buffer for
//!   in-flight txs.
//! - Rejects malformed `sender_auth` / `payer_auth` blobs early so
//!   the pool doesn't waste payload-builder cycles on them.
//! - Checks chain-id / tx-type byte.
//!
//! What this module deliberately doesn't do:
//!
//! - Nothing that requires chain state — that's the pool
//!   validator's job (account balance, on-chain 2D-nonce check,
//!   owner-set verification against `AccountConfig` storage, etc.).
//! - Custom verifier authorization — the expensive STATICCALL path
//!   runs at execution time, not at pool-admission time. Native
//!   verifiers (K1 / P256 / WebAuthn / Delegate) are validatable
//!   here but the full recovery is cheap enough that the call site
//!   can batch it.
//!
//! Mirrors Base's `eip8130_validate::validate_structure` shape so
//! error codes stay consistent across L2 tooling.

use alloy_eips::eip2718::Typed2718;
use alloy_primitives::U256;
use thiserror::Error;
use xlayer_consensus::{
    aa::parse_sender_auth, TxEip8130, XLayerPooledTxEnvelope, AA_TX_TYPE_ID,
    MAX_ACCOUNT_CHANGES_PER_TX, MAX_CALLS_PER_TX, MAX_SIGNATURE_SIZE, NONCE_KEY_MAX,
};

/// Minimum `expiry` margin (seconds) ahead of the current block
/// timestamp a tx needs to be pool-admissible. Accounts for P2P
/// propagation + one block of buffer so a barely-valid tx doesn't
/// get accepted at block `N` and rejected at block `N+1` mid-flight.
///
/// Matches Base's `AA_VALID_BEFORE_MIN_SECS = 3`.
pub const AA_VALID_BEFORE_MIN_SECS: u64 = 3;

/// Errors produced by [`validate_aa_structure`].
///
/// Kept as a flat enum rather than a nested hierarchy so RPC /
/// gossip error surfaces can pattern-match directly. Shape mirrors
/// Base's `eip8130_validate::AAValidationError` so clients can key
/// off the same discriminants.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AAValidationError {
    /// The tx's declared `chain_id` doesn't match the configured chain.
    #[error("chain id mismatch: tx={tx_chain_id}, expected={expected}")]
    ChainIdMismatch {
        /// Chain id found on the transaction.
        tx_chain_id: u64,
        /// Chain id the pool is configured for.
        expected: u64,
    },

    /// The tx's envelope reports a type byte other than [`AA_TX_TYPE_ID`].
    ///
    /// Surfaces when a caller hands an AA-validation entrypoint a
    /// non-AA pooled tx; the outer validator uses this discriminant
    /// to fall through to the standard-tx path rather than rejecting.
    #[error("not an xlayer aa transaction (type byte {ty:#04x})")]
    NotAnAATransaction {
        /// EIP-2718 type byte found.
        ty: u8,
    },

    /// `nonce_key` exceeds [`NONCE_KEY_MAX`].
    #[error("nonce_key {key} exceeds maximum {max}")]
    NonceKeyOutOfRange {
        /// The nonce key the tx carried.
        key: U256,
        /// Configured maximum.
        max: U256,
    },

    /// `calls` is empty. An AA tx must have at least one call phase
    /// — the whole point of the type is multi-call user intent.
    #[error("calls must contain at least one phase")]
    NoCalls,

    /// `calls` exceeds [`MAX_CALLS_PER_TX`]. Counts the flat sum of
    /// per-phase call counts (not phases).
    #[error("total calls {total} exceeds limit {limit}")]
    TooManyCalls {
        /// Actual total across all phases.
        total: usize,
        /// Configured limit.
        limit: usize,
    },

    /// `account_changes` exceeds [`MAX_ACCOUNT_CHANGES_PER_TX`].
    #[error("account changes {count} exceeds limit {limit}")]
    TooManyAccountChanges {
        /// Actual entries.
        count: usize,
        /// Configured limit.
        limit: usize,
    },

    /// `sender_auth` or `payer_auth` exceeds [`MAX_SIGNATURE_SIZE`].
    #[error("{field} blob {size} bytes exceeds limit {limit}")]
    SignatureTooLarge {
        /// Which field tripped the limit.
        field: &'static str,
        /// Actual size in bytes.
        size: usize,
        /// Configured limit.
        limit: usize,
    },

    /// `sender_auth` structure is malformed for the tx's `from`
    /// flag. Wraps [`parse_sender_auth`]'s error string so the RPC
    /// layer can surface the exact failure reason.
    #[error("invalid sender_auth: {reason}")]
    InvalidSenderAuth {
        /// Reason from [`parse_sender_auth`].
        reason: &'static str,
    },

    /// `expiry` has already passed at the current block timestamp
    /// (plus the [`AA_VALID_BEFORE_MIN_SECS`] propagation buffer).
    #[error("tx expired: expiry={expiry}, now={now}, min_buffer={min_buffer}")]
    Expired {
        /// `tx.expiry` (unix seconds).
        expiry: u64,
        /// Current block timestamp used for the check.
        now: u64,
        /// Required lead time, in seconds.
        min_buffer: u64,
    },
}

/// Structural validation for an AA tx.
///
/// Pool-admission entry point. Callers pass in the current block
/// timestamp so the expiry check uses whatever notion of "now" the
/// pool maintains (typically the parent header's timestamp or a
/// monotonic clock capped at it).
///
/// `chain_id` is the chain the pool is configured for. Passed in
/// rather than read from a chainspec handle because the validator
/// often runs without direct access to chainspec state (and all it
/// needs is the scalar).
///
/// Returns `Ok(())` for structurally valid txs, reserving any
/// state-dependent failure for the outer pool validator.
pub fn validate_aa_structure(
    tx: &TxEip8130,
    chain_id: u64,
    now: u64,
) -> Result<(), AAValidationError> {
    // Chain id first — a wrong-chain tx should never burn cycles on
    // the rest of the check path.
    if tx.chain_id != chain_id {
        return Err(AAValidationError::ChainIdMismatch {
            tx_chain_id: tx.chain_id,
            expected: chain_id,
        });
    }

    if tx.nonce_key > NONCE_KEY_MAX {
        return Err(AAValidationError::NonceKeyOutOfRange {
            key: tx.nonce_key,
            max: NONCE_KEY_MAX,
        });
    }

    if tx.calls.is_empty() {
        return Err(AAValidationError::NoCalls);
    }

    let total_calls: usize = tx.calls.iter().map(|phase| phase.len()).sum();
    if total_calls > MAX_CALLS_PER_TX {
        return Err(AAValidationError::TooManyCalls {
            total: total_calls,
            limit: MAX_CALLS_PER_TX,
        });
    }

    if tx.account_changes.len() > MAX_ACCOUNT_CHANGES_PER_TX {
        return Err(AAValidationError::TooManyAccountChanges {
            count: tx.account_changes.len(),
            limit: MAX_ACCOUNT_CHANGES_PER_TX,
        });
    }

    if tx.sender_auth.len() > MAX_SIGNATURE_SIZE {
        return Err(AAValidationError::SignatureTooLarge {
            field: "sender_auth",
            size: tx.sender_auth.len(),
            limit: MAX_SIGNATURE_SIZE,
        });
    }
    if tx.payer_auth.len() > MAX_SIGNATURE_SIZE {
        return Err(AAValidationError::SignatureTooLarge {
            field: "payer_auth",
            size: tx.payer_auth.len(),
            limit: MAX_SIGNATURE_SIZE,
        });
    }

    parse_sender_auth(tx).map_err(|reason| AAValidationError::InvalidSenderAuth { reason })?;

    // Expiry = 0 disables the check — used by deterministic test
    // fixtures and protocol-level txs that shouldn't age out.
    if tx.expiry != 0 && tx.expiry < now.saturating_add(AA_VALID_BEFORE_MIN_SECS) {
        return Err(AAValidationError::Expired {
            expiry: tx.expiry,
            now,
            min_buffer: AA_VALID_BEFORE_MIN_SECS,
        });
    }

    Ok(())
}

/// Validate a pooled envelope, dispatching on variant.
///
/// Returns `Ok(())` for non-AA envelopes (the outer validator
/// handles those via its standard path). Returns `Err` only for
/// structurally invalid AA txs — clean separation of concerns
/// means the caller doesn't have to match on envelope variant.
pub fn validate_pooled_structure(
    tx: &XLayerPooledTxEnvelope,
    chain_id: u64,
    now: u64,
) -> Result<(), AAValidationError> {
    match tx.as_aa() {
        Some(aa) => validate_aa_structure(aa, chain_id, now),
        None => {
            // Non-AA. Quick sanity: the type byte shouldn't be
            // AA's (defence in depth against a future routing bug
            // that wraps an AA body in the BuiltIn arm).
            let ty = tx.ty();
            if ty == AA_TX_TYPE_ID {
                return Err(AAValidationError::NotAnAATransaction { ty });
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes};
    use xlayer_consensus::Call;

    const CHAIN_ID: u64 = 196;
    const NOW: u64 = 1_800_000_000; // arbitrary unix timestamp

    fn base_aa_tx() -> TxEip8130 {
        TxEip8130 {
            chain_id: CHAIN_ID,
            from: Some(Address::repeat_byte(0x01)),
            nonce_key: U256::ZERO,
            nonce_sequence: 1,
            expiry: 0,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 10_000_000_000,
            gas_limit: 100_000,
            account_changes: Vec::new(),
            calls: vec![vec![Call { to: Address::repeat_byte(0xBB), data: Bytes::new() }]],
            payer: None,
            sender_auth: Bytes::from_static(&[0xFF; 65]),
            payer_auth: Bytes::new(),
        }
    }

    #[test]
    fn happy_path_accepts_minimal_tx() {
        let tx = base_aa_tx();
        assert!(validate_aa_structure(&tx, CHAIN_ID, NOW).is_ok());
    }

    #[test]
    fn wrong_chain_id_rejected() {
        let mut tx = base_aa_tx();
        tx.chain_id = 999;
        assert!(matches!(
            validate_aa_structure(&tx, CHAIN_ID, NOW),
            Err(AAValidationError::ChainIdMismatch { tx_chain_id: 999, expected: CHAIN_ID })
        ));
    }

    #[test]
    fn empty_calls_rejected() {
        let mut tx = base_aa_tx();
        tx.calls = Vec::new();
        assert_eq!(validate_aa_structure(&tx, CHAIN_ID, NOW), Err(AAValidationError::NoCalls));
    }

    #[test]
    fn too_many_calls_rejected() {
        let mut tx = base_aa_tx();
        // One phase with MAX_CALLS_PER_TX + 1 calls.
        tx.calls = vec![(0..MAX_CALLS_PER_TX + 1)
            .map(|_| Call { to: Address::repeat_byte(0xCC), data: Bytes::new() })
            .collect()];
        assert!(matches!(
            validate_aa_structure(&tx, CHAIN_ID, NOW),
            Err(AAValidationError::TooManyCalls { limit, .. }) if limit == MAX_CALLS_PER_TX
        ));
    }

    #[test]
    fn oversized_sender_auth_rejected() {
        let mut tx = base_aa_tx();
        tx.sender_auth = Bytes::from(vec![0xAA; MAX_SIGNATURE_SIZE + 1]);
        assert!(matches!(
            validate_aa_structure(&tx, CHAIN_ID, NOW),
            Err(AAValidationError::SignatureTooLarge { field: "sender_auth", .. })
        ));
    }

    #[test]
    fn malformed_sender_auth_surfaces_parse_error() {
        let mut tx = base_aa_tx();
        // EOA mode (from=None) requires exactly 65 bytes; feed 20.
        tx.from = None;
        tx.sender_auth = Bytes::from(vec![0xAA; 20]);
        assert!(matches!(
            validate_aa_structure(&tx, CHAIN_ID, NOW),
            Err(AAValidationError::InvalidSenderAuth { .. })
        ));
    }

    #[test]
    fn expired_tx_rejected_when_expiry_set() {
        let mut tx = base_aa_tx();
        tx.expiry = NOW - 1;
        assert!(matches!(
            validate_aa_structure(&tx, CHAIN_ID, NOW),
            Err(AAValidationError::Expired { .. })
        ));
    }

    #[test]
    fn expiry_zero_skips_the_check() {
        let mut tx = base_aa_tx();
        tx.expiry = 0;
        // With expiry=0 the check short-circuits; should succeed
        // regardless of `now`.
        assert!(validate_aa_structure(&tx, CHAIN_ID, u64::MAX).is_ok());
    }

    #[test]
    fn expiry_within_buffer_rejected() {
        let mut tx = base_aa_tx();
        // Exactly at the minimum buffer edge — must reject (the
        // check uses `<` with `now + buffer`, so `expiry ==
        // now+buffer` is still invalid).
        tx.expiry = NOW + AA_VALID_BEFORE_MIN_SECS - 1;
        assert!(matches!(
            validate_aa_structure(&tx, CHAIN_ID, NOW),
            Err(AAValidationError::Expired { .. })
        ));
    }

    #[test]
    fn expiry_just_past_buffer_accepted() {
        let mut tx = base_aa_tx();
        tx.expiry = NOW + AA_VALID_BEFORE_MIN_SECS;
        assert!(validate_aa_structure(&tx, CHAIN_ID, NOW).is_ok());
    }

    #[test]
    fn non_aa_envelope_is_ok_via_pooled_path() {
        use alloy_consensus::{SignableTransaction, TxEip1559};
        use alloy_primitives::{Signature, TxKind};
        use op_alloy_consensus::OpPooledTransaction;

        let tx = TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 1,
            gas_limit: 21_000,
            max_fee_per_gas: 2_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: TxKind::Call(Address::repeat_byte(0xAA)),
            value: U256::from(1u64),
            access_list: Default::default(),
            input: Bytes::new(),
        };
        let sig = Signature::from_scalars_and_parity(Default::default(), Default::default(), false);
        let env =
            XLayerPooledTxEnvelope::builtin(OpPooledTransaction::Eip1559(tx.into_signed(sig)));
        assert!(validate_pooled_structure(&env, CHAIN_ID, NOW).is_ok());
    }
}
