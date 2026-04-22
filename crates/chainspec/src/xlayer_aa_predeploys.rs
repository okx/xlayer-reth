//! Canonical XLayerAA (EIP-8130) predeploy table.
//!
//! Each entry pairs a canonical CREATE2(salt=0, creationCode) address
//! with the runtime bytecode that should be installed at that address
//! when `XLayerHardfork::XLayerAA` activates. Runtime bytes are read
//! via `include_bytes!` from [`contracts/eip8130/artifacts/<Name>.bin`]
//! — the raw-binary sibling of the committed hex `.bin-runtime` files
//! (both produced by `just contracts-eip8130-build`).
//!
//! # ⚠ Activation hook — deferred (M3 C4)
//!
//! This table is **declarative only**. Nothing in this commit installs
//! the bytecode into on-chain state at the activation block. The
//! installer that consumes `AA_PREDEPLOYS` is still TODO:
//!
//! **Intended shape**: mirror
//! [`alloy_op_evm::block::canyon::ensure_create2_deployer`] — a
//! pre-execution hook on `OpBlockExecutor::apply_pre_execution_changes`
//! that, at the first block where `is_xlayer_aa_active_at_timestamp`
//! flips to true, writes each predeploy's bytecode + codehash to
//! state.
//!
//! **Blocker**: the natural injection point requires either
//! (a) extending `OpHardforks` with a `named_fork_activation(&str) ->
//! ForkCondition` method so `alloy-op-evm` can detect XLayerAA
//! activation without pulling `xlayer-chainspec` upward, or
//! (b) a local block-executor wrapper inside xlayer-reth.
//!
//! Approach (a) was attempted in a submodule patch but triggered a
//! rustc ICE during `cargo test` build of `reth-optimism-node`
//! (spurious `Unpin` trait-obligation failure on
//! `BasicPayloadJob<..., OpEvmConfig, ()>`). Approach (b) requires a
//! non-trivial `BlockExecutorFactory` wrapper. Both are tracked for a
//! dedicated C4 follow-up.
//!
//! **Interim impact**: devnet's XLayerAA timestamp is `0` (genesis),
//! so without the installer the 7 addresses have empty code at block
//! zero. AA transactions that depend on `AccountConfiguration` or the
//! verifier contracts will revert during execution. The chainspec
//! fork entry is still in place, so
//! `is_xlayer_aa_active_at_timestamp(_)` returns `true` as expected —
//! only the on-chain bytecode is missing. Testnet / mainnet remain at
//! `XLAYER_AA_TIMESTAMP_TBD = u64::MAX`, so they are unaffected.
//!
//! **Addresses.** Derived from the vendored Solidity sources via
//! `forge script script/Deploy.s.sol:Deploy --sig 'addresses()'` —
//! deterministic functions of the runtime bytecode and the Nick's
//! factory salt (`0x00..`). Any change to a `.sol` file, compiler
//! setting in `contracts/eip8130/foundry.toml`, or pinned `lib/*`
//! submodule commit shifts both the bytes and the address; after
//! such a change, rerun `just contracts-eip8130-build` and re-paste
//! the addresses here.
//!
//! **Mirrored in.** [`contracts/eip8130/script/Deploy.s.sol`] keeps the
//! same addresses in Solidity form for `cast code`-based devnet smoke
//! tests.

use alloy_primitives::{address, Address};

/// One XLayerAA predeploy — canonical address + runtime bytecode.
#[derive(Debug, Clone, Copy)]
pub struct AAPredeploy {
    /// Human-readable contract name (for logs + error messages).
    pub name: &'static str,
    /// Canonical CREATE2 address. Must match
    /// `forge script ... addresses()` output for the vendored source.
    pub address: Address,
    /// Runtime bytecode to install at `address`. Read directly from
    /// the raw-binary `.bin` file committed alongside the hex
    /// `.bin-runtime`.
    pub code: &'static [u8],
}

/// Complete ordered list of XLayerAA predeploys installed at
/// activation. The order is reporting-friendly (core first, then
/// wallets, then verifiers); installation logic must be idempotent
/// and order-independent.
pub const AA_PREDEPLOYS: &[AAPredeploy] = &[
    AAPredeploy {
        name: "AccountConfiguration",
        address: address!("0x3621Acf9Fb8700777b69b97a648fC11944998FEe"),
        code: include_bytes!("../../../contracts/eip8130/artifacts/AccountConfiguration.bin"),
    },
    AAPredeploy {
        name: "DefaultAccount",
        address: address!("0xa0Bf4bc7a9f6330f8D736728e8d427d67f76f347"),
        code: include_bytes!("../../../contracts/eip8130/artifacts/DefaultAccount.bin"),
    },
    AAPredeploy {
        name: "DefaultHighRateAccount",
        address: address!("0x029B4de36B94B245Bf6565f32dECb7DD41bb176f"),
        code: include_bytes!("../../../contracts/eip8130/artifacts/DefaultHighRateAccount.bin"),
    },
    AAPredeploy {
        name: "K1Verifier",
        address: address!("0xE15687b69F03B8D8d9CD66957efF78A12E7f3590"),
        code: include_bytes!("../../../contracts/eip8130/artifacts/K1Verifier.bin"),
    },
    AAPredeploy {
        name: "P256Verifier",
        address: address!("0x6Aa9e9159bf94c6e8a41B0bd8D9518336272369a"),
        code: include_bytes!("../../../contracts/eip8130/artifacts/P256Verifier.bin"),
    },
    AAPredeploy {
        name: "WebAuthnVerifier",
        address: address!("0xDD4fd5645360a8E885aA27F109CA097062eD3BE9"),
        code: include_bytes!("../../../contracts/eip8130/artifacts/WebAuthnVerifier.bin"),
    },
    AAPredeploy {
        name: "DelegateVerifier",
        address: address!("0xf8ed43eCADe91D382E8D36363721B5e30b2747e7"),
        code: include_bytes!("../../../contracts/eip8130/artifacts/DelegateVerifier.bin"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_predeploy_has_nonempty_runtime() {
        // Guard against an accidentally-empty artifact — would silently
        // ship a no-code predeploy at genesis.
        for predeploy in AA_PREDEPLOYS {
            assert!(
                !predeploy.code.is_empty(),
                "{} runtime bytecode is empty — did `forge build` succeed?",
                predeploy.name,
            );
        }
    }

    #[test]
    fn predeploy_addresses_are_unique() {
        // Two predeploys colliding on one address would silently
        // overwrite each other at install time.
        for (i, a) in AA_PREDEPLOYS.iter().enumerate() {
            for b in AA_PREDEPLOYS.iter().skip(i + 1) {
                assert_ne!(a.address, b.address, "{} / {} share address", a.name, b.name);
            }
        }
    }

    #[test]
    fn expected_predeploy_count() {
        // Catches accidental removal of a contract from the table —
        // the activation hook (C4) depends on all 7 being present.
        assert_eq!(AA_PREDEPLOYS.len(), 7);
    }
}
