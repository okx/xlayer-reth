//! Pure helpers for deriving [`Eip8130ReceiptFields`] from an
//! XLayerAA transaction + receipt logs.
//!
//! This module is intentionally free of server / provider / converter
//! wiring so it can be called from:
//!
//! - the future `XLayerReceiptConverter` that lands with the M5 node
//!   swap and threads these fields into `eth_getTransactionReceipt`,
//! - AA-aware test harnesses that want to assert on per-phase
//!   outcomes without spinning up the full RPC tower,
//! - dev-ops scripts that parse receipts post-hoc.
//!
//! Keeping the derivation logic in one callable function means the
//! "what is the payer / what are the phase statuses for this
//! receipt" answer doesn't drift between callers.

use alloy_primitives::{Address, Log};
use op_alloy_consensus::OpTxEnvelope;
use xlayer_consensus::TxEip8130;
use xlayer_revm::{extract_phase_statuses_from_logs, TX_CONTEXT_ADDRESS};
use xlayer_rpc_types::Eip8130ReceiptFields;

/// Returns the AA-specific receipt fields for a mined XLayerAA tx,
/// or `None` for non-AA envelopes.
///
/// `recovered_sender` is the recovered (or configured) sender of the
/// tx — passed in rather than re-recovered because the receipt
/// converter already has it on hand from the upstream
/// `ConvertReceiptInput::tx` recovered bundle. For self-paying AA
/// txs this is also the payer; for sponsored txs the payer comes
/// from `tx.effective_payer()`.
///
/// `logs` come from the committed receipt; caller passes
/// consensus-shaped [`alloy_primitives::Log`] rather than
/// RPC-shaped logs because the system-log decode is identity-free
/// (emitter address + topic + data), so we don't need the extra
/// RPC metadata (block hash, tx index, etc). Callers with
/// RPC-shaped logs can just map through `.as_ref()` — most
/// alloy-level RPC log types satisfy `AsRef<alloy_primitives::Log>`.
pub fn build_eip8130_fields(
    tx: &OpTxEnvelope,
    recovered_sender: Address,
    logs: &[Log],
) -> Option<Eip8130ReceiptFields> {
    let aa_tx = match tx {
        OpTxEnvelope::Eip8130(sealed) => sealed.inner(),
        _ => return None,
    };
    Some(build_fields_for_aa(aa_tx, recovered_sender, logs, None))
}

/// Same derivation, but given direct access to a [`TxEip8130`] and
/// an opt-in fallback status from the receipt-level `status` bit.
///
/// Exposed separately so the converter can pass the consensus
/// receipt's top-level success/failure into the fallback path for
/// historical receipts where the system log is absent. Current-build
/// receipts always carry the log, in which case the fallback stays
/// unused.
pub fn build_fields_for_aa<L: AsRef<Log>>(
    aa_tx: &TxEip8130,
    recovered_sender: Address,
    logs: &[L],
    receipt_status_fallback: Option<bool>,
) -> Eip8130ReceiptFields {
    let phase_statuses = extract_phase_statuses_from_logs(logs, TX_CONTEXT_ADDRESS).or_else(|| {
        receipt_status_fallback.and_then(|tx_ok| infer_phase_statuses(aa_tx.calls.len(), tx_ok))
    });
    let payer = if aa_tx.is_self_pay() { recovered_sender } else { aa_tx.effective_payer() };
    Eip8130ReceiptFields { payer, phase_statuses }
}

