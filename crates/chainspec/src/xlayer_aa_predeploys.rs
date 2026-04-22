//! Canonical XLayerAA (EIP-8130) predeploy addresses.
//!
//! Each entry pairs a fresh, human-assigned deployer address
//! (`0x4210000000000000000000000000000000000010..0016`) with the
//! `CREATE(deployer, nonce=0)`-derived predeploy address where that
//! contract's runtime code lives after the `XLayerHardfork::XLayerAA`
//! activation block.
//!
//! # Installation model — CL upgrade deposit transactions
//!
//! Unlike Canyon's [`ensure_create2_deployer`][canyon] (an EL-side
//! irregular state transition), the 7 XLayerAA predeploys are
//! installed by **`op-node`-synthesized upgrade deposit transactions**
//! at the fork activation boundary block. This matches every OP
//! fork from Ecotone onwards (Fjord / Granite / Holocene / Isthmus /
//! Interop / Jovian). See
//! [`xlayer_aa_upgrade_transactions.go`][ops] in the `deps/optimism`
//! submodule.
//!
//! **Installation sequence.** At the activation boundary block,
//! `op-node` emits 7 [`DepositTx`][deposit_tx] in order, one per
//! predeploy. Each tx has:
//!
//! - `from = deployer_i` (a fresh `0x4210...001X` address, nonce
//!   guaranteed 0 because the account has never sent a tx).
//! - `to = nil` (triggers `CREATE`).
//! - `data = creation_code_i ++ abi.encode(constructor_args_i)`.
//!
//! For the 3 wallets / `DelegateVerifier` that take the
//! `AccountConfiguration` address as a constructor arg, the op-node
//! patch appends `abi.encode(AccountConfiguration.address)` to `data`.
//!
//! **Why addresses are chain-portable.** `CREATE(sender, nonce)` is
//! fully determined by the pair, and both are identical across
//! devnet / testnet / mainnet (same deployer constants, same
//! creation-code bytes). No reliance on Nick's factory or CREATE2
//! salting.
//!
//! # Activation timing — devnet fires at block 1, not genesis
//!
//! `op-node`'s upgrade-tx emission condition is
//! `isActive(blockTs) && !isActive(parentTs)` (the activation
//! boundary). Genesis has no parent, and the CL does not synthesize
//! transactions into genesis — so even with
//! `XLAYER_DEVNET_XLAYER_AA_TIMESTAMP = 0`, no predeploys would be
//! installed. Devnet therefore sets
//! `XLAYER_DEVNET_XLAYER_AA_TIMESTAMP = 1` and genesis timestamp =
//! 0, placing the activation boundary at block 1. Users can submit
//! AA transactions starting block 2.
//!
//! # What this table is used for in the EL
//!
//! - RPC / pool validation that must reference a canonical predeploy
//!   (most importantly `AccountConfiguration.address`, which the AA
//!   revm handler calls into to fetch owner config + spending
//!   limits).
//! - `xlayer-dev` chainspec genesis alloc — `reth --dev` has no
//!   op-node and therefore no CL to emit upgrade deposit txs, so
//!   the 7 predeploys must be present in state from block 0. The
//!   runtime bytecode embedded here (via `include_bytes!` against
//!   `contracts/eip8130/artifacts/<Name>.bin`) is copied into the
//!   chainspec's `Genesis::alloc` at construction time.
//! - `cast code`-based devnet smoke tests that compare installed
//!   runtime against expected hex.
//!
//! **Not** used for testnet/mainnet/`xlayer-devnet` production
//! deployment — those run the op-node upgrade-deposit-tx path which
//! carries `.bin-creation` payloads op-node-side, not runtime
//! bytecode EL-side.
//!
//! # Keeping the table in sync with op-node
//!
//! `op-node`'s Go file and this Rust table encode the same deployer
//! addresses + computed predeploy addresses. A mismatch makes AA
//! tx validation reference the wrong address and silently revert.
//! Both sides are guarded by:
//!
//! - A unit test in this file (`create_address_matches_deployer`)
//!   that recomputes every predeploy address from its deployer with
//!   `alloy_primitives::Address::create` and asserts equality.
//! - A mirror test in the op-node Go package that recomputes the
//!   same set with `crypto.CreateAddress` and asserts equality
//!   against the hard-coded addresses.
//!
//! [canyon]: ../../../deps/optimism/rust/alloy-op-evm/src/block/canyon.rs
//! [ops]: ../../../deps/optimism/op-node/rollup/derive/xlayer_aa_upgrade_transactions.go
//! [deposit_tx]: ../../../deps/optimism/rust/op-alloy/crates/consensus/src/transaction/deposit.rs

