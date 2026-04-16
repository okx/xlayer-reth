//! Mempool-level validation for EIP-8130 AA transactions.
//!
//! Validates structural integrity, expiry, intrinsic gas cost, and sender/payer
//! resolution before a transaction enters the pool.
//!
//! **Not performed here** (requires state access or EVM execution):
//! - Balance sufficiency check — caller must verify payer balance separately
//!   using [`max_payer_cost`] before pool insertion.
//! - Custom verifier STATICCALL — deferred to Phase 4 (EVM execution engine
//!   integration via `xlayer-eip8130-revm` crate).

use alloy_primitives::{Address, U256};

use xlayer_eip8130_consensus::{
    intrinsic_gas, resolve_sender, validate_expiry, validate_structure, TxEip8130, ValidationError,
};

/// Errors from mempool validation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum MempoolValidationError {
    /// Structural validation failed.
    #[error("structure: {0}")]
    Structure(#[from] ValidationError),
    /// The transaction type is not an AA transaction.
    #[error("not an AA transaction (type {0})")]
    NotAaTx(u8),
    /// The gas limit is below the intrinsic gas cost.
    #[error("gas limit {gas_limit} below intrinsic cost {intrinsic}")]
    IntrinsicGasTooLow {
        /// The gas limit.
        gas_limit: u64,
        /// The intrinsic gas cost.
        intrinsic: u64,
    },
    /// The payer has insufficient balance to cover gas.
    #[error("payer {payer} insufficient balance: need {required}, have {available}")]
    InsufficientBalance {
        /// The payer address.
        payer: Address,
        /// The required balance.
        required: U256,
        /// The available balance.
        available: U256,
    },
}

/// Validates an AA transaction for mempool acceptance.
///
/// This performs the subset of validation that does not require EVM execution
/// or state access:
/// - Structure validation (auth sizes, nonce-free rules, limits)
/// - Expiry check
/// - Intrinsic gas check
/// - Sender / payer resolution
///
/// **Caller responsibility:** after this succeeds, the caller must still verify
/// payer balance ≥ [`max_payer_cost`] using current state before pool insertion.
///
/// Returns `(sender, payer)` on success.
pub fn validate_aa_transaction(
    tx: &TxEip8130,
    block_timestamp: u64,
    recovered_sender: Option<Address>,
) -> Result<(Address, Address), MempoolValidationError> {
    // Structural validation
    validate_structure(tx)?;

    // Expiry check
    validate_expiry(tx, block_timestamp)?;

    // Intrinsic gas check
    // nonce_key_is_warm = false (cold, worst case for mempool), chain_id = 0 (unused default)
    let intrinsic = intrinsic_gas(tx, false, 0);
    if tx.gas_limit < intrinsic {
        return Err(MempoolValidationError::IntrinsicGasTooLow {
            gas_limit: tx.gas_limit,
            intrinsic,
        });
    }

    // Resolve sender
    let sender = resolve_sender(tx, recovered_sender).map_err(MempoolValidationError::Structure)?;

    // Resolve payer (use sender if self-pay)
    let payer = if tx.is_self_pay() { sender } else { tx.payer.unwrap_or(sender) };

    Ok((sender, payer))
}

/// Computes the maximum cost a payer must cover for an AA transaction.
pub fn max_payer_cost(tx: &TxEip8130) -> U256 {
    U256::from(tx.gas_limit) * U256::from(tx.max_fee_per_gas)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, Bytes, U256};
    use xlayer_eip8130_consensus::TxEip8130;

    use super::*;

    #[test]
    fn valid_self_pay_eoa_tx() {
        let recovered = Address::repeat_byte(0xAA);
        let tx = TxEip8130 { gas_limit: 100_000, max_fee_per_gas: 10, ..Default::default() };

        let (sender, payer) = validate_aa_transaction(&tx, 0, Some(recovered)).unwrap();
        assert_eq!(sender, recovered);
        assert_eq!(payer, recovered); // self-pay
    }

    #[test]
    fn eoa_without_recovered_sender_fails() {
        let tx = TxEip8130 { gas_limit: 100_000, max_fee_per_gas: 10, ..Default::default() };

        let result = validate_aa_transaction(&tx, 0, None);
        assert!(matches!(result, Err(MempoolValidationError::Structure(_))));
    }

    #[test]
    fn expired_tx_rejected() {
        let tx = TxEip8130 {
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            expiry: 100,
            ..Default::default()
        };

        let result = validate_aa_transaction(&tx, 200, Some(Address::repeat_byte(0xAA)));
        assert!(matches!(result, Err(MempoolValidationError::Structure(_))));
    }

    #[test]
    fn gas_below_intrinsic_rejected() {
        let tx = TxEip8130 {
            gas_limit: 1, // Way too low
            max_fee_per_gas: 10,
            ..Default::default()
        };

        let result = validate_aa_transaction(&tx, 0, Some(Address::repeat_byte(0xAA)));
        assert!(matches!(result, Err(MempoolValidationError::IntrinsicGasTooLow { .. })));
    }

    #[test]
    fn configured_sender_with_payer() {
        let sender = Address::repeat_byte(0xAA);
        let payer_addr = Address::repeat_byte(0xBB);
        let tx = TxEip8130 {
            from: Some(sender),
            payer: Some(payer_addr),
            payer_auth: Bytes::from(vec![0u8; 65]),
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            ..Default::default()
        };

        let (resolved_sender, resolved_payer) = validate_aa_transaction(&tx, 0, None).unwrap();
        assert_eq!(resolved_sender, sender);
        assert_eq!(resolved_payer, payer_addr);
    }

    #[test]
    fn max_payer_cost_calculation() {
        let tx = TxEip8130 { gas_limit: 100_000, max_fee_per_gas: 10, ..Default::default() };
        assert_eq!(max_payer_cost(&tx), U256::from(1_000_000));
    }
}
