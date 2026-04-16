//! Native Rust verification for known verifier addresses.
//!
//! Provides a fast-path for mempool validation: known verifier addresses
//! (K1, P256_RAW, P256_WEBAUTHN, DELEGATE) are verified using pure-Rust
//! crypto instead of spinning up an EVM STATICCALL. Custom verifiers
//! (unrecognized addresses) fall back to the on-chain STATICCALL path.

use alloy_primitives::{Address, Bytes, B256};
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey};
use p256::ecdsa::{Signature as P256Signature, VerifyingKey as P256VerifyingKey};
use sha2::{Digest, Sha256};

use super::predeploys::DELEGATE_VERIFIER_ADDRESS;
use super::verifier::NativeVerifier;

/// Outcome of a native verification attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeVerifyResult {
    /// Verification succeeded — the returned `B256` is the resolved `ownerId`.
    Verified(B256),
    /// The verifier address is not natively supported; fall back to STATICCALL.
    Unsupported,
    /// Verification was attempted but the signature is invalid.
    Invalid(NativeVerifyError),
}

/// Errors from native signature verification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeVerifyError {
    /// K1 auth data has wrong length (expected 65 bytes).
    #[error("invalid K1 signature data (expected 65 bytes, got {0})")]
    K1BadLength(usize),
    /// K1 ecrecover failed.
    #[error("K1 recovery failed: {0}")]
    K1RecoveryFailed(String),
    /// K1 recovered address does not match the expected owner.
    #[error("K1 recovered address does not match expected owner")]
    K1AddressMismatch,
    /// P256 auth data has wrong length (expected 128 bytes).
    #[error("invalid P256 signature data (expected 128 bytes, got {0})")]
    P256BadLength(usize),
    /// P256 signature verification failed.
    #[error("P256 verification failed: {0}")]
    P256VerificationFailed(String),
    /// WebAuthn envelope is too short to contain required fields.
    #[error("WebAuthn envelope too short ({0} bytes)")]
    WebAuthnTooShort(usize),
    /// WebAuthn clientDataJSON length field overflows the envelope.
    #[error("WebAuthn clientDataJSON length ({0}) overflows envelope")]
    WebAuthnClientDataOverflow(usize),
    /// WebAuthn clientDataJSON is not valid UTF-8.
    #[error("WebAuthn clientDataJSON is not valid UTF-8")]
    WebAuthnClientDataNotUtf8,
    /// WebAuthn clientDataJSON does not contain the expected challenge.
    #[error("WebAuthn clientDataJSON missing or mismatched challenge")]
    WebAuthnChallengeMismatch,
    /// WebAuthn P256 signature does not verify.
    #[error("WebAuthn P256 signature verification failed: {0}")]
    WebAuthnSignatureInvalid(String),
    /// Delegate auth blob is too short (needs at least inner_verifier_address + inner_data).
    #[error("delegate auth too short ({0} bytes, need at least 40)")]
    DelegateTooShort(usize),
    /// Nested delegation (delegate wrapping delegate) is not allowed.
    #[error("nested delegation is not allowed")]
    DelegateNested,
    /// Delegate implicit EOA nested signature recovered a different address.
    #[error("delegate implicit EOA nested signature did not recover delegate address")]
    DelegateImplicitSignerMismatch,
}

/// Attempts native verification for a known verifier address.
///
/// - `verifier`: The verifier address from the first 20 bytes of the auth blob.
/// - `data`: The auth data after the verifier address.
/// - `hash`: The signature hash (sender or payer).
///
/// Returns [`NativeVerifyResult::Unsupported`] for unrecognized verifier
/// addresses, signaling the caller to use the EVM STATICCALL path.
pub fn try_native_verify(verifier: Address, data: &Bytes, hash: B256) -> NativeVerifyResult {
    match NativeVerifier::from_address(verifier) {
        Some(verifier) => verifier.verify(data, hash),
        None => NativeVerifyResult::Unsupported,
    }
}

impl NativeVerifier {
    fn verify(self, data: &Bytes, hash: B256) -> NativeVerifyResult {
        match self {
            Self::K1 => verify_k1(data, hash),
            Self::P256Raw => verify_p256_raw(data, hash),
            Self::P256WebAuthn => verify_webauthn(data, hash),
            Self::Delegate => verify_delegate(data, hash),
        }
    }
}

