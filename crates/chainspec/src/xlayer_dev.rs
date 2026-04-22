//! XLayer local-dev chain specification.
//!
//! Purpose: give `reth --dev --chain xlayer-dev` a working AA-enabled
//! L2 for local development. Distinct from [`crate::XLAYER_DEVNET`]:
//!
//! | aspect            | `xlayer-dev` (this)            | `xlayer-devnet`              |
//! | ----------------- | ------------------------------ | ---------------------------- |
//! | purpose           | `reth --dev` auto-mining       | shared regenesis L2 snapshot |
//! | genesis block     | 0 (real genesis)               | 18_696_116 (snapshot point)  |
//! | state at block 0  | 7 predeploys + 1 funded dev    | ~1.87M accounts from prior   |
//! |                   | account, computed on the fly   | chain, pre-hashed state root |
//! | consensus driver  | reth dev auto-miner (no CL)    | op-node (CL deposit txs)     |
//! | XLayerAA install  | genesis alloc (this file)      | CL upgrade deposit tx        |
//! | XLayerAA ts       | `0` (active from genesis)      | `1` (first post-genesis blk) |
//!
//! The two chainspecs intentionally do not share a chain id —
//! `xlayer-dev` uses `2195`, `xlayer-devnet` uses `195` — so a user
//! switching between them can't accidentally mix state from one into
//! the other.

use crate::{AA_PREDEPLOYS, XLAYER_DEV_HARDFORKS};
use alloy_chains::Chain;
use alloy_genesis::GenesisAccount;
use alloy_primitives::{address, Bytes, U256};
use once_cell::sync::Lazy;
use reth_chainspec::{BaseFeeParams, BaseFeeParamsKind, ChainSpec};
use reth_optimism_chainspec::{make_op_genesis_header, OpChainSpec};
use reth_primitives_traits::SealedHeader;
use std::sync::Arc;

/// Chain id for the local-dev chainspec.
///
/// Picked to be adjacent to `xlayer-devnet` (195) / `xlayer-testnet`
/// (1952) / `xlayer-mainnet` (196) but not colliding with any of them.
pub const XLAYER_DEV_CHAIN_ID: u64 = 2195;

/// Pre-funded developer account bundled into the `xlayer-dev` genesis
/// alloc so `reth --dev` can immediately send AA transactions without
/// a separate funding step.
///
/// This is the public address of reth's deterministic dev key
/// (private key `0x...aaaa...`), matching what `reth node --dev`
/// uses upstream for its auto-funded sender.
const DEV_ACCOUNT: alloy_primitives::Address =
    address!("0x14dC79964da2C08b23698B3D3cc7Ca32193d9955");

/// Initial balance seeded to [`DEV_ACCOUNT`] (10 million XLayer ETH,
/// in wei). Large enough to cover any reasonable local test run;
/// deliberately larger than `U256::MAX / 2` would be, to avoid
/// overflow in calldata-heavy scenarios.
const DEV_ACCOUNT_BALANCE_WEI: u128 = 10_000_000 * 10u128.pow(18);

/// EIP-1559 params for `xlayer-dev` — same values as the production
/// XLayer chains to keep fee semantics identical during dev testing.
const XLAYER_DEV_BASE_FEE_DENOMINATOR: u128 = 100_000_000;
const XLAYER_DEV_BASE_FEE_ELASTICITY: u128 = 1;
const XLAYER_DEV_BASE_FEE_PARAMS: BaseFeeParams =
    BaseFeeParams::new(XLAYER_DEV_BASE_FEE_DENOMINATOR, XLAYER_DEV_BASE_FEE_ELASTICITY);