use alloy_primitives::{address, Address};

/// One XLayerAA predeploy — name, op-node deployer, predeploy address,
/// embedded runtime bytecode.
#[derive(Debug, Clone, Copy)]
pub struct AAPredeploy {
    /// Human-readable contract name (used in logs + error messages).
    pub name: &'static str,
    /// Fresh op-node deployer account. `nonce=0` at the activation
    /// boundary block; op-node's upgrade deposit tx uses this as `from`.
    pub deployer: Address,
    /// `CREATE(deployer, 0)`-derived predeploy address. Stable across
    /// chains as long as `deployer` + creation-code are identical.
    pub address: Address,
    /// Runtime bytecode (immutables patched against
    /// [`ACCOUNT_CONFIGURATION_ADDRESS`]). `include_bytes!`-ed from
    /// `contracts/eip8130/artifacts/<Name>.bin`, which is regenerated
    /// by `just contracts-eip8130-build`. Consumed **only** by the
    /// `xlayer-dev` chainspec to seed `--dev` mode's genesis alloc.
    /// op-node-backed chains (`xlayer-devnet` / `xlayer-testnet` /
    /// `xlayer-mainnet`) install predeploys via CL upgrade deposit
    /// txs and do not look at this field.
    pub runtime_code: &'static [u8],
}

/// Complete ordered list of XLayerAA predeploys. Order matters only
/// because dependent contracts (`DefaultAccount`, `DefaultHighRateAccount`,
/// `DelegateVerifier`) need `AccountConfiguration.address` as a
/// constructor arg — so AccountConfiguration must be installed first.
/// op-node must emit deposit txs in this exact order.
pub const AA_PREDEPLOYS: &[AAPredeploy] = &[
    AAPredeploy {
        name: "AccountConfiguration",
        deployer: address!("0x4210000000000000000000000000000000000010"),
        address: address!("0xA6A551b856B139B3292128F3b36ADa58025c4b27"),
        runtime_code: include_bytes!(
            "../../../contracts/eip8130/artifacts/AccountConfiguration.bin"
        ),
    },
    AAPredeploy {
        name: "DefaultAccount",
        deployer: address!("0x4210000000000000000000000000000000000011"),
        address: address!("0x5D82f4311f134052bb36b11BD665Ddab843ebb3D"),
        runtime_code: include_bytes!("../../../contracts/eip8130/artifacts/DefaultAccount.bin"),
    },
    AAPredeploy {
        name: "DefaultHighRateAccount",
        deployer: address!("0x4210000000000000000000000000000000000012"),
        address: address!("0x86bf4F2d426b3386a04a24fE21a0CEb34A7b806c"),
        runtime_code: include_bytes!(
            "../../../contracts/eip8130/artifacts/DefaultHighRateAccount.bin"
        ),
    },
    AAPredeploy {
        name: "K1Verifier",
        deployer: address!("0x4210000000000000000000000000000000000013"),
        address: address!("0x7F2c04d16c53f2be99aD1a86771637568B718dBf"),
        runtime_code: include_bytes!("../../../contracts/eip8130/artifacts/K1Verifier.bin"),
    },
    AAPredeploy {
        name: "P256Verifier",
        deployer: address!("0x4210000000000000000000000000000000000014"),
        address: address!("0xAfc812351BE998FB088851a79Fc68887C42D7719"),
        runtime_code: include_bytes!("../../../contracts/eip8130/artifacts/P256Verifier.bin"),
    },
    AAPredeploy {
        name: "WebAuthnVerifier",
        deployer: address!("0x4210000000000000000000000000000000000015"),
        address: address!("0x4921DCFD2541f738990767852aB925B3b9f652A2"),
        runtime_code: include_bytes!("../../../contracts/eip8130/artifacts/WebAuthnVerifier.bin"),
    },
    AAPredeploy {
        name: "DelegateVerifier",
        deployer: address!("0x4210000000000000000000000000000000000016"),
        address: address!("0xE89A62553fE775AFe77464969b2296dc1745CF85"),
        runtime_code: include_bytes!("../../../contracts/eip8130/artifacts/DelegateVerifier.bin"),
    },
];

