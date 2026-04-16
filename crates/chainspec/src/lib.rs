//! XLayer chain specifications
//!
//! This crate provides chain specifications for XLayer mainnet and testnet networks.

mod parser;
mod xlayer_devnet;
mod xlayer_mainnet;
mod xlayer_testnet;

pub use parser::XLayerChainSpecParser;
pub use xlayer_devnet::XLAYER_DEVNET;
pub use xlayer_mainnet::XLAYER_MAINNET;
pub use xlayer_testnet::XLAYER_TESTNET;

// Re-export OpChainSpec for convenience
pub use reth_optimism_chainspec::OpChainSpec;

use alloy_primitives::U256;
use once_cell::sync::Lazy;
use reth_chainspec::Hardfork;
use reth_ethereum_forks::{hardfork, ChainHardforks, EthereumHardfork, ForkCondition};
use reth_optimism_forks::OpHardfork;

// ---------------------------------------------------------------------------
// XLayer-specific hardforks (not part of the upstream OpHardfork enum)
// ---------------------------------------------------------------------------

hardfork!(
    /// XLayer-specific hardforks that extend the OP Stack fork schedule.
    XLayerHardfork {
        /// Native Account Abstraction (EIP-8130) activation.
        NativeAA,
    }
);

/// Returns `true` if the Native AA hardfork is active at the given `timestamp`
/// for the provided chain spec.
pub fn is_native_aa_active(hardforks: &ChainHardforks, timestamp: u64) -> bool {
    hardforks.is_fork_active_at_timestamp(XLayerHardfork::NativeAA, timestamp)
}

/// XLayer mainnet Jovian hardfork activation timestamp
/// 2025-12-02 16:00:01 UTC
pub const XLAYER_MAINNET_JOVIAN_TIMESTAMP: u64 = 1764691201;

/// XLayer testnet Jovian hardfork activation timestamp
/// 2025-11-28 11:00:00 UTC
pub const XLAYER_TESTNET_JOVIAN_TIMESTAMP: u64 = 1764327600;

/// XLayer devnet Jovian hardfork activation timestamp
/// 2025-11-28 11:00:00 UTC
pub const XLAYER_DEVNET_JOVIAN_TIMESTAMP: u64 = 1764327600;

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
        (OpHardfork::Jovian.boxed(), ForkCondition::Timestamp(XLAYER_MAINNET_JOVIAN_TIMESTAMP)),
        // NativeAA: not yet scheduled on mainnet
        (XLayerHardfork::NativeAA.boxed(), ForkCondition::Never),
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
        (OpHardfork::Jovian.boxed(), ForkCondition::Timestamp(XLAYER_TESTNET_JOVIAN_TIMESTAMP)),
        // NativeAA: not yet scheduled on testnet
        (XLayerHardfork::NativeAA.boxed(), ForkCondition::Never),
    ])
});

/// X Layer devnet list of hardforks.
///
/// All time-based hardforks are activated at genesis (timestamp 0).
pub static XLAYER_DEVNET_HARDFORKS: Lazy<ChainHardforks> = Lazy::new(|| {
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
        (OpHardfork::Jovian.boxed(), ForkCondition::Timestamp(XLAYER_DEVNET_JOVIAN_TIMESTAMP)),
        // NativeAA: activated at genesis on devnet for testing
        (XLayerHardfork::NativeAA.boxed(), ForkCondition::Timestamp(0)),
    ])
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_aa_active_on_devnet_at_genesis() {
        assert!(is_native_aa_active(&XLAYER_DEVNET_HARDFORKS, 0));
        assert!(is_native_aa_active(&XLAYER_DEVNET_HARDFORKS, u64::MAX));
    }

    #[test]
    fn native_aa_never_on_mainnet() {
        assert!(!is_native_aa_active(&XLAYER_MAINNET_HARDFORKS, 0));
        assert!(!is_native_aa_active(&XLAYER_MAINNET_HARDFORKS, u64::MAX));
    }

    #[test]
    fn native_aa_never_on_testnet() {
        assert!(!is_native_aa_active(&XLAYER_TESTNET_HARDFORKS, 0));
        assert!(!is_native_aa_active(&XLAYER_TESTNET_HARDFORKS, u64::MAX));
    }

    #[test]
    fn xlayer_hardfork_name() {
        assert_eq!(XLayerHardfork::NativeAA.to_string(), "NativeAA");
    }
}
