//! XLayerAA verifier routing.
//!
//! Classifies a verifier address as one of the four **native** verifiers
//! (handled by Rust without a STATICCALL) or a **custom** contract verifier
//! (handled by a STATICCALL from the handler during execution). Auth blobs
//! start with a 20-byte verifier prefix — see [`auth_verifier_kind`].

use alloy_primitives::{address, Address};

/// Native secp256k1 / ecrecover verifier sentinel — `address(1)`.
pub const K1_VERIFIER_ADDRESS: Address = address!("0x0000000000000000000000000000000000000001");

/// Native secp256r1 raw-signature verifier predeploy.
pub const P256_RAW_VERIFIER_ADDRESS: Address =
    address!("0x30A76831b27732087561372f6a1bef6Fc391d804");

/// Native secp256r1 WebAuthn verifier predeploy.
pub const P256_WEBAUTHN_VERIFIER_ADDRESS: Address =
    address!("0x30A76831b27732087561372f6a1bef6Fc391d803");

/// Native one-hop delegation verifier predeploy.
pub const DELEGATE_VERIFIER_ADDRESS: Address =
    address!("0x30A76831b27732087561372f6a1bef6Fc391d805");

/// A verifier whose logic is overridden by native Rust code rather than
/// being executed through the EVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeVerifier {
    /// secp256k1 ecrecover (EOA or 20-byte-address owner_id).
    K1,
    /// secp256r1 raw signature.
    P256Raw,
    /// secp256r1 WebAuthn envelope.
    P256WebAuthn,
    /// One-hop delegate to another owner.
    Delegate,
}

impl NativeVerifier {
    /// Every native verifier — convenient for lookup tables in tests.
    pub const ALL: [Self; 4] = [Self::K1, Self::P256Raw, Self::P256WebAuthn, Self::Delegate];

    /// Canonical on-chain address.
    pub fn address(self) -> Address {
        match self {
            Self::K1 => K1_VERIFIER_ADDRESS,
            Self::P256Raw => P256_RAW_VERIFIER_ADDRESS,
            Self::P256WebAuthn => P256_WEBAUTHN_VERIFIER_ADDRESS,
            Self::Delegate => DELEGATE_VERIFIER_ADDRESS,
        }
    }

    /// Reverse lookup — returns `Some` when `address` matches a native
    /// verifier predeploy.
    pub fn from_address(address: Address) -> Option<Self> {
        if address == K1_VERIFIER_ADDRESS {
            Some(Self::K1)
        } else if address == P256_RAW_VERIFIER_ADDRESS {
            Some(Self::P256Raw)
        } else if address == P256_WEBAUTHN_VERIFIER_ADDRESS {
            Some(Self::P256WebAuthn)
        } else if address == DELEGATE_VERIFIER_ADDRESS {
            Some(Self::Delegate)
        } else {
            None
        }
    }
}

/// Route selection for a verifier address: the auth blob is either resolved
/// by a native Rust override or delegated to an on-chain contract via
/// STATICCALL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerifierKind {
    /// Native verifier — no EVM call is needed.
    Native(NativeVerifier),
    /// Custom verifier contract — must be executed via STATICCALL at
    /// inclusion time. The inner `Address` is the contract to call.
    Custom(Address),
}

impl VerifierKind {
    /// The verifier address, regardless of routing.
    pub fn address(self) -> Address {
        match self {
            Self::Native(verifier) => verifier.address(),
            Self::Custom(address) => address,
        }
    }

    /// `true` for the native Rust path.
    pub fn is_native(self) -> bool {
        matches!(self, Self::Native(_))
    }

    /// `true` for the EVM STATICCALL path.
    pub fn is_custom(self) -> bool {
        matches!(self, Self::Custom(_))
    }
}

/// Classifies a verifier address as native or custom.
pub fn verifier_kind(address: Address) -> VerifierKind {
    match NativeVerifier::from_address(address) {
        Some(verifier) => VerifierKind::Native(verifier),
        None => VerifierKind::Custom(address),
    }
}

/// Reads the 20-byte verifier prefix from an auth blob and classifies it.
///
/// Returns `None` for blobs shorter than 20 bytes — malformed shape that
/// consumers should reject outright rather than try to interpret.
pub fn auth_verifier_kind(auth: &[u8]) -> Option<VerifierKind> {
    if auth.len() < 20 {
        None
    } else {
        Some(verifier_kind(Address::from_slice(&auth[..20])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_verifier_roundtrip() {
        for verifier in NativeVerifier::ALL {
            assert_eq!(NativeVerifier::from_address(verifier.address()), Some(verifier));
        }
    }

    #[test]
    fn custom_verifier_stays_custom() {
        let address = Address::repeat_byte(0x55);
        assert_eq!(verifier_kind(address), VerifierKind::Custom(address));
        assert!(verifier_kind(address).is_custom());
    }

    #[test]
    fn auth_verifier_kind_too_short() {
        assert!(auth_verifier_kind(&[0u8; 19]).is_none());
        assert!(auth_verifier_kind(&[]).is_none());
    }

    #[test]
    fn auth_verifier_kind_k1() {
        let mut blob = K1_VERIFIER_ADDRESS.as_slice().to_vec();
        blob.extend_from_slice(&[0u8; 65]);
        assert_eq!(auth_verifier_kind(&blob), Some(VerifierKind::Native(NativeVerifier::K1)));
    }
}