/// Canonical `AccountConfiguration` predeploy address.
///
/// Exposed as a named constant because the AA revm handler + pool
/// validator + RPC all reference it by name (not by index into
/// [`AA_PREDEPLOYS`]). A rename of the table entry must not break
/// downstream consumers.
pub const ACCOUNT_CONFIGURATION_ADDRESS: Address =
    address!("0xA6A551b856B139B3292128F3b36ADa58025c4b27");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_address_matches_deployer() {
        // Regression: each predeploy address MUST equal
        // CREATE(deployer, nonce=0). If this fails, either the
        // deployer was changed without recomputing the address, or
        // the address was typed wrong — both would silently break
        // at chain launch (op-node would deploy the contract to one
        // address while the EL expects it at another).
        for entry in AA_PREDEPLOYS {
            let expected = entry.deployer.create(0);
            assert_eq!(
                entry.address, expected,
                "{}: address {} != CREATE({}, 0) = {}",
                entry.name, entry.address, entry.deployer, expected,
            );
        }
    }

    #[test]
    fn deployers_are_unique() {
        // Two entries sharing a deployer would compute the same
        // predeploy address, collapsing the table silently.
        for (i, a) in AA_PREDEPLOYS.iter().enumerate() {
            for b in AA_PREDEPLOYS.iter().skip(i + 1) {
                assert_ne!(
                    a.deployer, b.deployer,
                    "{} / {} share deployer {}",
                    a.name, b.name, a.deployer,
                );
            }
        }
    }

    #[test]
    fn addresses_are_unique() {
        // Defense in depth — `create_address_matches_deployer` +
        // `deployers_are_unique` already imply this, but assert
        // directly in case the CREATE derivation invariant ever
        // weakens.
        for (i, a) in AA_PREDEPLOYS.iter().enumerate() {
            for b in AA_PREDEPLOYS.iter().skip(i + 1) {
                assert_ne!(a.address, b.address, "{} / {} share address", a.name, b.name);
            }
        }
    }

    #[test]
    fn expected_predeploy_count() {
        // Catches accidental removal of a contract from the table —
        // the op-node upgrade-tx bundle depends on all 7 being
        // present and in this exact order.
        assert_eq!(AA_PREDEPLOYS.len(), 7);
    }

    #[test]
    fn every_predeploy_has_nonempty_runtime() {
        // Guard against an empty artifact slipping in (e.g. forge
        // build was skipped or extract_runtime.py failed silently).
        // Without runtime bytecode, `xlayer-dev`'s genesis alloc
        // would install code-less accounts and every AA call would
        // revert.
        for p in AA_PREDEPLOYS {
            assert!(
                !p.runtime_code.is_empty(),
                "{} runtime bytecode is empty — did `just contracts-eip8130-build` succeed?",
                p.name,
            );
        }
    }

    #[test]
    fn account_configuration_is_first_entry() {
        // Dependent contracts (DefaultAccount, DefaultHighRateAccount,
        // DelegateVerifier) take AccountConfiguration.address as a
        // constructor arg. op-node's dispatch must therefore deploy
        // AccountConfiguration first; both sides index the table by
        // position 0 for AC, so guard the ordering.
        assert_eq!(AA_PREDEPLOYS[0].name, "AccountConfiguration");
        assert_eq!(AA_PREDEPLOYS[0].address, ACCOUNT_CONFIGURATION_ADDRESS);
    }
}
