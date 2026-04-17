//! Deployment bytecodes and deployer addresses for EIP-8130 system contracts.
//!
//! The bytecodes are compiled from Base's EIP-8130 Solidity contracts and
//! loaded at compile time via `include_str!()`. Each contract is deployed via
//! a deterministic `TxDeposit` from a unique deployer address; the resulting
//! on-chain address is `deployer.create(0)`.
//!
//! # Contract deployment order
//!
//! 1. K1Verifier            (no constructor args)
//! 2. P256Verifier          (no constructor args)
//! 3. WebAuthnVerifier      (no constructor args)
//! 4. AccountConfiguration  (constructor: k1, p256, webauthn, delegate=address(0))
//! 5. DelegateVerifier      (constructor: accountConfiguration)
//! 6. DefaultAccount        (constructor: accountConfiguration)
//!
//! # XLayer divergence from Base
//!
//! XLayer keeps `K1_VERIFIER_ADDRESS = 0x01` (ecrecover precompile) as the
//! routing sentinel for secp256k1 verification. The K1Verifier bytecode is
//! still embedded for completeness, but the AccountConfiguration constructor
//! receives `K1_VERIFIER_ADDRESS` (0x01) instead of the deployed K1 contract
//! address. The native verifier system intercepts K1 calls before they reach
//! the EVM.

use alloy_primitives::{address, hex, Address, Bytes};

// ── Deployer addresses ───────────────────────────────────────────────
// These are the `from` addresses for the upgrade deposit transactions.
// Each deployer has nonce 0, so `deployer.create(0)` yields the contract address.

/// Deployer for the K1 (secp256k1) Verifier contract.
pub const DEPLOYER_K1_VERIFIER: Address = address!("0x4210000000000000000000000000000000000008");

/// Deployer for the P256 raw ECDSA Verifier contract.
pub const DEPLOYER_P256_VERIFIER: Address = address!("0x4210000000000000000000000000000000000009");

/// Deployer for the WebAuthn Verifier contract.
pub const DEPLOYER_WEBAUTHN_VERIFIER: Address =
    address!("0x421000000000000000000000000000000000000a");

/// Deployer for the Account Configuration contract.
pub const DEPLOYER_ACCOUNT_CONFIGURATION: Address =
    address!("0x421000000000000000000000000000000000000b");

/// Deployer for the Delegate Verifier contract.
pub const DEPLOYER_DELEGATE_VERIFIER: Address =
    address!("0x421000000000000000000000000000000000000c");

/// Deployer for the Default Account contract.
pub const DEPLOYER_DEFAULT_ACCOUNT: Address =
    address!("0x421000000000000000000000000000000000000d");

// ── Gas limits for deployment transactions ───────────────────────────

/// Gas limit for K1Verifier deployment.
pub const GAS_LIMIT_K1_VERIFIER: u64 = 200_000;

/// Gas limit for P256Verifier deployment.
pub const GAS_LIMIT_P256_VERIFIER: u64 = 800_000;

/// Gas limit for WebAuthnVerifier deployment.
pub const GAS_LIMIT_WEBAUTHN_VERIFIER: u64 = 1_100_000;

/// Gas limit for AccountConfiguration deployment.
pub const GAS_LIMIT_ACCOUNT_CONFIGURATION: u64 = 2_000_000;

/// Gas limit for DelegateVerifier deployment.
pub const GAS_LIMIT_DELEGATE_VERIFIER: u64 = 200_000;

/// Gas limit for DefaultAccount deployment.
pub const GAS_LIMIT_DEFAULT_ACCOUNT: u64 = 500_000;

// ── Raw bytecode loading ─────────────────────────────────────────────

/// Returns the raw K1Verifier deployment bytecode (no constructor args needed).
pub fn k1_verifier_bytecode() -> Bytes {
    decode_hex(include_str!("./bytecode/base-v1-k1-verifier-deployment.hex"))
}

/// Returns the raw P256Verifier deployment bytecode (no constructor args needed).
pub fn p256_verifier_bytecode() -> Bytes {
    decode_hex(include_str!("./bytecode/base-v1-p256-verifier-deployment.hex"))
}

/// Returns the raw WebAuthnVerifier deployment bytecode (no constructor args needed).
pub fn webauthn_verifier_bytecode() -> Bytes {
    decode_hex(include_str!("./bytecode/base-v1-web-authn-verifier-deployment.hex"))
}

/// Returns the AccountConfiguration deployment bytecode with constructor args.
///
/// Constructor: `(address k1, address p256Raw, address p256WebAuthn, address delegate)`
///
/// `delegate` is set to `address(0)` to break the circular dependency with
/// DelegateVerifier (which itself needs the AccountConfiguration address).
pub fn account_configuration_bytecode(k1: Address, p256: Address, webauthn: Address) -> Bytes {
    let base =
        decode_hex_vec(include_str!("./bytecode/base-v1-account-configuration-deployment.hex"));

    let delegate = Address::ZERO;

    let mut input = base;
    input.extend_from_slice(k1.into_word().as_slice());
    input.extend_from_slice(p256.into_word().as_slice());
    input.extend_from_slice(webauthn.into_word().as_slice());
    input.extend_from_slice(delegate.into_word().as_slice());
    input.into()
}

