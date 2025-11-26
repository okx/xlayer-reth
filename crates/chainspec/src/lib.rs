//! XLayer chain specifications
//!
//! This crate provides chain specifications for XLayer mainnet and testnet networks.

mod parser;
mod xlayer_mainnet;
mod xlayer_testnet;

pub use parser::XLayerChainSpecParser;
pub use xlayer_mainnet::XLAYER_MAINNET;
pub use xlayer_testnet::XLAYER_TESTNET;

// Re-export OpChainSpec for convenience
pub use reth_optimism_chainspec::OpChainSpec;

use alloy_primitives::U256;
use once_cell::sync::Lazy;
use reth_chainspec::Hardfork;
use reth_ethereum_forks::{ChainHardforks, EthereumHardfork, ForkCondition};
use reth_optimism_forks::OpHardfork;

/// X Layer mainnet list of hardforks.
///
/// All time-based hardforks are activated at genesis (timestamp 0).
pub static XLAYER_MAINNET_HARDFORKS: Lazy<ChainHardforks> = Lazy::new(|| {
    ChainHardforks::new(vec![
        (EthereumHardfork::Frontier.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Homestead.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Tangerine.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::SpuriousDragon.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Byzantium.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Constantinople.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Petersburg.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Istanbul.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::MuirGlacier.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Berlin.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::London.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::ArrowGlacier.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::GrayGlacier.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::Paris.boxed(),
            ForkCondition::TTD {
                activation_block_number: 0,
                fork_block: Some(0),
                total_difficulty: U256::ZERO,
            },
        ),
        (OpHardfork::Bedrock.boxed(), ForkCondition::Block(0)),
        (OpHardfork::Regolith.boxed(), ForkCondition::Timestamp(0)),
        (EthereumHardfork::Shanghai.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Canyon.boxed(), ForkCondition::Timestamp(0)),
        (EthereumHardfork::Cancun.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Ecotone.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Fjord.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Granite.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Holocene.boxed(), ForkCondition::Timestamp(0)),
        (EthereumHardfork::Prague.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Isthmus.boxed(), ForkCondition::Timestamp(0)),
    ])
});

/// X Layer testnet list of hardforks.
///
/// All time-based hardforks are activated at genesis (timestamp 0).
pub static XLAYER_TESTNET_HARDFORKS: Lazy<ChainHardforks> = Lazy::new(|| {
    ChainHardforks::new(vec![
        (EthereumHardfork::Frontier.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Homestead.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Tangerine.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::SpuriousDragon.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Byzantium.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Constantinople.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Petersburg.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Istanbul.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::MuirGlacier.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Berlin.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::London.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::ArrowGlacier.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::GrayGlacier.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::Paris.boxed(),
            ForkCondition::TTD {
                activation_block_number: 0,
                fork_block: Some(0),
                total_difficulty: U256::ZERO,
            },
        ),
        (OpHardfork::Bedrock.boxed(), ForkCondition::Block(0)),
        (OpHardfork::Regolith.boxed(), ForkCondition::Timestamp(0)),
        (EthereumHardfork::Shanghai.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Canyon.boxed(), ForkCondition::Timestamp(0)),
        (EthereumHardfork::Cancun.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Ecotone.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Fjord.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Granite.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Holocene.boxed(), ForkCondition::Timestamp(0)),
        (EthereumHardfork::Prague.boxed(), ForkCondition::Timestamp(0)),
        (OpHardfork::Isthmus.boxed(), ForkCondition::Timestamp(0)),
    ])
});