/// secp256k1 ECDSA verification with address recovery.
///
/// `data` layout: `r(32) || s(32) || v(1)` — standard 65-byte Ethereum signature.
///
/// The `ownerId` is derived as `bytes32(bytes20(ecrecover(hash, v, r, s)))`.
fn verify_k1(data: &Bytes, hash: B256) -> NativeVerifyResult {
    if data.len() != 65 {
        return NativeVerifyResult::Invalid(NativeVerifyError::K1BadLength(data.len()));
    }

    let r_s = &data[..64];
    let v_byte = data[64];

    let recovery_id = match v_byte {
        0 | 27 => 0,
        1 | 28 => 1,
        _ => {
            return NativeVerifyResult::Invalid(NativeVerifyError::K1RecoveryFailed(format!(
                "invalid v byte: {v_byte}"
            )));
        }
    };

    let signature = match K256Signature::from_slice(r_s) {
        Ok(sig) => sig,
        Err(e) => {
            return NativeVerifyResult::Invalid(NativeVerifyError::K1RecoveryFailed(e.to_string()));
        }
    };

    let recid = RecoveryId::new(recovery_id != 0, false);

    let recovered_key = match VerifyingKey::recover_from_prehash(hash.as_slice(), &signature, recid)
    {
        Ok(key) => key,
        Err(e) => {
            return NativeVerifyResult::Invalid(NativeVerifyError::K1RecoveryFailed(e.to_string()));
        }
    };

    let public_key_bytes = recovered_key.to_encoded_point(false);
    let address = address_from_pubkey(public_key_bytes.as_bytes());

    let mut owner_id = [0u8; 32];
    owner_id[..20].copy_from_slice(address.as_slice());
    NativeVerifyResult::Verified(B256::from(owner_id))
}

/// Derives an Ethereum address from an uncompressed public key (65 bytes).
fn address_from_pubkey(uncompressed: &[u8]) -> Address {
    debug_assert!(uncompressed.len() == 65 && uncompressed[0] == 0x04);
    let hash = alloy_primitives::keccak256(&uncompressed[1..]);
    Address::from_slice(&hash[12..])
}

/// secp256r1 (P-256) raw ECDSA verification.
///
/// `data` layout: `public_key(64) || r(32) || s(32)` — 128 bytes total.
///
/// The `ownerId` is `keccak256(public_key)`.
fn verify_p256_raw(data: &Bytes, hash: B256) -> NativeVerifyResult {
    if data.len() != 128 {
        return NativeVerifyResult::Invalid(NativeVerifyError::P256BadLength(data.len()));
    }

    let pubkey_bytes = &data[..64];
    let sig_bytes = &data[64..128];

    match verify_p256_signature(pubkey_bytes, hash.as_slice(), sig_bytes) {
        Ok(()) => {
            let owner_id = alloy_primitives::keccak256(pubkey_bytes);
            NativeVerifyResult::Verified(owner_id)
        }
        Err(e) => NativeVerifyResult::Invalid(NativeVerifyError::P256VerificationFailed(e)),
    }
}

/// Shared P256 signature verification: parses the key and signature,
/// then verifies against the given prehash message.
fn verify_p256_signature(
    pubkey_raw: &[u8],
    message: &[u8],
    sig_bytes: &[u8],
) -> Result<(), String> {
    let mut uncompressed = [0u8; 65];
    uncompressed[0] = 0x04;
    uncompressed[1..].copy_from_slice(pubkey_raw);

    let verifying_key =
        P256VerifyingKey::from_sec1_bytes(&uncompressed).map_err(|e| e.to_string())?;

    let signature = P256Signature::from_slice(sig_bytes).map_err(|e| e.to_string())?;

    use p256::ecdsa::signature::hazmat::PrehashVerifier;
    verifying_key.verify_prehash(message, &signature).map_err(|e| e.to_string())
}

// ── WebAuthn ───────────────────────────────────────────────────────

/// Raw P256 public key length (x || y, no 0x04 prefix).
const P256_PUBKEY_LEN: usize = 64;
/// Minimum authenticator data: 32-byte rpIdHash + 1-byte flags + 4-byte signCount.
const MIN_AUTHENTICATOR_DATA_LEN: usize = 37;
/// P256 signature: r(32) || s(32).
const P256_SIG_LEN: usize = 64;
/// clientDataJSON length prefix (big-endian u32).
const CLIENT_DATA_LEN_PREFIX: usize = 4;