/// Returns the DelegateVerifier deployment bytecode with constructor args.
///
/// Constructor: `(address accountConfiguration)`
pub fn delegate_verifier_bytecode(account_config: Address) -> Bytes {
    let base = decode_hex_vec(include_str!("./bytecode/base-v1-delegate-verifier-deployment.hex"));

    let mut input = base;
    input.extend_from_slice(account_config.into_word().as_slice());
    input.into()
}

/// Returns the DefaultAccount deployment bytecode with constructor args.
///
/// Constructor: `(address accountConfiguration)`
pub fn default_account_bytecode(account_config: Address) -> Bytes {
    let base = decode_hex_vec(include_str!("./bytecode/base-v1-default-account-deployment.hex"));

    let mut input = base;
    input.extend_from_slice(account_config.into_word().as_slice());
    input.into()
}

// ── Helpers ──────────────────────────────────────────────────────────

fn decode_hex(hex_str: &str) -> Bytes {
    hex::decode(hex_str.replace('\n', "")).expect("embedded hex is valid").into()
}

fn decode_hex_vec(hex_str: &str) -> Vec<u8> {
    hex::decode(hex_str.replace('\n', "")).expect("embedded hex is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predeploys::*;

    #[test]
    fn k1_verifier_bytecode_is_non_empty() {
        assert!(!k1_verifier_bytecode().is_empty());
    }

    #[test]
    fn p256_verifier_bytecode_is_non_empty() {
        assert!(!p256_verifier_bytecode().is_empty());
    }

    #[test]
    fn webauthn_verifier_bytecode_is_non_empty() {
        assert!(!webauthn_verifier_bytecode().is_empty());
    }

    #[test]
    fn account_configuration_bytecode_is_non_empty() {
        let bytecode = account_configuration_bytecode(
            K1_VERIFIER_ADDRESS,
            P256_RAW_VERIFIER_ADDRESS,
            P256_WEBAUTHN_VERIFIER_ADDRESS,
        );
        assert!(!bytecode.is_empty());
        // Constructor appends 4 address words (4 * 32 = 128 bytes).
        let raw_len = k1_verifier_bytecode().len(); // just for reference; not same contract
        assert!(bytecode.len() > raw_len, "should be larger than the smallest contract");
    }

    #[test]
    fn delegate_verifier_bytecode_is_non_empty() {
        let bytecode = delegate_verifier_bytecode(ACCOUNT_CONFIG_ADDRESS);
        assert!(!bytecode.is_empty());
    }

    #[test]
    fn default_account_bytecode_is_non_empty() {
        let bytecode = default_account_bytecode(ACCOUNT_CONFIG_ADDRESS);
        assert!(!bytecode.is_empty());
    }

    #[test]
    fn account_configuration_constructor_args_are_appended() {
        let raw =
            decode_hex_vec(include_str!("./bytecode/base-v1-account-configuration-deployment.hex"));
        let with_args = account_configuration_bytecode(
            K1_VERIFIER_ADDRESS,
            P256_RAW_VERIFIER_ADDRESS,
            P256_WEBAUTHN_VERIFIER_ADDRESS,
        );
        // 4 address args × 32 bytes each = 128 bytes appended.
        assert_eq!(with_args.len(), raw.len() + 128);
    }

    #[test]
    fn delegate_verifier_constructor_arg_is_appended() {
        let raw =
            decode_hex_vec(include_str!("./bytecode/base-v1-delegate-verifier-deployment.hex"));
        let with_args = delegate_verifier_bytecode(ACCOUNT_CONFIG_ADDRESS);
        // 1 address arg × 32 bytes.
        assert_eq!(with_args.len(), raw.len() + 32);
    }

    #[test]
    fn default_account_constructor_arg_is_appended() {
        let raw = decode_hex_vec(include_str!("./bytecode/base-v1-default-account-deployment.hex"));
        let with_args = default_account_bytecode(ACCOUNT_CONFIG_ADDRESS);
        // 1 address arg × 32 bytes.
        assert_eq!(with_args.len(), raw.len() + 32);
    }

    #[test]
    fn deployer_addresses_are_sequential() {
        let deployers = [
            DEPLOYER_K1_VERIFIER,
            DEPLOYER_P256_VERIFIER,
            DEPLOYER_WEBAUTHN_VERIFIER,
            DEPLOYER_ACCOUNT_CONFIGURATION,
            DEPLOYER_DELEGATE_VERIFIER,
            DEPLOYER_DEFAULT_ACCOUNT,
        ];
        for (i, deployer) in deployers.iter().enumerate() {
            let expected_suffix = (8 + i) as u8;
            assert_eq!(
                deployer.as_slice()[19],
                expected_suffix,
                "deployer {i} should end with 0x{expected_suffix:02x}"
            );
        }
    }
}
