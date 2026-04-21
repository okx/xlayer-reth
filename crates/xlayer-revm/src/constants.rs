//! XLayerAA (EIP-8130) constants.
use core::sync::atomic::{AtomicU64, Ordering};

use revm::primitives::{address, Address};

/// XLayerAA transaction type byte (originally EIP-8130 `0x7B`).
pub const XLAYERAA_TX_TYPE: u8 = 0x7B;

// ---------------------------------------------------------------------------
// Owner scope bitmask
// ---------------------------------------------------------------------------

/// Owner scope bit: allowed to sign as the sender.
pub const OWNER_SCOPE_SENDER: u8 = 0x02;

/// Owner scope bit: allowed to sign as the payer.
pub const OWNER_SCOPE_PAYER: u8 = 0x04;

/// Owner scope bit: allowed to authorize config changes.
pub const OWNER_SCOPE_CONFIG: u8 = 0x08;

// ---------------------------------------------------------------------------
// Per-tx capacity limits (defense-in-depth at inclusion time)
// ---------------------------------------------------------------------------

/// Maximum number of calls across all XLayerAA phases.
pub const MAX_CALLS_PER_TX: usize = 100;

/// Maximum number of account-change units in one XLayerAA transaction.
pub const MAX_ACCOUNT_CHANGES_PER_TX: usize = 10;

// ---------------------------------------------------------------------------
// Verifier addresses
// ---------------------------------------------------------------------------

/// Delegate verifier contract address (1-hop delegation).
pub const DELEGATE_VERIFIER_ADDRESS: Address =
    address!("0x30A76831b27732087561372f6a1bef6Fc391d805");

// ---------------------------------------------------------------------------
// Custom verifier gas cap (runtime-configurable)
// ---------------------------------------------------------------------------

/// Default cap for aggregate gas spent across custom verifier STATICCALLs.
pub const DEFAULT_CUSTOM_VERIFIER_GAS_CAP: u64 = 200_000;

static CUSTOM_VERIFIER_GAS_CAP: AtomicU64 = AtomicU64::new(DEFAULT_CUSTOM_VERIFIER_GAS_CAP);

/// Returns the configured aggregate custom verifier STATICCALL gas cap.
pub fn custom_verifier_gas_cap() -> u64 {
    CUSTOM_VERIFIER_GAS_CAP.load(Ordering::Relaxed)
}

/// Sets the aggregate custom verifier STATICCALL gas cap.
pub fn set_custom_verifier_gas_cap(gas_cap: u64) {
    CUSTOM_VERIFIER_GAS_CAP.store(gas_cap, Ordering::Relaxed);
}
