//! XLayer Devnet chain specification

use alloy_chains::Chain;
use alloy_primitives::{b256, B256, U256};
use once_cell::sync::Lazy;
use reth_chainspec::{BaseFeeParams, BaseFeeParamsKind, ChainSpec, Hardfork};
use reth_ethereum_forks::EthereumHardfork;
use reth_optimism_chainspec::{make_op_genesis_header, OpChainSpec};
use reth_optimism_forks::OpHardfork;
use reth_primitives_traits::SealedHeader;
use std::sync::Arc;
use xlayer_legacy_migrate::XLAYER_DEVNET_HARDFORKS;

/// X Layer Devnet genesis hash
///
/// Computed from the genesis block header.
pub(crate) static XLAYER_DEVNET_GENESIS_HASH: Lazy<B256> = Lazy::new(|| {
    include_str!("../../../crates/chainspec/res/genesis/xlayer-devnet-genesis-hash.txt")
        .trim()
        .parse::<B256>()
        .expect("Can't parse X Layer Devnet genesis hash")
});

/// X Layer Devnet genesis state root
///
/// The Merkle Patricia Trie root of all 1,866,483 accounts in the genesis alloc.
pub(crate) static XLAYER_DEVNET_STATE_ROOT: Lazy<B256> = Lazy::new(|| {
    include_str!("../../../crates/chainspec/res/genesis/xlayer-devnet-state-root.txt")
        .trim()
        .parse::<B256>()
        .expect("Can't parse X Layer Devnet state root")
});

/// X Layer devnet chain id as specified in the published `genesis.json`.
const XLAYER_DEVNET_CHAIN_ID: u64 = 195;

/// X Layer devnet EIP-1559 parameters.
///
/// Same as mainnet: see `config.optimism` in `genesis-devnet.json`.
const XLAYER_DEVNET_BASE_FEE_DENOMINATOR: u128 = 100_000_000;
const XLAYER_DEVNET_BASE_FEE_ELASTICITY: u128 = 1;

/// X Layer devnet base fee params (same for London and Canyon forks).
const XLAYER_DEVNET_BASE_FEE_PARAMS: BaseFeeParams =
    BaseFeeParams::new(XLAYER_DEVNET_BASE_FEE_DENOMINATOR, XLAYER_DEVNET_BASE_FEE_ELASTICITY);