/// Infers per-phase statuses from the receipt-level status when the
/// system log is absent.
///
/// Faithful port of Base's `infer_eip8130_phase_statuses` semantics:
///
/// - 0 phases → empty vec (status is moot)
/// - 1 phase → clone the single receipt status
/// - N phases with receipt failure → every phase is false
///   (`TxEip8130` execution halts at the first failing phase; all
///   subsequent phases never run, which the spec treats as
///   implicitly failed)
/// - N phases with receipt success → **indeterminate** (individual
///   phases might have mixed outcomes but the tx still succeeded
///   overall), so the helper returns `None` to signal "unknown"
fn infer_phase_statuses(phase_count: usize, tx_success: bool) -> Option<Vec<bool>> {
    match (phase_count, tx_success) {
        (0, _) => Some(Vec::new()),
        (1, status) => Some(vec![status]),
        (_, false) => Some(vec![false; phase_count]),
        (_, true) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Bytes, LogData, U256};
    use xlayer_consensus::Call;

    fn sample_aa_tx() -> TxEip8130 {
        TxEip8130 {
            chain_id: 196,
            from: Some(address!("0x0101010101010101010101010101010101010101")),
            nonce_key: U256::from(0u64),
            nonce_sequence: 1,
            expiry: 0,
            gas_price: 10_000_000_000,
            gas_limit: 100_000,
            account_changes: Vec::new(),
            calls: vec![
                vec![Call {
                    to: address!("0xBBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbBbB"),
                    data: Bytes::from_static(&[0xDE, 0xAD]),
                }],
                vec![Call {
                    to: address!("0xcCCcCccCccCCcccCCcCCcCccCCCCCCccccCcCccC"),
                    data: Bytes::new(),
                }],
            ],
            payer: Some(address!("0x9999999999999999999999999999999999999999")),
            sender_auth: Bytes::from_static(&[0xFF; 65]),
            payer_auth: Bytes::from_static(&[0xAA; 65]),
        }
    }

    /// Construct a system log matching the format
    /// [`xlayer_revm::phase_statuses_system_log`] emits: phase bytes
    /// in `data`, topic = `phase_statuses_log_topic()`, emitter =
    /// [`TX_CONTEXT_ADDRESS`].
    fn make_phase_statuses_log(statuses: &[bool]) -> Log {
        let topic = xlayer_revm::phase_statuses_log_topic();
        let data: Vec<u8> = statuses.iter().map(|s| u8::from(*s)).collect();
        Log {
            address: TX_CONTEXT_ADDRESS,
            data: LogData::new_unchecked(vec![topic], Bytes::from(data)),
        }
    }

    #[test]
    fn returns_none_for_non_aa_envelope() {
        use alloy_consensus::{SignableTransaction, TxEip1559};
        use alloy_primitives::Signature;
        use op_alloy_consensus::OpTxEnvelope;
        let tx = TxEip1559 {
            chain_id: 196,
            nonce: 1,
            gas_limit: 21_000,
            max_fee_per_gas: 2_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: alloy_primitives::TxKind::Call(Address::repeat_byte(0x10)),
            value: U256::from(1u64),
            access_list: Default::default(),
            input: Bytes::new(),
        };
        let sig = Signature::from_scalars_and_parity(Default::default(), Default::default(), false);
        let env = OpTxEnvelope::Eip1559(tx.into_signed(sig));
        let empty_logs: [Log; 0] = [];
        assert!(build_eip8130_fields(&env, Address::repeat_byte(0x01), &empty_logs).is_none());
    }

    #[test]
    fn payer_for_sponsored_tx_is_effective_payer_not_sender() {
        let aa = sample_aa_tx();
        let sender = address!("0x0101010101010101010101010101010101010101");
        let expected_payer = address!("0x9999999999999999999999999999999999999999");
        let log = make_phase_statuses_log(&[true, false]);
        let fields = build_fields_for_aa(&aa, sender, &[log], None);
        assert_eq!(fields.payer, expected_payer, "payer must be tx.payer, not recovered sender");
    }

    #[test]
    fn payer_for_self_paying_tx_is_recovered_sender() {
        let mut aa = sample_aa_tx();
        aa.payer = None;
        let sender = address!("0x0101010101010101010101010101010101010101");
        let log = make_phase_statuses_log(&[true, true]);
        let fields = build_fields_for_aa(&aa, sender, &[log], None);
        assert_eq!(fields.payer, sender);
    }

    #[test]
    fn phase_statuses_from_system_log_take_precedence_over_fallback() {
        let aa = sample_aa_tx();
        let log = make_phase_statuses_log(&[true, false]);
        let fields = build_fields_for_aa(&aa, Address::repeat_byte(0x01), &[log], Some(true));
        assert_eq!(
            fields.phase_statuses,
            Some(vec![true, false]),
            "system log must win — fallback is only for legacy receipts"
        );
    }

    #[test]
    fn missing_log_triggers_fallback_with_multi_phase_failure() {
        let aa = sample_aa_tx(); // 2 phases
        let empty_logs: [Log; 0] = [];
        let fields = build_fields_for_aa(&aa, Address::repeat_byte(0x01), &empty_logs, Some(false));
        assert_eq!(fields.phase_statuses, Some(vec![false, false]));
    }

    #[test]
    fn missing_log_returns_none_for_multi_phase_success() {
        let aa = sample_aa_tx(); // 2 phases
        let empty_logs: [Log; 0] = [];
        let fields = build_fields_for_aa(&aa, Address::repeat_byte(0x01), &empty_logs, Some(true));
        assert_eq!(
            fields.phase_statuses, None,
            "multi-phase success without system log is indeterminate"
        );
    }

    #[test]
    fn single_phase_fallback_mirrors_tx_status() {
        let mut aa = sample_aa_tx();
        aa.calls = vec![vec![Call { to: Address::repeat_byte(0x22), data: Bytes::new() }]];
        let empty_logs: [Log; 0] = [];
        let fields = build_fields_for_aa(&aa, Address::repeat_byte(0x01), &empty_logs, Some(true));
        assert_eq!(fields.phase_statuses, Some(vec![true]));
        let empty_logs: [Log; 0] = [];
        let fields = build_fields_for_aa(&aa, Address::repeat_byte(0x01), &empty_logs, Some(false));
        assert_eq!(fields.phase_statuses, Some(vec![false]));
    }
}
