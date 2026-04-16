//! Addresses for EIP-8130 system contracts and precompiles.
//!
//! # Deployment model
//!
//! **Precompiles** (native code, no EVM bytecode, fixed addresses):
//!   - `NonceManager` (`0x…aa02`)  — 2D nonce reads
//!   - `TxContext`     (`0x…aa03`)  — AA transaction metadata
//!
//! **Deployed contracts** (Solidity, deployed at NativeAA activation via
//! `TxDeposit` upgrade transactions):
//!   - `AccountConfiguration` — owner registrations, account creation, locks
//!   - `P256Verifier`, `WebAuthnVerifier`, `DelegateVerifier`
//!   - `DefaultAccount` — wallet implementation for EIP-7702 auto-delegation
//!
//! All deployed contract addresses are deterministic via CREATE2.
//!
//! > **TODO**: Replace placeholder addresses with XLayer-specific deployment
//! > addresses once system contracts are compiled and deployer addresses finalized.

use alloy_primitives::{address, Address};
use core::sync::atomic::{AtomicBool, Ordering};

use super::verifier::NativeVerifier;

/// Sentinel verifier address written on self-ownerId revocation.
///
/// When the implicit EOA owner (`ownerId == bytes32(bytes20(account))`) is
/// revoked, the contract writes
/// `OwnerConfig{verifier: address(type(uint160).max), scopes: 0}`
/// instead of deleting the slot. This prevents the protocol's implicit EOA
/// rule from re-authorizing the account on an empty slot.
pub const REVOKED_VERIFIER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

// ── AccountConfiguration deployment cache ─────────────────────────
static ACCOUNT_CONFIG_DEPLOYED: AtomicBool = AtomicBool::new(false);

/// Returns `true` if AccountConfiguration has been detected as deployed.
pub fn is_account_config_known_deployed() -> bool {
    ACCOUNT_CONFIG_DEPLOYED.load(Ordering::Relaxed)
}

/// Records that AccountConfiguration has real bytecode.
pub fn mark_account_config_deployed() {
    ACCOUNT_CONFIG_DEPLOYED.store(true, Ordering::Relaxed);
}

/// Resets the deployment flag. Test-only — allows tests to run in isolation.
#[cfg(test)]
pub fn reset_account_config_deployed() {
    ACCOUNT_CONFIG_DEPLOYED.store(false, Ordering::Relaxed);
}

// ── Precompiles (native, fixed addresses) ─────────────────────────

/// Nonce Manager precompile.
pub const NONCE_MANAGER_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa02");

/// Transaction context precompile.
pub const TX_CONTEXT_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa03");

// ── Deployed contracts ────────────────────────────────────────────
// TODO: Replace with XLayer-specific deployment addresses.
// Currently using Base's deterministic CREATE2 addresses as dev placeholders.

/// Default account (wallet) implementation contract.
pub const DEFAULT_ACCOUNT_ADDRESS: Address = address!("0x31914Dd8C3901448D787b2097744Bf7D3241E85A");

/// Account configuration system contract.
pub const ACCOUNT_CONFIG_ADDRESS: Address = address!("0x4F20618Cf5c160e7AA385268721dA968F86F0e61");

/// Explicit native K1/ecrecover verifier sentinel.
pub const K1_VERIFIER_ADDRESS: Address = address!("0x0000000000000000000000000000000000000001");

/// P256 raw ECDSA verifier contract.
pub const P256_RAW_VERIFIER_ADDRESS: Address =
    address!("0x75E9779603e826f2D8d4dD7Edee3F0a737e4228d");

/// P256 WebAuthn verifier contract.
pub const P256_WEBAUTHN_VERIFIER_ADDRESS: Address =
    address!("0xb2c8b7ec119882fBcc32FDe1be1341e19a5Bd53E");

/// Delegate verifier contract (1-hop delegation).
pub const DELEGATE_VERIFIER_ADDRESS: Address =
    address!("0x30A76831b27732087561372f6a1bef6Fc391d805");

/// Default high-rate account variant.
pub const DEFAULT_HIGH_RATE_ACCOUNT_ADDRESS: Address =
    address!("0x42Ebc02d3D7aaff19226D96F83C376B304BD25Cf");

/// Sentinel verifier address for external caller authorization.
pub const EXTERNAL_CALLER_VERIFIER: Address =
    address!("0x345249274ee98994abbf79ef955319e4cb3f6849");

/// Returns `true` if the given address is a known native verifier.
pub fn is_native_verifier(addr: Address) -> bool {
    NativeVerifier::from_address(addr).is_some()
}