/// The X Layer devnet spec
pub static XLAYER_DEVNET: Lazy<Arc<OpChainSpec>> = Lazy::new(|| {
    // Minimal genesis contains empty alloc field for fast loading
    let genesis = serde_json::from_str(include_str!("../../../crates/chainspec/res/genesis/xlayer-devnet.json"))
        .expect("Can't deserialize X Layer Devnet genesis json");
    let hardforks = XLAYER_DEVNET_HARDFORKS.clone();

    
    // Build genesis header using standard helper, then override state_root with pre-computed value
    let mut genesis_header = make_op_genesis_header(&genesis, &hardforks);
    genesis_header.state_root = *XLAYER_DEVNET_STATE_ROOT;
    // Set block number and parent hash from genesis JSON (not a standard genesis block 0)
    if let Some(number) = genesis.number {
        genesis_header.number = number;
    }
    if let Some(parent_hash) = genesis.parent_hash {
        genesis_header.parent_hash = parent_hash;
    }
    let genesis_header = SealedHeader::new(genesis_header, *XLAYER_DEVNET_GENESIS_HASH);

    OpChainSpec {
        inner: ChainSpec {
            chain: Chain::from_id(XLAYER_DEVNET_CHAIN_ID),
            genesis_header,
            genesis,
            paris_block_and_final_difficulty: Some((0, U256::from(0))),
            hardforks,
            base_fee_params: BaseFeeParamsKind::Variable(
                vec![
                    (EthereumHardfork::London.boxed(), XLAYER_DEVNET_BASE_FEE_PARAMS),
                    (OpHardfork::Canyon.boxed(), XLAYER_DEVNET_BASE_FEE_PARAMS),
                ]
                .into(),
            ),
            ..Default::default()
        },
    }
    .into()
});

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_genesis::Genesis;
    use alloy_primitives::hex;
    use reth_ethereum_forks::EthereumHardfork;
    use reth_optimism_forks::OpHardfork;

    const XLAYER_DEVNET_BLOCK_NUMBER: u64 = 18696116;

    fn parse_genesis() -> Genesis {
        serde_json::from_str(include_str!("../res/genesis/xlayer-devnet.json"))
            .expect("Failed to parse xlayer-devnet.json")
    }

    #[test]
    fn test_xlayer_devnet_chain_id() {
        assert_eq!(XLAYER_DEVNET.chain().id(), 195);
    }

    #[test]
    fn test_xlayer_devnet_genesis_hash() {
        assert_eq!(XLAYER_DEVNET.genesis_hash(), *XLAYER_DEVNET_GENESIS_HASH);
    }

    #[test]
    fn test_xlayer_devnet_state_root() {
        assert_eq!(XLAYER_DEVNET.genesis_header().state_root, *XLAYER_DEVNET_STATE_ROOT);
    }

    #[test]
    fn test_xlayer_devnet_genesis_block_number() {
        assert_eq!(XLAYER_DEVNET.genesis_header().number, XLAYER_DEVNET_BLOCK_NUMBER);
    }

    #[test]
    fn test_xlayer_devnet_hardforks() {
        let spec = &*XLAYER_DEVNET;
        assert!(spec.fork(EthereumHardfork::Shanghai).active_at_timestamp(0));
        assert!(spec.fork(EthereumHardfork::Cancun).active_at_timestamp(0));
        assert!(spec.fork(OpHardfork::Bedrock).active_at_block(0));
        assert!(spec.fork(OpHardfork::Isthmus).active_at_timestamp(0));
    }

    #[test]
    fn test_xlayer_devnet_base_fee_params() {
        assert_eq!(
            XLAYER_DEVNET.base_fee_params_at_timestamp(0),
            BaseFeeParams::new(
                XLAYER_DEVNET_BASE_FEE_DENOMINATOR,
                XLAYER_DEVNET_BASE_FEE_ELASTICITY
            )
        );
    }

    #[test]
    fn test_xlayer_devnet_fast_loading() {
        assert_eq!(XLAYER_DEVNET.genesis().alloc.len(), 0);
    }

    #[test]
    fn test_xlayer_devnet_paris_activated() {
        assert_eq!(XLAYER_DEVNET.get_final_paris_total_difficulty(), Some(U256::ZERO));
    }

    #[test]
    fn test_xlayer_devnet_canyon_base_fee_unchanged() {
        let spec = &*XLAYER_DEVNET;
        let london = spec.base_fee_params_at_timestamp(0);
        let canyon = spec.base_fee_params_at_timestamp(1);
        assert_eq!(london, canyon);
        assert_eq!(canyon, XLAYER_DEVNET_BASE_FEE_PARAMS);
    }

    #[test]
    fn test_xlayer_devnet_genesis_header_fields() {
        let header = XLAYER_DEVNET.genesis_header();
        assert_eq!(header.withdrawals_root, Some(alloy_consensus::constants::EMPTY_WITHDRAWALS));
        assert_eq!(header.parent_beacon_block_root, Some(B256::ZERO));
        assert_eq!(header.requests_hash, Some(alloy_eips::eip7685::EMPTY_REQUESTS_HASH));
    }

    #[test]
    fn test_xlayer_devnet_all_hardforks_active() {
        let spec = &*XLAYER_DEVNET;
        let ts = spec.genesis_header().timestamp;
        // Ethereum hardforks
        assert!(spec.fork(EthereumHardfork::London).active_at_block(0));
        assert!(spec.fork(EthereumHardfork::Shanghai).active_at_timestamp(ts));
        assert!(spec.fork(EthereumHardfork::Cancun).active_at_timestamp(ts));
        assert!(spec.fork(EthereumHardfork::Prague).active_at_timestamp(ts));
        // Optimism hardforks
        assert!(spec.fork(OpHardfork::Bedrock).active_at_block(0));
        assert!(spec.fork(OpHardfork::Regolith).active_at_timestamp(ts));
        assert!(spec.fork(OpHardfork::Canyon).active_at_timestamp(ts));
        assert!(spec.fork(OpHardfork::Ecotone).active_at_timestamp(ts));
        assert!(spec.fork(OpHardfork::Fjord).active_at_timestamp(ts));
        assert!(spec.fork(OpHardfork::Granite).active_at_timestamp(ts));
        assert!(spec.fork(OpHardfork::Holocene).active_at_timestamp(ts));
        assert!(spec.fork(OpHardfork::Isthmus).active_at_timestamp(ts));
        assert!(spec.fork(OpHardfork::Jovian).active_at_timestamp(ts));
    }

    #[test]
    fn test_xlayer_devnet_constants_match_spec() {
        assert_eq!(XLAYER_DEVNET.chain().id(), XLAYER_DEVNET_CHAIN_ID);
        assert_eq!(
            XLAYER_DEVNET.base_fee_params_at_timestamp(0),
            BaseFeeParams::new(
                XLAYER_DEVNET_BASE_FEE_DENOMINATOR,
                XLAYER_DEVNET_BASE_FEE_ELASTICITY
            )
        );
    }

    #[test]
    fn test_xlayer_devnet_json_config_consistency() {
        let genesis = parse_genesis();
        assert_eq!(genesis.config.chain_id, XLAYER_DEVNET_CHAIN_ID);
        assert_eq!(genesis.number, Some(XLAYER_DEVNET_BLOCK_NUMBER));
        assert_eq!(genesis.timestamp, 0x699d723d);
        assert_eq!(genesis.extra_data.as_ref(), hex!("01000000fa000000060000000000000000").as_ref());
        assert_eq!(genesis.gas_limit, 0xbebc200);
        assert_eq!(genesis.difficulty, U256::ZERO);
        assert_eq!(genesis.nonce, 0);
        assert_eq!(genesis.mix_hash, B256::ZERO);
        assert_eq!(genesis.coinbase.to_string(), "0x4200000000000000000000000000000000000011");
        assert_eq!(
            genesis.parent_hash,
            Some(b256!("a3a639b09fea244d577c7e7ed7bcc4eb1adb0c5b54441cd29d9949e417dfa355"))
        );
        assert_eq!(genesis.base_fee_per_gas.map(|fee| fee as u64), Some(0x5fc01c5u64));
        assert_eq!(genesis.excess_blob_gas, Some(0));
        assert_eq!(genesis.blob_gas_used, Some(0));
    }

    #[test]
    fn test_xlayer_devnet_json_optimism_config() {
        let genesis = parse_genesis();
        let cfg = genesis.config.extra_fields.get("optimism").expect("optimism config must exist");
        assert_eq!(
            cfg.get("eip1559Elasticity").and_then(|v| v.as_u64()).unwrap() as u128,
            XLAYER_DEVNET_BASE_FEE_ELASTICITY
        );
        assert_eq!(
            cfg.get("eip1559Denominator").and_then(|v| v.as_u64()).unwrap() as u128,
            XLAYER_DEVNET_BASE_FEE_DENOMINATOR
        );
        assert_eq!(
            cfg.get("eip1559DenominatorCanyon").and_then(|v| v.as_u64()).unwrap() as u128,
            XLAYER_DEVNET_BASE_FEE_DENOMINATOR
        );
    }

    #[test]
    fn test_xlayer_devnet_json_hardforks_warning() {
        let genesis = parse_genesis();
        // WARNING: Hardfork times in JSON are overridden by XLAYER_DEVNET_HARDFORKS
        assert_eq!(
            genesis.config.extra_fields.get("legacyXLayerBlock").and_then(|v| v.as_u64()),
            Some(XLAYER_DEVNET_BLOCK_NUMBER)
        );
        assert_eq!(genesis.config.shanghai_time, Some(0));
        assert_eq!(genesis.config.cancun_time, Some(0));
    }

    #[test]
    fn test_xlayer_devnet_genesis_header_matches_json() {
        let header = XLAYER_DEVNET.genesis_header();
        let genesis = parse_genesis();
        // Verify header fields match JSON (except state_root which is hardcoded)
        assert_eq!(header.number, genesis.number.unwrap_or_default());
        assert_eq!(header.timestamp, genesis.timestamp);
        assert_eq!(header.extra_data, genesis.extra_data);
        assert_eq!(header.gas_limit, genesis.gas_limit);
        assert_eq!(header.difficulty, genesis.difficulty);
        assert_eq!(header.nonce, alloy_primitives::B64::from(genesis.nonce));
        assert_eq!(header.mix_hash, genesis.mix_hash);
        assert_eq!(header.beneficiary, genesis.coinbase);
        assert_eq!(header.parent_hash, genesis.parent_hash.unwrap_or_default());
        assert_eq!(header.base_fee_per_gas, genesis.base_fee_per_gas.map(|fee| fee as u64));
        // NOTE: state_root is hardcoded, not read from JSON
        assert_eq!(header.state_root, *XLAYER_DEVNET_STATE_ROOT);
    }

    #[test]
    fn test_xlayer_devnet_jovian_activation() {
        use crate::XLAYER_DEVNET_JOVIAN_TIMESTAMP;

        let spec = &*XLAYER_DEVNET;

        // Jovian should not be active before activation timestamp
        assert!(!spec
            .fork(OpHardfork::Jovian)
            .active_at_timestamp(XLAYER_DEVNET_JOVIAN_TIMESTAMP - 1));

        // Jovian should be active at activation timestamp
        assert!(spec.fork(OpHardfork::Jovian).active_at_timestamp(XLAYER_DEVNET_JOVIAN_TIMESTAMP));

        // Jovian should be active after activation timestamp
        assert!(spec
            .fork(OpHardfork::Jovian)
            .active_at_timestamp(XLAYER_DEVNET_JOVIAN_TIMESTAMP + 1));
    }

    #[test]
    fn test_xlayer_devnet_jovian_included() {
        use crate::XLAYER_DEVNET_HARDFORKS;
        let hardforks = &*XLAYER_DEVNET_HARDFORKS;
        assert!(
            hardforks.get(OpHardfork::Jovian).is_some(),
            "XLayer devnet hardforks should include Jovian"
        );
    }

    #[test]
    fn test_xlayer_devnet_jovian_timestamp_condition() {
        use crate::{XLAYER_DEVNET_HARDFORKS, XLAYER_DEVNET_JOVIAN_TIMESTAMP};
        use reth_ethereum_forks::ForkCondition;

        let xlayer_devnet = &*XLAYER_DEVNET_HARDFORKS;

        let jovian_fork =
            xlayer_devnet.get(OpHardfork::Jovian).expect("XLayer devnet should have Jovian fork");

        match jovian_fork {
            ForkCondition::Timestamp(ts) => {
                assert_eq!(
                    ts, XLAYER_DEVNET_JOVIAN_TIMESTAMP,
                    "Jovian fork should use XLAYER_DEVNET_JOVIAN_TIMESTAMP"
                );
            }
            _ => panic!("Jovian fork should use timestamp condition"),
        }
    }
}
