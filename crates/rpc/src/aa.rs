//! EIP-8130 AA RPC extensions.
//!
//! Provides helpers for AA-specific RPC functionality:
//!
//! - **2D nonce query**: reads the current nonce sequence from NonceManager
//!   precompile storage for a given `(address, nonce_key)` pair.
//! - **Receipt extension types**: `payer` and `phaseStatuses` fields that
//!   are appended to standard receipts for AA (type 0x7B) transactions.
//! - **Transaction identification**: helpers to detect AA transactions
//!   in raw bytes.

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};
use xlayer_eip8130_consensus::AA_TX_TYPE_ID;

// ── Transaction identification ───────────────────────────────────────

/// Returns `true` if the raw EIP-2718 encoded transaction starts with
/// the AA type byte (0x7B).
pub fn is_aa_raw_tx(raw: &[u8]) -> bool {
    raw.first().copied() == Some(AA_TX_TYPE_ID)
}

// ── Receipt extension types ──────────────────────────────────────────

/// AA-specific receipt extension fields.
///
/// These fields augment the standard Ethereum receipt for AA (type 0x7B)
/// transactions. Returned by `eth_getTransactionReceipt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Eip8130ReceiptExtra {
    /// The address that paid for gas (may differ from sender in sponsored
    /// transactions).
    pub payer: Address,
    /// Per-phase execution statuses.
    pub phase_statuses: Vec<Eip8130PhaseStatus>,
}

/// Execution result for a single phase within an AA transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Eip8130PhaseStatus {
    /// Whether this phase executed successfully.
    pub success: bool,
    /// Gas consumed by this phase (hex-encoded in JSON).
    #[serde(with = "alloy_serde::quantity")]
    pub gas_used: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Transaction identification ───────────────────────────────────

    #[test]
    fn detects_aa_tx_type() {
        assert!(is_aa_raw_tx(&[0x7B, 0x01, 0x02]));
    }

    #[test]
    fn rejects_non_aa_tx_types() {
        assert!(!is_aa_raw_tx(&[0x02, 0x01, 0x02])); // EIP-1559
        assert!(!is_aa_raw_tx(&[0x01, 0x01, 0x02])); // EIP-2930
        assert!(!is_aa_raw_tx(&[0x7E, 0x01, 0x02])); // deposit
    }

    #[test]
    fn rejects_empty_bytes() {
        assert!(!is_aa_raw_tx(&[]));
    }

    // ── Receipt JSON serialization ───────────────────────────────────

    #[test]
    fn receipt_extra_json_roundtrip() {
        let extra = Eip8130ReceiptExtra {
            payer: Address::repeat_byte(0xBB),
            phase_statuses: vec![
                Eip8130PhaseStatus { success: true, gas_used: 21_000 },
                Eip8130PhaseStatus { success: false, gas_used: 50_000 },
            ],
        };
        let json = serde_json::to_string(&extra).unwrap();
        let decoded: Eip8130ReceiptExtra = serde_json::from_str(&json).unwrap();
        assert_eq!(extra, decoded);
    }

    #[test]
    fn receipt_extra_uses_camel_case_keys() {
        let extra = Eip8130ReceiptExtra {
            payer: Address::ZERO,
            phase_statuses: vec![Eip8130PhaseStatus { success: true, gas_used: 100 }],
        };
        let json = serde_json::to_string(&extra).unwrap();
        assert!(json.contains("\"payer\""));
        assert!(json.contains("\"phaseStatuses\""));
        assert!(json.contains("\"gasUsed\""));
    }

    #[test]
    fn phase_status_gas_used_is_hex_encoded() {
        let status = Eip8130PhaseStatus { success: true, gas_used: 21_000 };
        let json = serde_json::to_string(&status).unwrap();
        // 21000 = 0x5208
        assert!(json.contains("\"0x5208\""), "expected hex gas, got: {json}");
    }

    #[test]
    fn phase_status_zero_gas() {
        let status = Eip8130PhaseStatus { success: false, gas_used: 0 };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"0x0\""), "expected hex zero, got: {json}");
    }

    #[test]
    fn receipt_extra_empty_phases() {
        let extra =
            Eip8130ReceiptExtra { payer: Address::repeat_byte(0xAA), phase_statuses: vec![] };
        let json = serde_json::to_string(&extra).unwrap();
        let decoded: Eip8130ReceiptExtra = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.phase_statuses.len(), 0);
    }
}
