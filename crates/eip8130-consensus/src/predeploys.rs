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
//! All deployed contract addresses are deterministic: each contract is
//! deployed by a unique deployer via `deployer.create(0)`, yielding a
//! fixed address. The deployer addresses are defined in
//! [`super::system_bytecodes`].

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
// Addresses are deterministic: `deployer.create(0)`.
// Deployer addresses are in `system_bytecodes.rs`.

/// Default account (wallet) implementation contract.
/// Deployed from `DEPLOYER_DEFAULT_ACCOUNT` (`0x4210…000d`).
pub const DEFAULT_ACCOUNT_ADDRESS: Address = address!("0xAb4eE49EE97e49807e180BD5Fb9D9F35783b84F2");

/// Account configuration system contract.
/// Deployed from `DEPLOYER_ACCOUNT_CONFIGURATION` (`0x4210…000b`).
pub const ACCOUNT_CONFIG_ADDRESS: Address = address!("0xf946601D5424118A4e4054BB0B13133f216b4FeE");

/// Explicit native K1/ecrecover verifier sentinel.
///
/// XLayer uses the ecrecover precompile address (`0x01`) as the K1 verifier
/// routing key. The native verifier system intercepts this address and runs
/// secp256k1 verification in Rust. This diverges from Base, which deploys
/// a separate K1Verifier contract.
pub const K1_VERIFIER_ADDRESS: Address = address!("0x0000000000000000000000000000000000000001");

/// P256 raw ECDSA verifier contract.
/// Deployed from `DEPLOYER_P256_VERIFIER` (`0x4210…0009`).
pub const P256_RAW_VERIFIER_ADDRESS: Address =
    address!("0x6751c7ED0C58319e75437f8E6Dafa2d7F6b8306F");

/// P256 WebAuthn verifier contract.
/// Deployed from `DEPLOYER_WEBAUTHN_VERIFIER` (`0x4210…000a`).
pub const P256_WEBAUTHN_VERIFIER_ADDRESS: Address =
    address!("0x3572bb3F611a40DDcA70e5b55Cc797D58357AD44");

/// Delegate verifier contract (1-hop delegation).
/// Deployed from `DEPLOYER_DELEGATE_VERIFIER` (`0x4210…000c`).
pub const DELEGATE_VERIFIER_ADDRESS: Address =
    address!("0xc758A89C53542164aaB7f6439e8c8cAcf628fF62");

/// Default high-rate account variant.
// TODO: Compute from a dedicated deployer once the Solidity source is finalized.
pub const DEFAULT_HIGH_RATE_ACCOUNT_ADDRESS: Address =
    address!("0x42Ebc02d3D7aaff19226D96F83C376B304BD25Cf");

/// Sentinel verifier address for external caller authorization.
pub const EXTERNAL_CALLER_VERIFIER: Address =
    address!("0x345249274ee98994abbf79ef955319e4cb3f6849");

/// Returns `true` if the given address is a known native verifier.
pub fn is_native_verifier(addr: Address) -> bool {
    NativeVerifier::from_address(addr).is_some()
}

/// All system contract addresses that must have non-empty bytecode
/// after the NativeAA upgrade deposits execute.
///
/// Does **not** include precompile addresses (NonceManager, TxContext, K1)
/// since those are handled by native code, not deployed bytecode.
pub const DEPLOYED_SYSTEM_CONTRACT_ADDRESSES: [Address; 6] = [
    ACCOUNT_CONFIG_ADDRESS,
    DEFAULT_ACCOUNT_ADDRESS,
    DEFAULT_HIGH_RATE_ACCOUNT_ADDRESS,
    P256_RAW_VERIFIER_ADDRESS,
    P256_WEBAUTHN_VERIFIER_ADDRESS,
    DELEGATE_VERIFIER_ADDRESS,
];
