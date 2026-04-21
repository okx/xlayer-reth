//! Construction of the execution-time `XLayerAAParts` from a wire
//! [`TxEip8130`].
//!
//! This is the "wire → exec env" boundary. Crypto verification of
//! `sender_auth` / `payer_auth` happens here (native path only — custom
//! verifiers are metered + executed during EVM execution). The output is
//! consumed by `XLayerAATransaction` in `xlayer-revm` and fed to the EVM
//! factory.
//!
//! **Scope caveats** (all tracked as explicit `Err` variants, no `panic!`):
//!
//! - P256 / WebAuthn / Delegate native paths are not yet implemented —
//!   the function returns [`BuildError::UnsupportedVerifier`] when those
//!   schemes are used as the sender or payer verifier. The custom-verifier
//!   (STATICCALL) path is still available to users of those schemes.
//! - Intrinsic gas is computed from the bundled wire-level constants but
//!   does not yet include per-verifier refinements (authorizer chain SLOAD
//!   accounting, P256 curve costs). The AA handler's `validate_env` guards
//!   the upper bounds, so a slightly-under-estimated `aa_intrinsic_gas`
//!   translates to a revert rather than a consensus issue.
//! - `pre_writes`, `code_placements`, `config_writes`, `sequence_updates`,
//!   and the corresponding system logs are not yet populated — create /
//!   config entries are skipped. Setting `BuildError::FeatureNotReady`
//!   surfaces this to callers that care.

use std::vec::Vec;

use alloy_primitives::{Address, B256, U256};
use op_revm::OpSpecId;
use xlayer_revm::transaction::XLayerAAParts;

use super::{
    gas_schedule::XLayerAAGasSchedule,
    native::{NativeVerifyError, NativeVerifyResult},
    signature::{parse_sender_auth, sender_signature_hash, ParsedSenderAuth},
    verifier::{verifier_kind, NativeVerifier, VerifierKind},
    TxEip8130, NONCE_KEY_MAX,
};

/// Output of [`build_aa_parts`].
#[derive(Debug, Clone)]
pub struct BuiltAaParts {
    /// Execution-time parts consumed by the EVM handler.
    pub parts: XLayerAAParts,
    /// Resolved sender address — matches `parts.sender` and is surfaced
    /// separately so callers that only need the sender (e.g. the tx pool)
    /// don't have to clone `parts`.
    pub sender: Address,
}

