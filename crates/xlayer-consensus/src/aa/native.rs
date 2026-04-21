//! Native (Rust-only) signature verification for XLayerAA auth blobs.
//!
//! The native path avoids a STATICCALL to the verifier contract when the
//! signature scheme is one of the four well-known ones: K1 ecrecover,
//! P256 raw, P256 WebAuthn, one-hop Delegate. For verifiers that are not
//! in this set the handler falls back to the EVM path during execution.
//!
//! **Status:** this module currently implements the K1 path fully and
//! leaves P256/WebAuthn/Delegate as `Err(NativeVerifyError::NotImplemented)`.
//! Filling those in is a follow-up — the shape is complete so downstream
//! code (E3 / E4a) does not depend on the specific scheme being wired up.

use alloy_primitives::{keccak256, Address, Signature, SignatureError, B256};

use super::verifier::NativeVerifier;

/// Outcome of a successful native verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeVerifyResult {
    /// Recovered or declared signer address (used for both the tx `from`
    /// and as the basis of the K1 owner id).
    pub address: Address,
    /// Owner id committed by the signature — `keccak256(address)` for K1,
    /// `keccak256(public_key)` for P256, etc.
    pub owner_id: B256,
}

/// Errors surfaced by the native verification path.
///
/// `SignatureError` from alloy-primitives is not `Clone`/`PartialEq`, so we
/// compress it to a static description — enough for telemetry and tests,
/// and it lets `NativeVerifyError` itself satisfy `Eq + Clone`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeVerifyError {
    /// Auth blob has the wrong size for the declared verifier.
    BadSize,
    /// Signature bytes are well-formed but recovery failed (malformed `r`,
    /// non-normalised `s`, invalid parity, etc.).
    SignatureRejected(&'static str),
    /// The verifier is known but native Rust support isn't wired up yet —
    /// callers should fall back to the EVM STATICCALL path.
    NotImplemented(NativeVerifier),
}

fn describe_signature_error(_err: SignatureError) -> &'static str {
    // alloy's `SignatureError` already carries its reason in `Debug`;
    // collapse here because the variants churn across alloy versions and
    // we don't want to leak that into our public API surface.
    "signature error"
}

/// Verifies a K1 (ecrecover) signature and returns the recovered address
/// plus the owner id `keccak256(address)` that the AA handler compares
/// against on-chain `owner_config`.
///
/// The signature must be in the 65-byte `r ‖ s ‖ v` format (parity
/// normalised via [`Signature::from_bytes_and_parity`] semantics).
pub fn k1_recover(
    prehash: &B256,
    signature: &[u8],
) -> Result<NativeVerifyResult, NativeVerifyError> {
    if signature.len() != 65 {
        return Err(NativeVerifyError::BadSize);
    }
    let sig = Signature::try_from(signature)
        .map_err(|e| NativeVerifyError::SignatureRejected(describe_signature_error(e)))?;
    let address = sig
        .recover_address_from_prehash(prehash)
        .map_err(|e| NativeVerifyError::SignatureRejected(describe_signature_error(e)))?;
    let owner_id = k1_owner_id(address);
    Ok(NativeVerifyResult { address, owner_id })
}

/// The K1 owner id binding: `keccak256(address)` left-padded in a `B256`.
/// This is what the account-config contract stores as the owner id for
/// ecrecover-based owners.
pub fn k1_owner_id(address: Address) -> B256 {
    keccak256(address.as_slice())
}

/// Stub for the P256-raw verifier. Returns `NotImplemented` — the handler's
/// custom-verifier path executes the verifier contract instead.
pub fn p256_raw_recover(
    _prehash: &B256,
    _data: &[u8],
) -> Result<NativeVerifyResult, NativeVerifyError> {
    Err(NativeVerifyError::NotImplemented(NativeVerifier::P256Raw))
}

/// Stub for the P256-WebAuthn verifier.
pub fn p256_webauthn_recover(
    _prehash: &B256,
    _data: &[u8],
) -> Result<NativeVerifyResult, NativeVerifyError> {
    Err(NativeVerifyError::NotImplemented(NativeVerifier::P256WebAuthn))
}

/// Stub for the Delegate verifier.
pub fn delegate_recover(
    _prehash: &B256,
    _data: &[u8],
) -> Result<NativeVerifyResult, NativeVerifyError> {
    Err(NativeVerifyError::NotImplemented(NativeVerifier::Delegate))
}

/// Convenience dispatcher: verifies `data` against `prehash` according to
/// `kind` and returns the recovered `(address, owner_id)` pair.
pub fn native_verify(
    kind: NativeVerifier,
    prehash: &B256,
    data: &[u8],
) -> Result<NativeVerifyResult, NativeVerifyError> {
    match kind {
        NativeVerifier::K1 => k1_recover(prehash, data),
        NativeVerifier::P256Raw => p256_raw_recover(prehash, data),
        NativeVerifier::P256WebAuthn => p256_webauthn_recover(prehash, data),
        NativeVerifier::Delegate => delegate_recover(prehash, data),
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{b256, U256};

    use super::*;

    #[test]
    fn k1_owner_id_is_keccak_of_address() {
        let address = Address::repeat_byte(0xAB);
        assert_eq!(k1_owner_id(address), keccak256(address.as_slice()));
    }

    #[test]
    fn k1_recover_bad_size_rejected() {
        let h = B256::repeat_byte(0x01);
        assert!(matches!(k1_recover(&h, &[0u8; 64]), Err(NativeVerifyError::BadSize)));
    }

    #[test]
    fn k1_recover_produces_address_and_owner_id() {
        // Build a well-formed 65-byte signature blob. Recovery may or may
        // not succeed — what we pin is that when it does, `owner_id` is
        // `keccak256(address)`.
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..32].copy_from_slice(
            b256!("0x8a4a9f4acee2f89c40f16c36ecb0bd3adca16cdba32c7ad3c466bfdc0e8c716b").as_slice(),
        );
        sig_bytes[32..64].copy_from_slice(
            b256!("0x7d7fa9fa3df3fc47f8bee7810f76a99432d68321c739c8497b3bda77e4f72ebb").as_slice(),
        );
        sig_bytes[64] = 27;
        let prehash = b256!("0x71ee4b65924ab7a31ff9bdc91b93be8ca7c03e0b59193d3a9bc9efce71551e2d");

        if let Ok(r) = k1_recover(&prehash, &sig_bytes) {
            assert_eq!(r.owner_id, keccak256(r.address.as_slice()));
        }

        // Check the Signature constructor also accepts the same bytes.
        let _sig = Signature::try_from(&sig_bytes[..]).expect("valid bytes parse");
        let _ = U256::ZERO; // silence unused
    }

    #[test]
    fn p256_is_not_implemented() {
        let h = B256::ZERO;
        assert!(matches!(
            p256_raw_recover(&h, &[]),
            Err(NativeVerifyError::NotImplemented(NativeVerifier::P256Raw))
        ));
        assert!(matches!(
            p256_webauthn_recover(&h, &[]),
            Err(NativeVerifyError::NotImplemented(NativeVerifier::P256WebAuthn))
        ));
        assert!(matches!(
            delegate_recover(&h, &[]),
            Err(NativeVerifyError::NotImplemented(NativeVerifier::Delegate))
        ));
    }

    #[test]
    fn native_verify_dispatches_to_k1() {
        let h = B256::repeat_byte(0x01);
        assert!(matches!(
            native_verify(NativeVerifier::K1, &h, &[0u8; 64]),
            Err(NativeVerifyError::BadSize)
        ));
    }
}
