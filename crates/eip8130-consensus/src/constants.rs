//! EIP-8130 constants and verifier gas costs.

use alloy_primitives::{Address, U256};

use super::verifier::NativeVerifier;

/// The EIP-2718 transaction type byte for AA transactions.
pub const AA_TX_TYPE_ID: u8 = 0x7B;

/// Payer signature domain separator byte.
pub const AA_PAYER_TYPE: u8 = 0x7C;

/// Base intrinsic gas cost for an AA transaction (replaces the standard 21 000).
pub const AA_BASE_COST: u64 = 15_000;

/// Size in bytes of the EVM deployment header prepended to bytecode during CREATE2.
pub const DEPLOYMENT_HEADER_SIZE: usize = 14;

/// Maximum allowed size of `sender_auth` or `payer_auth`.
pub const MAX_SIGNATURE_SIZE: usize = 2048;

// ---------------------------------------------------------------------------
// Intrinsic gas sub-components
// ---------------------------------------------------------------------------

/// Gas for a cold nonce key (new channel).
pub const NONCE_KEY_COLD_GAS: u64 = 22_100;

/// Gas for a warm nonce key (existing channel).
pub const NONCE_KEY_WARM_GAS: u64 = 5_000;

/// Base gas for a CREATE2 deployment.
pub const BYTECODE_BASE_GAS: u64 = 32_000;

/// Per-byte gas for deployed bytecode.
pub const BYTECODE_PER_BYTE_GAS: u64 = 200;

/// Gas for each applied account-change unit (SSTORE).
pub const CONFIG_CHANGE_OP_GAS: u64 = 20_000;

/// Gas for each skipped config change entry (wrong chain, SLOAD only).
pub const CONFIG_CHANGE_SKIP_GAS: u64 = 2_100;

/// Cost of a single SLOAD during auth resolution.
pub const SLOAD_GAS: u64 = 2_100;

/// Flat gas cost for EOA (ecrecover) authentication.
pub const EOA_AUTH_GAS: u64 = 6_000;

/// Maximum calls across all phases in one transaction.
pub const MAX_CALLS_PER_TX: usize = 100;

/// Maximum account-change units in one transaction.
pub const MAX_ACCOUNT_CHANGES_PER_TX: usize = 10;

/// Maximum config operations across all entries in one transaction.
pub const MAX_CONFIG_OPS_PER_TX: usize = 5;

/// Maximum gas allowed for a custom verifier STATICCALL.
pub const CUSTOM_VERIFIER_GAS_CAP: u64 = 200_000;

/// Maximum nonce key value, enabling nonce-free mode.
pub const NONCE_KEY_MAX: U256 = U256::MAX;

/// Maximum allowed expiry window (seconds) for nonce-free transactions.
pub const NONCE_FREE_MAX_EXPIRY_WINDOW: u64 = 30;

/// Capacity of the expiring-nonce circular buffer.
pub const EXPIRING_NONCE_SET_CAPACITY: u32 = 300_000;

/// Intrinsic gas for expiring-nonce (nonce-free) transactions.
pub const EXPIRING_NONCE_GAS: u64 = 14_000;

// ---------------------------------------------------------------------------
// Verifier gas cost table
// ---------------------------------------------------------------------------

/// Configurable gas costs for native signature verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifierGasCosts {
    /// secp256k1 ECDSA recovery.
    pub k1: u64,
    /// secp256r1 / P-256 raw ECDSA verification.
    pub p256_raw: u64,
    /// secp256r1 / P-256 WebAuthn assertion verification.
    pub p256_webauthn: u64,
    /// Delegate hop overhead.
    pub delegate: u64,
}

impl VerifierGasCosts {
    /// Default gas costs.
    pub const BASE_V1: Self =
        Self { k1: 6_000, p256_raw: 9_500, p256_webauthn: 15_000, delegate: 3_000 };

    /// Returns the verification gas for a native verifier.
    pub fn gas_for_native_verifier(
        &self,
        verifier: NativeVerifier,
        inner_verifier: Option<NativeVerifier>,
    ) -> u64 {
        match verifier {
            NativeVerifier::K1 => self.k1,
            NativeVerifier::P256Raw => self.p256_raw,
            NativeVerifier::P256WebAuthn => self.p256_webauthn,
            NativeVerifier::Delegate => {
                self.delegate
                    + inner_verifier
                        .map(|inner| self.gas_for_native_verifier(inner, None))
                        .unwrap_or(0)
            }
        }
    }

    /// Returns the verification gas for a given verifier address.
    pub fn gas_for_verifier(&self, verifier: Address, inner_verifier: Option<Address>) -> u64 {
        match NativeVerifier::from_address(verifier) {
            Some(native) => self.gas_for_native_verifier(
                native,
                inner_verifier.and_then(NativeVerifier::from_address),
            ),
            None => 0,
        }
    }
}

impl Default for VerifierGasCosts {
    fn default() -> Self {
        Self::BASE_V1
    }
}
