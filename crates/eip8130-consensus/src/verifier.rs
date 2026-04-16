//! Shared metadata for EIP-8130 verifier routing.

use alloy_primitives::Address;

use super::predeploys::{
    DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS, P256_RAW_VERIFIER_ADDRESS,
    P256_WEBAUTHN_VERIFIER_ADDRESS,
};

/// A verifier whose logic can be overridden by native Rust code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeVerifier {
    /// Native secp256k1 / ecrecover verifier.
    K1,
    /// Native secp256r1 raw signature verifier.
    P256Raw,
    /// Native secp256r1 WebAuthn verifier.
    P256WebAuthn,
    /// Native one-hop delegation verifier.
    Delegate,
}

impl NativeVerifier {
    /// All native verifiers supported by the node.
    pub const ALL: [Self; 4] = [Self::K1, Self::P256Raw, Self::P256WebAuthn, Self::Delegate];

    /// Returns the verifier's canonical on-chain address.
    pub fn address(self) -> Address {
        match self {
            Self::K1 => K1_VERIFIER_ADDRESS,
            Self::P256Raw => P256_RAW_VERIFIER_ADDRESS,
            Self::P256WebAuthn => P256_WEBAUTHN_VERIFIER_ADDRESS,
            Self::Delegate => DELEGATE_VERIFIER_ADDRESS,
        }
    }

    /// Returns the native verifier for an address, if one exists.
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

/// Route selection for a verifier address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerifierKind {
    /// Native Rust override is available for this verifier.
    Native(NativeVerifier),
    /// This verifier must be executed via the EVM contract path.
    Custom(Address),
}

impl VerifierKind {
    /// Returns the verifier address.
    pub fn address(self) -> Address {
        match self {
            Self::Native(verifier) => verifier.address(),
            Self::Custom(address) => address,
        }
    }

    /// Returns `true` if this verifier is handled natively.
    pub fn is_native(self) -> bool {
        matches!(self, Self::Native(_))
    }

    /// Returns `true` if this verifier must go through the EVM path.
    pub fn is_custom(self) -> bool {
        matches!(self, Self::Custom(_))
    }
}

/// Resolves a verifier address to either the native or custom execution path.
pub fn verifier_kind(address: Address) -> VerifierKind {
    match NativeVerifier::from_address(address) {
        Some(verifier) => VerifierKind::Native(verifier),
        None => VerifierKind::Custom(address),
    }
}

/// Resolves the verifier encoded in an auth blob (`verifier(20) || data...`).
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
    }
}