/// Full WebAuthn P256 verification.
fn verify_webauthn(data: &Bytes, expected_hash: B256) -> NativeVerifyResult {
    let min_len =
        P256_PUBKEY_LEN + MIN_AUTHENTICATOR_DATA_LEN + CLIENT_DATA_LEN_PREFIX + P256_SIG_LEN;
    if data.len() < min_len {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnTooShort(data.len()));
    }

    let pubkey_bytes = &data[..P256_PUBKEY_LEN];
    let rest = &data[P256_PUBKEY_LEN..];

    let client_data_len_offset = MIN_AUTHENTICATOR_DATA_LEN;
    if rest.len() < client_data_len_offset + CLIENT_DATA_LEN_PREFIX {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnTooShort(data.len()));
    }

    let len_bytes: [u8; 4] =
        rest[client_data_len_offset..client_data_len_offset + 4].try_into().unwrap();
    let client_data_len = u32::from_be_bytes(len_bytes) as usize;

    let client_data_start = client_data_len_offset + CLIENT_DATA_LEN_PREFIX;
    let Some(client_data_end) = client_data_start.checked_add(client_data_len) else {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnClientDataOverflow(
            client_data_len,
        ));
    };
    let Some(expected_total) = client_data_end.checked_add(P256_SIG_LEN) else {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnClientDataOverflow(
            client_data_len,
        ));
    };

    if rest.len() < expected_total {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnClientDataOverflow(
            client_data_len,
        ));
    }

    let authenticator_data = &rest[..client_data_len_offset];
    let client_data_bytes = &rest[client_data_start..client_data_end];
    let sig_bytes = &rest[client_data_end..client_data_end + P256_SIG_LEN];

    let client_data_str = match core::str::from_utf8(client_data_bytes) {
        Ok(s) => s,
        Err(_) => {
            return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnClientDataNotUtf8);
        }
    };

    let expected_challenge = base64_url_encode(expected_hash.as_slice());
    if !webauthn_challenge_matches(client_data_str, expected_challenge.as_str()) {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnChallengeMismatch);
    }

    // message = sha256(authenticatorData || sha256(clientDataJSON))
    let client_data_hash = Sha256::digest(client_data_bytes);
    let mut hasher = Sha256::new();
    hasher.update(authenticator_data);
    hasher.update(client_data_hash);
    let message = hasher.finalize();

    match verify_p256_signature(pubkey_bytes, &message, sig_bytes) {
        Ok(()) => {
            let owner_id = alloy_primitives::keccak256(pubkey_bytes);
            NativeVerifyResult::Verified(owner_id)
        }
        Err(e) => NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnSignatureInvalid(e)),
    }
}

/// Returns `true` iff top-level `clientDataJSON.challenge` exactly matches the expected value.
fn webauthn_challenge_matches(client_data_json: &str, expected_challenge: &str) -> bool {
    extract_top_level_json_string_field(client_data_json, "challenge")
        .is_some_and(|challenge| challenge == expected_challenge)
}

/// Extracts a top-level object string field value from JSON.
fn extract_top_level_json_string_field<'a>(json: &'a str, field: &str) -> Option<&'a str> {
    let bytes = json.as_bytes();
    let mut i = skip_json_ws(bytes, 0);
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;

    loop {
        i = skip_json_ws(bytes, i);
        match bytes.get(i) {
            Some(b'}') => return None,
            Some(b'"') => {}
            _ => return None,
        }

        let (key_bytes, next, key_escaped) = parse_json_string(bytes, i)?;
        if key_escaped {
            return None;
        }
        i = skip_json_ws(bytes, next);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        i = skip_json_ws(bytes, i);

        let is_target = key_bytes == field.as_bytes();
        if is_target {
            let (value_bytes, _, value_escaped) = parse_json_string(bytes, i)?;
            if value_escaped {
                return None;
            }
            return core::str::from_utf8(value_bytes).ok();
        }

        i = skip_json_value(bytes, i)?;
        i = skip_json_ws(bytes, i);
        match bytes.get(i) {
            Some(b',') => {
                i += 1;
            }
            Some(b'}') => return None,
            _ => return None,
        }
    }
}