/// The XLayer local-dev chainspec.
///
/// # Genesis alloc construction
///
/// We start from the JSON template (empty alloc, block 0 header
/// fields) and inject two kinds of accounts before computing the
/// state root:
///
/// 1. The 7 XLayerAA predeploys from [`AA_PREDEPLOYS`]. Each entry's
///    [`AAPredeploy::runtime_code`][crate::AAPredeploy#structfield.runtime_code]
///    is wrapped into a [`GenesisAccount`] at the entry's `address`.
///    Immutables in the runtime are already patched (see
///    `contracts/eip8130/script/extract_runtime.py`), so the
///    predeploys work correctly without running any constructor.
/// 2. The developer account at [`DEV_ACCOUNT`] with
///    [`DEV_ACCOUNT_BALANCE_WEI`] balance — parallels what `reth
///    --dev` does with its built-in dev chainspec.
///
/// Because the JSON's alloc is populated at construction time,
/// [`make_op_genesis_header`] computes a state root that correctly
/// commits to these 8 accounts. No pre-computed state root file
/// needed (unlike `xlayer-devnet`).
pub static XLAYER_DEV: Lazy<Arc<OpChainSpec>> = Lazy::new(|| {
    let mut genesis: alloy_genesis::Genesis =
        serde_json::from_str(include_str!("../res/genesis/xlayer-dev.json"))
            .expect("Can't deserialize X Layer dev genesis json");

    // Seed the 7 XLayerAA predeploys.
    for p in AA_PREDEPLOYS {
        genesis.alloc.insert(
            p.address,
            GenesisAccount {
                balance: U256::ZERO,
                nonce: Some(0),
                code: Some(Bytes::from_static(p.runtime_code)),
                storage: None,
                private_key: None,
            },
        );
    }

    // Seed the pre-funded dev account.
    genesis.alloc.insert(
        DEV_ACCOUNT,
        GenesisAccount {
            balance: U256::from(DEV_ACCOUNT_BALANCE_WEI),
            nonce: Some(0),
            code: None,
            storage: None,
            private_key: None,
        },
    );

    let hardforks = XLAYER_DEV_HARDFORKS.clone();
    let genesis_header = make_op_genesis_header(&genesis, &hardforks);
    let genesis_header = SealedHeader::seal_slow(genesis_header);

    OpChainSpec {
        inner: ChainSpec {
            chain: Chain::from_id(XLAYER_DEV_CHAIN_ID),
            genesis_header,
            genesis,
            paris_block_and_final_difficulty: Some((0, U256::ZERO)),
            hardforks,
            base_fee_params: BaseFeeParamsKind::Constant(XLAYER_DEV_BASE_FEE_PARAMS),
            ..Default::default()
        },
    }
    .into()
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{XLayerHardforks, ACCOUNT_CONFIGURATION_ADDRESS};

    #[test]
    fn chain_id_is_2195() {
        assert_eq!(XLAYER_DEV.chain().id(), XLAYER_DEV_CHAIN_ID);
    }

    #[test]
    fn genesis_is_block_zero() {
        // `xlayer-dev` must start at a real block 0 so the reth
        // dev-miner can produce block 1 onwards. If this flips to
        // a non-zero number (e.g. via a stray `legacyXLayerBlock`
        // field sneaking into the JSON), auto-mining breaks.
        assert_eq!(XLAYER_DEV.genesis_header().number, 0);
    }

    #[test]
    fn genesis_alloc_has_seven_predeploys_plus_dev_account() {
        let alloc = &XLAYER_DEV.genesis().alloc;
        assert_eq!(
            alloc.len(),
            AA_PREDEPLOYS.len() + 1,
            "expected {} predeploys + 1 dev account",
            AA_PREDEPLOYS.len(),
        );
        for p in AA_PREDEPLOYS {
            let entry = alloc.get(&p.address).unwrap_or_else(|| {
                panic!("predeploy {} missing from alloc at {}", p.name, p.address)
            });
            let code = entry
                .code
                .as_ref()
                .unwrap_or_else(|| panic!("predeploy {} has no code in alloc", p.name));
            assert_eq!(
                code.as_ref(),
                p.runtime_code,
                "{} runtime bytecode in alloc does not match AA_PREDEPLOYS table",
                p.name,
            );
        }
        assert!(alloc.contains_key(&DEV_ACCOUNT), "dev-funded account missing from alloc",);
    }

    #[test]
    fn account_configuration_is_at_canonical_address() {
        // Downstream consumers (AA revm handler, pool validator,
        // RPC fillers) hard-reference `ACCOUNT_CONFIGURATION_ADDRESS`.
        // A drift between the constant and what xlayer-dev actually
        // installs would silently break every AA call path.
        let alloc = &XLAYER_DEV.genesis().alloc;
        assert!(
            alloc.contains_key(&ACCOUNT_CONFIGURATION_ADDRESS),
            "AccountConfiguration missing at canonical address {}",
            ACCOUNT_CONFIGURATION_ADDRESS,
        );
    }

    #[test]
    fn xlayer_aa_active_at_genesis() {
        // Contrast with `xlayer-devnet` (ts=1). `--dev` has no CL
        // to emit upgrade txs at the boundary block, so XLayerAA
        // must be "on" from block 0 for tx validation to accept
        // `0x7B`-typed transactions starting at block 1.
        assert!(XLAYER_DEV.is_xlayer_aa_active_at_timestamp(0));
    }
}
