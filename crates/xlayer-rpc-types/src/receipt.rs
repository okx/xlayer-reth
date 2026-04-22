//! EIP-8130 (XLayerAA) receipt fields and the combined receipt
//! response shape served by `eth_getTransactionReceipt`.

use alloy_primitives::Address;
use op_alloy_rpc_types::OpTransactionReceipt;
use serde::{Deserialize, Serialize};

/// Extension fields returned on receipts for XLayerAA (tx type
/// `0x7B`) transactions.
///
/// Shape chosen to match Base's `Eip8130ReceiptFields` byte-for-byte
/// — same JSON key casing (`phaseStatuses`, `payer`), same optional
/// semantics on `phase_statuses` — so existing client tooling built
/// against Base's RPC surface works with XLayer without a code change.
///
/// `phase_statuses` decoding is the RPC-server's job (see
/// `xlayer-rpc::aa::receipt`); we just carry the shape here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Eip8130ReceiptFields {
    /// Address that actually paid gas for the transaction. Equal to
    /// `tx.from` for self-paying AA txs; differs when a sponsor
    /// (`tx.payer`) covers the fee.
    pub payer: Address,

    /// Per-phase execution status: one entry per entry in
    /// `tx.calls`, in order. `true` = phase committed, `false` =
    /// phase reverted.
    ///
    /// `None` for legacy receipts where the system log at
    /// `TX_CONTEXT_ADDRESS` was not emitted (pre-system-log AA
    /// execution) and the per-phase outcome can't be inferred from
    /// receipt-level status alone (multi-phase mixed outcomes). The
    /// handler always emits the log on current builds, so `None`
    /// indicates historical receipts only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_statuses: Option<Vec<bool>>,
}

/// JSON-RPC receipt shape for XLayer transactions.
///
/// Structurally: the standard OP receipt (`OpTransactionReceipt`,
/// carrying L1 fee info and deposit-specific fields) with an
/// optional flattened [`Eip8130ReceiptFields`] block that's only
/// populated for XLayerAA (0x7B) receipts.
///
/// Using `#[serde(flatten)]` on the extension preserves backward
/// compatibility: a client that only knows the upstream OP receipt
/// shape still parses correctly — the AA fields are additional keys
/// that existing `serde(deny_unknown_fields = false)` consumers
/// silently accept. Base does the same trick with their receipt
/// crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XLayerTransactionReceipt {
    /// Standard OP receipt envelope — `inner` (eth receipt with
    /// logs) plus `l1BlockInfo` and deposit-specific metadata.
    #[serde(flatten)]
    pub inner: OpTransactionReceipt,

    /// XLayerAA extension. `None` for non-AA txs; `Some` for 0x7B
    /// receipts, in which case its keys (`payer`, `phaseStatuses`)
    /// flatten into the top-level JSON object.
    #[serde(default, flatten, skip_serializing_if = "Option::is_none")]
    pub eip8130_fields: Option<Eip8130ReceiptFields>,
}

impl From<OpTransactionReceipt> for XLayerTransactionReceipt {
    fn from(inner: OpTransactionReceipt) -> Self {
        Self { inner, eip8130_fields: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn eip8130_fields_skip_empty_phase_statuses_key_when_none() {
        let fields = Eip8130ReceiptFields {
            payer: address!("0x1111111111111111111111111111111111111111"),
            phase_statuses: None,
        };
        let json = serde_json::to_string(&fields).unwrap();
        assert!(!json.contains("phaseStatuses"), "phase_statuses: None must not serialize a key");
        assert!(json.contains("payer"));
    }

    #[test]
    fn eip8130_fields_emit_phase_statuses_when_populated() {
        let fields = Eip8130ReceiptFields {
            payer: address!("0x2222222222222222222222222222222222222222"),
            phase_statuses: Some(vec![true, false, true]),
        };
        let json = serde_json::to_string(&fields).unwrap();
        assert!(json.contains("\"phaseStatuses\":[true,false,true]"));
    }

    /// Round-trip test: serialize → deserialize gives back the same
    /// value. Pins both the field naming (`phaseStatuses`, not
    /// `phase_statuses`) and the flatten/skip semantics.
    #[test]
    fn eip8130_fields_round_trip() {
        let original = Eip8130ReceiptFields {
            payer: address!("0xAabBccDdEeFf00112233445566778899aAbBccDd"),
            phase_statuses: Some(vec![false, true]),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: Eip8130ReceiptFields = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }
}