fn skip_json_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\n' | b'\r' | b'\t') {
        i += 1;
    }
    i
}

fn parse_json_string(bytes: &[u8], start: usize) -> Option<(&[u8], usize, bool)> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }

    let mut i = start + 1;
    let content_start = i;
    let mut has_escape = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                has_escape = true;
                i += 1;
                if i >= bytes.len() {
                    return None;
                }
                i += 1;
            }
            b'"' => return Some((&bytes[content_start..i], i + 1, has_escape)),
            _ => i += 1,
        }
    }
    None
}

fn skip_json_value(bytes: &[u8], start: usize) -> Option<usize> {
    let i = skip_json_ws(bytes, start);
    match bytes.get(i) {
        Some(b'"') => parse_json_string(bytes, i).map(|(_, next, _)| next),
        Some(b'{') => skip_json_object(bytes, i),
        Some(b'[') => skip_json_array(bytes, i),
        Some(_) => {
            let mut j = i;
            while j < bytes.len() {
                match bytes[j] {
                    b',' | b'}' | b']' => break,
                    _ => j += 1,
                }
            }
            Some(j)
        }
        None => None,
    }
}

fn skip_json_object(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let mut i = start + 1;
    loop {
        i = skip_json_ws(bytes, i);
        match bytes.get(i) {
            Some(b'}') => return Some(i + 1),
            Some(b'"') => {}
            _ => return None,
        }

        let (_, next, _) = parse_json_string(bytes, i)?;
        i = skip_json_ws(bytes, next);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;

        i = skip_json_value(bytes, i)?;
        i = skip_json_ws(bytes, i);
        match bytes.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => return Some(i + 1),
            _ => return None,
        }
    }
}

fn skip_json_array(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut i = start + 1;
    loop {
        i = skip_json_ws(bytes, i);
        match bytes.get(i) {
            Some(b']') => return Some(i + 1),
            Some(_) => {}
            None => return None,
        }

        i = skip_json_value(bytes, i)?;
        i = skip_json_ws(bytes, i);
        match bytes.get(i) {
            Some(b',') => i += 1,
            Some(b']') => return Some(i + 1),
            _ => return None,
        }
    }
}

/// Base64url-encodes bytes without padding (WebAuthn challenge format).
fn base64_url_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    let chunks = bytes.chunks(3);
    for chunk in chunks {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        }
    }
    out
}

// ── Delegate ───────────────────────────────────────────────────────