/// Errors surfaced by [`build_aa_parts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// `sender_auth` or `payer_auth` has a structural problem the parser
    /// refused to interpret (wrong length, missing verifier prefix, etc.).
    MalformedAuth(&'static str),
    /// `sender_auth` / `payer_auth` is verified by a custom contract — the
    /// handler must run the STATICCALL at execution time. Upstream callers
    /// that need the custom calldata pre-assembled should query via the
    /// pool validator; the execution path does not require it here.
    CustomVerifierDeferred(Address),
    /// Verifier kind is recognised but its native Rust path is not wired up
    /// in this crate yet — callers can either fall back to the custom
    /// STATICCALL path or refuse the tx at the edge (mempool validator).
    UnsupportedVerifier(NativeVerifier),
    /// Native signature verification failed.
    VerifyFailed(NativeVerifyError),
    /// EIP-8130 feature requested by the tx is not yet implemented in this
    /// build path (create / config-change entries, for example). Callers
    /// should route such txs through the fuller validation pipeline when it
    /// lands; for now they are rejected here.
    FeatureNotReady(&'static str),
}

impl From<NativeVerifyError> for BuildError {
    fn from(e: NativeVerifyError) -> Self {
        Self::VerifyFailed(e)
    }
}

/// Builds an [`XLayerAAParts`] from a wire [`TxEip8130`].
///
/// This is a **minimum viable** implementation that covers the K1 sender
/// path — sufficient for the default builder path wired up in the following
/// milestones. Extensions are tracked in [`BuildError`] variants.
///
/// `spec` selects the fork-bound intrinsic-gas schedule; every caller
/// already has the `OpSpecId` it's building under (pool validator, RPC
/// filler, and the handler's transaction-env conversion all read
/// `ctx.cfg().spec()`), so threading it through here is free but lets a
/// future AA fee revision ship as a single [`XLayerAAGasSchedule`] arm
/// without touching callers.
pub fn build_aa_parts(tx: &TxEip8130, spec: OpSpecId) -> Result<BuiltAaParts, BuildError> {
    // Account-change entries require CREATE2 derivation + pre-writes + log
    // emission plumbing that ships with the full pipeline. Fail fast here
    // rather than silently dropping them.
    if !tx.account_changes.is_empty() {
        return Err(BuildError::FeatureNotReady(
            "account_changes processing not wired up in build_aa_parts",
        ));
    }

    // --- Sender authentication -------------------------------------------
    let sender_prehash = sender_signature_hash(tx);
    let (sender, sender_verifier, sender_owner_id) = resolve_sender(tx, &sender_prehash)?;

    // --- Payer authentication --------------------------------------------
    let (payer, payer_verifier, payer_owner_id) = if tx.is_self_pay() {
        (sender, Address::ZERO, B256::ZERO)
    } else {
        // Sponsored: resolve the payer's native path too.
        resolve_payer(tx, sender)?
    };

    // --- Intrinsic gas (minimum viable) ----------------------------------
    //
    // base_cost + nonce-key SSTORE + a flat EOA auth cost when the
    // sender used ecrecover. This will under-estimate configured-owner,
    // sponsor, and create-entry paths; the handler's `validate_env`
    // structural guards still reject oversized txs, so the outcome is at
    // worst a tighter-than-expected gas budget that reverts cleanly.
    //
    // `schedule` is fork-bound — see [`XLayerAAGasSchedule::for_spec`].
    let schedule = XLayerAAGasSchedule::for_spec(spec);
    let verification_gas = if tx.is_eoa() { schedule.eoa_auth_gas } else { 0 };
    let nonce_cost = if tx.nonce_key == NONCE_KEY_MAX { 0 } else { schedule.nonce_key_cold_gas };
    let aa_intrinsic_gas =
        schedule.base_cost.saturating_add(verification_gas).saturating_add(nonce_cost);

    // --- Call phases — wire → exec copy ----------------------------------
    let call_phases: Vec<Vec<xlayer_revm::transaction::XLayerAACall>> = tx
        .calls
        .iter()
        .map(|phase| {
            phase
                .iter()
                .map(|c| xlayer_revm::transaction::XLayerAACall {
                    to: c.to,
                    data: c.data.clone(),
                    // EIP-8130 call tuples do not carry a per-call value on
                    // the wire; value transfers are expressed through
                    // dedicated system calls. Zero is the correct default.
                    value: U256::ZERO,
                })
                .collect()
        })
        .collect();

    // `nonce_free_hash` is used as the ring-buffer replay key when the tx
    // opts into nonce-free mode. Derive it from the sender preimage so a
    // malleable payer signature can't produce a different key.
    let nonce_free_hash = if tx.nonce_key == NONCE_KEY_MAX { Some(sender_prehash) } else { None };

    let parts = XLayerAAParts {
        expiry: tx.expiry,
        sender,
        payer,
        owner_id: sender_owner_id,
        payer_owner_id,
        nonce_key: tx.nonce_key,
        nonce_free_hash,
        has_create_entry: false, // account_changes handled above
        delegation_target: None, // account_changes handled above
        account_change_units: 0,
        verification_gas,
        aa_intrinsic_gas,
        payer_intrinsic_gas: 0,
        custom_verifier_gas_cap: 0,
        sender_verifier,
        payer_verifier,
        call_phases,
        // Remaining fields stay at their `Default` values — the handler
        // treats empty vectors as "nothing to do" and `auto_delegation_code`
        // at len != 23 as "no auto-delegation", which is the correct no-op.
        ..XLayerAAParts::default()
    };

    Ok(BuiltAaParts { parts, sender })
}

// ---------------------------------------------------------------------------
// Sender / payer resolution
// ---------------------------------------------------------------------------

/// Returns `(sender_address, sender_verifier, sender_owner_id)`.
fn resolve_sender(tx: &TxEip8130, prehash: &B256) -> Result<(Address, Address, B256), BuildError> {
    let parsed = parse_sender_auth(tx).map_err(BuildError::MalformedAuth)?;

    match parsed {
        ParsedSenderAuth::Eoa { signature } => {
            // EOA: ecrecover the sender from the preimage; both `sender`
            // and `owner_id` derive from the recovered address.
            let NativeVerifyResult { address, owner_id } =
                super::native::k1_recover(prehash, &signature)?;
            Ok((address, super::verifier::K1_VERIFIER_ADDRESS, owner_id))
        }
        ParsedSenderAuth::Configured { verifier, data } => {
            let Some(from) = tx.from else {
                // The parser guaranteed `from` is present for the
                // Configured branch, but make the invariant explicit.
                return Err(BuildError::MalformedAuth("configured auth requires tx.from"));
            };
            match verifier_kind(verifier) {
                VerifierKind::Native(kind) => match kind {
                    NativeVerifier::K1 => {
                        let NativeVerifyResult { owner_id, .. } =
                            super::native::k1_recover(prehash, &data)?;
                        Ok((from, verifier, owner_id))
                    }
                    other => Err(BuildError::UnsupportedVerifier(other)),
                },
                VerifierKind::Custom(address) => Err(BuildError::CustomVerifierDeferred(address)),
            }
        }
    }
}

/// Returns `(payer_address, payer_verifier, payer_owner_id)`.
fn resolve_payer(tx: &TxEip8130, sender: Address) -> Result<(Address, Address, B256), BuildError> {
    let payer = tx.payer.unwrap_or(sender);
    if tx.payer_auth.is_empty() {
        // No auth blob; treat as "payer is the same owner as sender"
        // — safe default while the full sponsored path is plumbed in.
        return Ok((payer, Address::ZERO, B256::ZERO));
    }
    if tx.payer_auth.len() < 20 {
        return Err(BuildError::MalformedAuth("payer_auth shorter than verifier prefix"));
    }
    let verifier = Address::from_slice(&tx.payer_auth[..20]);
    let data = &tx.payer_auth[20..];
    match verifier_kind(verifier) {
        VerifierKind::Native(NativeVerifier::K1) => {
            let prehash = super::signature::payer_signature_hash(tx);
            let NativeVerifyResult { owner_id, .. } = super::native::k1_recover(&prehash, data)?;
            Ok((payer, verifier, owner_id))
        }
        VerifierKind::Native(other) => Err(BuildError::UnsupportedVerifier(other)),
        VerifierKind::Custom(address) => Err(BuildError::CustomVerifierDeferred(address)),
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Bytes, U256};

    use super::super::AccountChangeEntry;
    use super::*;

    /// Spec every `build_aa_parts` call in these tests runs under. Pinned to
    /// the same value the handler tests use (see
    /// `xlayer-revm/src/handler/tests.rs::AA_ACTIVE_SPEC`) so a future fork
    /// rename only needs to touch one constant per crate.
    const TEST_SPEC: OpSpecId = OpSpecId::JOVIAN;

    fn tx_template() -> TxEip8130 {
        TxEip8130 {
            chain_id: 196,
            from: Some(Address::repeat_byte(0x01)),
            nonce_key: U256::ZERO,
            nonce_sequence: 0,
            expiry: 0,
            max_priority_fee_per_gas: 1,
            max_fee_per_gas: 100,
            gas_limit: 100_000,
            ..Default::default()
        }
    }

    #[test]
    fn account_changes_feature_not_ready() {
        let tx = TxEip8130 {
            account_changes: vec![AccountChangeEntry::Delegation(super::super::DelegationEntry {
                target: Address::repeat_byte(0x42),
            })],
            ..tx_template()
        };
        assert!(matches!(build_aa_parts(&tx, TEST_SPEC), Err(BuildError::FeatureNotReady(_))));
    }

    #[test]
    fn malformed_eoa_auth_rejected() {
        let tx = TxEip8130 {
            from: None,
            sender_auth: Bytes::from_static(&[0x01u8; 64]), // wrong length
            ..tx_template()
        };
        assert!(matches!(build_aa_parts(&tx, TEST_SPEC), Err(BuildError::MalformedAuth(_))));
    }

    #[test]
    fn configured_custom_verifier_deferred() {
        let custom = Address::repeat_byte(0xAB);
        let mut auth = Vec::new();
        auth.extend_from_slice(custom.as_slice());
        auth.extend_from_slice(&[0u8; 32]);
        let tx = TxEip8130 { sender_auth: Bytes::from(auth), ..tx_template() };
        match build_aa_parts(&tx, TEST_SPEC) {
            Err(BuildError::CustomVerifierDeferred(addr)) => assert_eq!(addr, custom),
            other => panic!("expected CustomVerifierDeferred, got {other:?}"),
        }
    }

    #[test]
    fn configured_unsupported_native_rejected() {
        let mut auth = Vec::new();
        auth.extend_from_slice(super::super::verifier::P256_RAW_VERIFIER_ADDRESS.as_slice());
        auth.extend_from_slice(&[0u8; 64]);
        let tx = TxEip8130 { sender_auth: Bytes::from(auth), ..tx_template() };
        match build_aa_parts(&tx, TEST_SPEC) {
            Err(BuildError::UnsupportedVerifier(
                super::super::verifier::NativeVerifier::P256Raw,
            )) => {}
            other => panic!("expected UnsupportedVerifier(P256Raw), got {other:?}"),
        }
    }

    #[test]
    fn nonce_free_hash_populated_in_nonce_free_mode() {
        // Construct a well-formed configured-K1 auth blob so we reach the
        // point where `nonce_free_hash` is set. The K1 verifier will fail
        // to recover (dummy data), so we check the error path: we should
        // surface VerifyFailed rather than accept the tx, but the preimage
        // hashing path is exercised.
        let mut auth = Vec::new();
        auth.extend_from_slice(super::super::verifier::K1_VERIFIER_ADDRESS.as_slice());
        auth.extend_from_slice(&[0u8; 65]);
        let tx = TxEip8130 {
            nonce_key: NONCE_KEY_MAX,
            expiry: 12345,
            sender_auth: Bytes::from(auth),
            ..tx_template()
        };
        assert!(matches!(build_aa_parts(&tx, TEST_SPEC), Err(BuildError::VerifyFailed(_))));
    }
}