/// Delegate (1-hop) verification.
fn verify_delegate(data: &Bytes, hash: B256) -> NativeVerifyResult {
    if data.len() < 40 {
        return NativeVerifyResult::Invalid(NativeVerifyError::DelegateTooShort(data.len()));
    }

    let delegate = Address::from_slice(&data[..20]);
    let nested_auth = &data[20..];
    let inner_verifier = Address::from_slice(&nested_auth[..20]);

    if inner_verifier == DELEGATE_VERIFIER_ADDRESS {
        return NativeVerifyResult::Invalid(NativeVerifyError::DelegateNested);
    }

    // Keep nested implicit EOA (`verifier=0`) native by reusing K1 recovery.
    if inner_verifier == Address::ZERO {
        let inner_data = Bytes::copy_from_slice(&nested_auth[20..]);
        return match verify_k1(&inner_data, hash) {
            NativeVerifyResult::Verified(owner_id) => {
                let mut delegate_owner = [0u8; 32];
                delegate_owner[..20].copy_from_slice(delegate.as_slice());
                let delegate_owner = B256::from(delegate_owner);
                if owner_id == delegate_owner {
                    NativeVerifyResult::Verified(delegate_owner)
                } else {
                    NativeVerifyResult::Invalid(NativeVerifyError::DelegateImplicitSignerMismatch)
                }
            }
            NativeVerifyResult::Invalid(err) => NativeVerifyResult::Invalid(err),
            NativeVerifyResult::Unsupported => NativeVerifyResult::Unsupported,
        };
    }

    match NativeVerifier::from_address(inner_verifier) {
        Some(_) => {
            let inner_data = Bytes::copy_from_slice(&nested_auth[20..]);
            match try_native_verify(inner_verifier, &inner_data, hash) {
                NativeVerifyResult::Verified(_) => {
                    let mut owner_id = [0u8; 32];
                    owner_id[..20].copy_from_slice(delegate.as_slice());
                    NativeVerifyResult::Verified(B256::from(owner_id))
                }
                NativeVerifyResult::Invalid(err) => NativeVerifyResult::Invalid(err),
                NativeVerifyResult::Unsupported => NativeVerifyResult::Unsupported,
            }
        }
        None => NativeVerifyResult::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::super::predeploys::{
        DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS, P256_RAW_VERIFIER_ADDRESS,
        P256_WEBAUTHN_VERIFIER_ADDRESS,
    };
    use super::*;
    use alloy_primitives::keccak256;
    use k256::elliptic_curve::rand_core::OsRng;

    #[test]
    fn unsupported_verifier_addresses_return_unsupported() {
        let data = Bytes::from(vec![0u8; 65]);
        let hash = B256::repeat_byte(0xAA);
        assert_eq!(try_native_verify(Address::ZERO, &data, hash), NativeVerifyResult::Unsupported);
        assert_eq!(
            try_native_verify(Address::repeat_byte(0xFF), &data, hash),
            NativeVerifyResult::Unsupported,
        );
    }

    #[test]
    fn k1_bad_length_returns_invalid() {
        let data = Bytes::from(vec![0u8; 64]);
        let hash = B256::repeat_byte(0xAA);
        let result = try_native_verify(K1_VERIFIER_ADDRESS, &data, hash);
        assert!(matches!(result, NativeVerifyResult::Invalid(NativeVerifyError::K1BadLength(64))));
    }

    #[test]
    fn p256_bad_length_returns_invalid() {
        let data = Bytes::from(vec![0u8; 100]);
        let hash = B256::repeat_byte(0xBB);
        let result = try_native_verify(P256_RAW_VERIFIER_ADDRESS, &data, hash);
        assert!(matches!(
            result,
            NativeVerifyResult::Invalid(NativeVerifyError::P256BadLength(100))
        ));
    }

    #[test]
    fn k1_valid_signature_verifies() {
        use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};

        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = VerifyingKey::from(&signing_key);

        let hash = keccak256(b"test message");

        let (signature, recid) = signing_key.sign_prehash(hash.as_slice()).unwrap();
        let mut sig_bytes = Vec::with_capacity(65);
        sig_bytes.extend_from_slice(&signature.to_bytes());
        sig_bytes.push(recid.to_byte());

        let result = try_native_verify(K1_VERIFIER_ADDRESS, &Bytes::from(sig_bytes), hash);

        match &result {
            NativeVerifyResult::Verified(owner_id) => {
                let pk_bytes = verifying_key.to_encoded_point(false);
                let expected_addr = address_from_pubkey(pk_bytes.as_bytes());
                let mut expected_owner_id = [0u8; 32];
                expected_owner_id[..20].copy_from_slice(expected_addr.as_slice());
                assert_eq!(*owner_id, B256::from(expected_owner_id));
            }
            other => panic!("expected Verified, got {:?}", other),
        }
    }

    #[test]
    fn p256_valid_signature_verifies() {
        use p256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey as P256SigningKey};

        let signing_key = P256SigningKey::random(&mut OsRng);
        let verifying_key = P256VerifyingKey::from(&signing_key);

        let hash = keccak256(b"p256 test");

        let (signature, _): (P256Signature, _) = signing_key.sign_prehash(hash.as_slice()).unwrap();

        let pk_uncompressed = verifying_key.to_encoded_point(false);
        let pk_raw = &pk_uncompressed.as_bytes()[1..];

        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(pk_raw);
        data.extend_from_slice(&signature.to_bytes());

        let result = try_native_verify(P256_RAW_VERIFIER_ADDRESS, &Bytes::from(data), hash);

        match &result {
            NativeVerifyResult::Verified(owner_id) => {
                let expected = keccak256(pk_raw);
                assert_eq!(*owner_id, expected);
            }
            other => panic!("expected Verified, got {:?}", other),
        }
    }

    #[test]
    fn k1_wrong_signature_returns_valid_but_different_owner() {
        use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};

        let key_a = SigningKey::random(&mut OsRng);
        let key_b = SigningKey::random(&mut OsRng);

        let hash = keccak256(b"different keys");

        let (sig_a, recid_a) = key_a.sign_prehash(hash.as_slice()).unwrap();
        let mut sig_bytes = Vec::with_capacity(65);
        sig_bytes.extend_from_slice(&sig_a.to_bytes());
        sig_bytes.push(recid_a.to_byte());

        let result = try_native_verify(K1_VERIFIER_ADDRESS, &Bytes::from(sig_bytes), hash);
        assert!(matches!(result, NativeVerifyResult::Verified(_)));

        let pk_b = VerifyingKey::from(&key_b).to_encoded_point(false);
        let addr_b = address_from_pubkey(pk_b.as_bytes());
        let mut owner_b = [0u8; 32];
        owner_b[..20].copy_from_slice(addr_b.as_slice());

        if let NativeVerifyResult::Verified(owner_id) = result {
            assert_ne!(owner_id, B256::from(owner_b));
        }
    }

    // ── WebAuthn tests ─────────────────────────────────────────────

    fn build_webauthn_envelope(
        hash: B256,
        signing_key: &p256::ecdsa::SigningKey,
        tamper_challenge: bool,
    ) -> Bytes {
        let challenge_b64 = base64_url_encode(hash.as_slice());
        let client_data_json = if tamper_challenge {
            format!(
                r#"{{"type":"webauthn.get","challenge":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","origin":"https://example.com"}}"#,
            )
        } else {
            format!(
                r#"{{"type":"webauthn.get","challenge":"{}","origin":"https://example.com"}}"#,
                challenge_b64,
            )
        };
        build_webauthn_envelope_with_client_data_json(signing_key, client_data_json.as_bytes())
    }

    fn build_webauthn_envelope_with_client_data_json(
        signing_key: &p256::ecdsa::SigningKey,
        client_data_json: &[u8],
    ) -> Bytes {
        let pk_uncompressed = P256VerifyingKey::from(signing_key).to_encoded_point(false);
        let pk_raw = &pk_uncompressed.as_bytes()[1..];

        let authenticator_data = vec![0xAA; MIN_AUTHENTICATOR_DATA_LEN];

        let cd_len = (client_data_json.len() as u32).to_be_bytes();

        // sign: sha256(authenticatorData || sha256(clientDataJSON))
        let client_data_hash = Sha256::digest(client_data_json);
        let mut hasher = Sha256::new();
        hasher.update(&authenticator_data);
        hasher.update(client_data_hash);
        let message = hasher.finalize();

        use p256::ecdsa::signature::hazmat::PrehashSigner;
        let (sig, _): (P256Signature, _) = signing_key.sign_prehash(&message).unwrap();

        let mut envelope = Vec::new();
        envelope.extend_from_slice(pk_raw);
        envelope.extend_from_slice(&authenticator_data);
        envelope.extend_from_slice(&cd_len);
        envelope.extend_from_slice(client_data_json);
        envelope.extend_from_slice(&sig.to_bytes());
        Bytes::from(envelope)
    }

    #[test]
    fn webauthn_too_short_returns_invalid() {
        let data = Bytes::from(vec![0u8; 50]);
        let hash = B256::repeat_byte(0xCC);
        assert!(matches!(
            try_native_verify(P256_WEBAUTHN_VERIFIER_ADDRESS, &data, hash),
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnTooShort(50)),
        ));
    }

    #[test]
    fn webauthn_valid_signature_verifies() {
        use p256::ecdsa::SigningKey as P256SigningKey;

        let signing_key = P256SigningKey::random(&mut OsRng);
        let pk_uncompressed = P256VerifyingKey::from(&signing_key).to_encoded_point(false);
        let pk_raw = &pk_uncompressed.as_bytes()[1..];

        let hash = keccak256(b"webauthn test");
        let data = build_webauthn_envelope(hash, &signing_key, false);

        let result = try_native_verify(P256_WEBAUTHN_VERIFIER_ADDRESS, &data, hash);
        match &result {
            NativeVerifyResult::Verified(owner_id) => {
                let expected = keccak256(pk_raw);
                assert_eq!(*owner_id, expected);
            }
            other => panic!("expected Verified, got {:?}", other),
        }
    }

    #[test]
    fn webauthn_wrong_challenge_returns_invalid() {
        use p256::ecdsa::SigningKey as P256SigningKey;

        let signing_key = P256SigningKey::random(&mut OsRng);
        let hash = keccak256(b"webauthn test");
        let data = build_webauthn_envelope(hash, &signing_key, true);
        assert!(matches!(
            try_native_verify(P256_WEBAUTHN_VERIFIER_ADDRESS, &data, hash),
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnChallengeMismatch),
        ));
    }

    #[test]
    fn webauthn_challenge_must_match_challenge_field() {
        use p256::ecdsa::SigningKey as P256SigningKey;

        let signing_key = P256SigningKey::random(&mut OsRng);
        let hash = keccak256(b"webauthn challenge field match");
        let expected_challenge = base64_url_encode(hash.as_slice());
        let client_data_json = format!(
            r#"{{"type":"webauthn.get","challenge":"WRONG_CHALLENGE","origin":"https://example.com/{}"}}"#,
            expected_challenge,
        );
        let data = build_webauthn_envelope_with_client_data_json(
            &signing_key,
            client_data_json.as_bytes(),
        );

        assert!(matches!(
            try_native_verify(P256_WEBAUTHN_VERIFIER_ADDRESS, &data, hash),
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnChallengeMismatch),
        ));
    }

    #[test]
    fn webauthn_wrong_key_returns_invalid() {
        use p256::ecdsa::SigningKey as P256SigningKey;

        let key_a = P256SigningKey::random(&mut OsRng);
        let key_b = P256SigningKey::random(&mut OsRng);

        let hash = keccak256(b"webauthn wrong key");
        let data = build_webauthn_envelope(hash, &key_a, false);

        let pk_b = P256VerifyingKey::from(&key_b).to_encoded_point(false);
        let pk_b_raw = &pk_b.as_bytes()[1..];
        let mut tampered = pk_b_raw.to_vec();
        tampered.extend_from_slice(&data[P256_PUBKEY_LEN..]);

        let result =
            try_native_verify(P256_WEBAUTHN_VERIFIER_ADDRESS, &Bytes::from(tampered), hash);
        assert!(matches!(
            result,
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnSignatureInvalid(_)),
        ));
    }

    #[test]
    fn webauthn_overflow_client_data_returns_invalid() {
        let hash = B256::repeat_byte(0xDD);
        let mut data = vec![0u8; P256_PUBKEY_LEN + MIN_AUTHENTICATOR_DATA_LEN];
        data.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // 4GB length
        data.extend_from_slice(&[0u8; P256_SIG_LEN]);
        assert!(matches!(
            try_native_verify(P256_WEBAUTHN_VERIFIER_ADDRESS, &Bytes::from(data), hash),
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnClientDataOverflow(_)),
        ));
    }

    // ── Delegate tests ─────────────────────────────────────────────

    #[test]
    fn delegate_k1_verifies_and_returns_delegate_owner() {
        use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};

        let signing_key = SigningKey::random(&mut OsRng);
        let hash = keccak256(b"delegate k1 test");
        let (signature, recid) = signing_key.sign_prehash(hash.as_slice()).unwrap();

        let delegate = Address::repeat_byte(0x22);
        let mut data = Vec::new();
        data.extend_from_slice(delegate.as_slice());
        data.extend_from_slice(K1_VERIFIER_ADDRESS.as_slice());
        data.extend_from_slice(&signature.to_bytes());
        data.push(recid.to_byte());

        let result = try_native_verify(DELEGATE_VERIFIER_ADDRESS, &Bytes::from(data), hash);
        let mut expected_owner = [0u8; 32];
        expected_owner[..20].copy_from_slice(delegate.as_slice());
        assert_eq!(result, NativeVerifyResult::Verified(B256::from(expected_owner)));
    }

    #[test]
    fn delegate_p256_verifies_and_returns_delegate_owner() {
        use p256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey as P256SigningKey};

        let signing_key = P256SigningKey::random(&mut OsRng);
        let hash = keccak256(b"delegate p256 test");
        let (signature, _): (P256Signature, _) = signing_key.sign_prehash(hash.as_slice()).unwrap();

        let pk_uncompressed = P256VerifyingKey::from(&signing_key).to_encoded_point(false);
        let pk_raw = &pk_uncompressed.as_bytes()[1..];

        let delegate = Address::repeat_byte(0x23);
        let mut data = Vec::new();
        data.extend_from_slice(delegate.as_slice());
        data.extend_from_slice(P256_RAW_VERIFIER_ADDRESS.as_slice());
        data.extend_from_slice(pk_raw);
        data.extend_from_slice(&signature.to_bytes());

        let result = try_native_verify(DELEGATE_VERIFIER_ADDRESS, &Bytes::from(data), hash);
        let mut expected_owner = [0u8; 32];
        expected_owner[..20].copy_from_slice(delegate.as_slice());
        assert_eq!(result, NativeVerifyResult::Verified(B256::from(expected_owner)));
    }

    #[test]
    fn delegate_webauthn_verifies_and_returns_delegate_owner() {
        use p256::ecdsa::SigningKey as P256SigningKey;

        let signing_key = P256SigningKey::random(&mut OsRng);

        let hash = keccak256(b"delegate webauthn test");
        let webauthn_data = build_webauthn_envelope(hash, &signing_key, false);

        let delegate = Address::repeat_byte(0x24);
        let mut data = Vec::new();
        data.extend_from_slice(delegate.as_slice());
        data.extend_from_slice(P256_WEBAUTHN_VERIFIER_ADDRESS.as_slice());
        data.extend_from_slice(&webauthn_data);

        let result = try_native_verify(DELEGATE_VERIFIER_ADDRESS, &Bytes::from(data), hash);
        let mut expected_owner = [0u8; 32];
        expected_owner[..20].copy_from_slice(delegate.as_slice());
        assert_eq!(result, NativeVerifyResult::Verified(B256::from(expected_owner)));
    }

    #[test]
    fn delegate_implicit_eoa_verifies_and_returns_delegate_owner() {
        use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};

        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = VerifyingKey::from(&signing_key);
        let delegate = address_from_pubkey(verifying_key.to_encoded_point(false).as_bytes());

        let hash = keccak256(b"delegate implicit k1 test");
        let (signature, recid) = signing_key.sign_prehash(hash.as_slice()).unwrap();

        let mut data = Vec::new();
        data.extend_from_slice(delegate.as_slice());
        data.extend_from_slice(Address::ZERO.as_slice());
        data.extend_from_slice(&signature.to_bytes());
        data.push(recid.to_byte());

        let result = try_native_verify(DELEGATE_VERIFIER_ADDRESS, &Bytes::from(data), hash);
        let mut expected_owner = [0u8; 32];
        expected_owner[..20].copy_from_slice(delegate.as_slice());
        assert_eq!(result, NativeVerifyResult::Verified(B256::from(expected_owner)));
    }

    #[test]
    fn delegate_nested_returns_invalid() {
        let delegate = Address::repeat_byte(0x25);
        let mut data = Vec::new();
        data.extend_from_slice(delegate.as_slice());
        data.extend_from_slice(DELEGATE_VERIFIER_ADDRESS.as_slice());
        data.push(0xAA);
        let hash = B256::repeat_byte(0xEE);
        assert!(matches!(
            try_native_verify(DELEGATE_VERIFIER_ADDRESS, &Bytes::from(data), hash),
            NativeVerifyResult::Invalid(NativeVerifyError::DelegateNested),
        ));
    }

    #[test]
    fn delegate_too_short_returns_invalid() {
        let data = Bytes::from(vec![0xAA; 15]);
        let hash = B256::repeat_byte(0xFF);
        assert!(matches!(
            try_native_verify(DELEGATE_VERIFIER_ADDRESS, &data, hash),
            NativeVerifyResult::Invalid(NativeVerifyError::DelegateTooShort(15)),
        ));
    }

    #[test]
    fn delegate_custom_inner_returns_unsupported() {
        let delegate = Address::repeat_byte(0xCC);
        let arbitrary_address = Address::repeat_byte(0xDD);
        let mut data = Vec::new();
        data.extend_from_slice(delegate.as_slice());
        data.extend_from_slice(arbitrary_address.as_slice());
        data.extend_from_slice(&[0xEE; 20]);
        let hash = B256::repeat_byte(0xAA);
        assert_eq!(
            try_native_verify(DELEGATE_VERIFIER_ADDRESS, &Bytes::from(data), hash),
            NativeVerifyResult::Unsupported,
        );
    }

    #[test]
    fn base64_url_encode_known_value() {
        let input = [0xDE, 0xAD, 0xBE, 0xEF];
        let encoded = base64_url_encode(&input);
        assert_eq!(encoded, "3q2-7w");
    }
}
