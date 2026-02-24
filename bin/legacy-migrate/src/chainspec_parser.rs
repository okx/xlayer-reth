use crate::xlayer_mainnet::XLAYER_MAINNET;
use crate::xlayer_testnet::XLAYER_TESTNET;
use crate::xlayer_devnet::XLAYER_DEVNET;
use alloy_genesis::Genesis;
use reth_cli::chainspec::ChainSpecParser;
use reth_optimism_chainspec::{generated_chain_value_parser, OpChainSpec};
use std::sync::Arc;
use tracing::debug;

/// XLayer chain specification parser
///
/// This parser extends the default OpChainSpecParser to support XLayer chains:
/// - xlayer-mainnet (chain id 196)
/// - xlayer-testnet (chain id 1952)
///
/// It also supports all standard Optimism chains through delegation to the
/// upstream OpChainSpecParser.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub(crate) struct XLayerChainSpecParser;

impl ChainSpecParser for XLayerChainSpecParser {
    type ChainSpec = OpChainSpec;

    const SUPPORTED_CHAINS: &'static [&'static str] = &[
        // Standard OP chains
        "dev",
        "optimism",
        "optimism_sepolia",
        "optimism-sepolia",
        "base",
        "base_sepolia",
        "base-sepolia",
        // XLayer chains
        "xlayer-mainnet",
        "xlayer-testnet",
        "xlayer-devnet",
    ];

    fn parse(s: &str) -> eyre::Result<Arc<Self::ChainSpec>> {
        xlayer_chain_value_parser(s)
    }
}

/// Parse genesis from file path or JSON string
fn parse_genesis(s: &str) -> eyre::Result<Genesis> {
    // Use the standard reth parse_genesis to maintain compatibility
    let mut genesis = reth_cli::chainspec::parse_genesis(s)?;

    // XLayer extension: If legacyXLayerBlock is specified in config, override genesis.number
    // This allows XLayer to migrate from a legacy chain by setting the genesis
    // block number to match the legacy chain's starting block.
    if let Some(legacy_block_value) = genesis.config.extra_fields.get("legacyXLayerBlock")
        && let Some(legacy_block) = legacy_block_value.as_u64()
    {
        debug!("Overriding genesis.number from {:?} to {legacy_block}", genesis.number);
        genesis.number = Some(legacy_block);
    }

    Ok(genesis)
}

/// XLayer chain value parser
///
/// Parses chain specifications with the following priority:
/// 1. XLayer named chains (xlayer-mainnet, xlayer-testnet)
/// 2. Standard Optimism named chains (via `generated_chain_value_parser`)
/// 3. Genesis file path or JSON string (with `legacyXLayerBlock` support)
fn xlayer_chain_value_parser(s: &str) -> eyre::Result<Arc<OpChainSpec>> {
    match s {
        "xlayer-mainnet" => {
            // Support environment variable override for genesis path
            if let Ok(genesis_path) = std::env::var("XLAYER_MAINNET_GENESIS") {
                return Ok(Arc::new(parse_genesis(&genesis_path)?.into()));
            }
            Ok(XLAYER_MAINNET.clone())
        }
        "xlayer-testnet" => {
            // Support environment variable override for genesis path
            if let Ok(genesis_path) = std::env::var("XLAYER_TESTNET_GENESIS") {
                return Ok(Arc::new(parse_genesis(&genesis_path)?.into()));
            }
            Ok(XLAYER_TESTNET.clone())
        }
        "xlayer-devnet" => {
            // Support environment variable override for genesis path
            if let Ok(genesis_path) = std::env::var("XLAYER_DEVNET_GENESIS") {
                return Ok(Arc::new(parse_genesis(&genesis_path)?.into()));
            }
            Ok(XLAYER_DEVNET.clone())
        }
        // For other inputs, try known OP chains first, then parse as genesis
        _ => {
            // Try to match known OP chains (optimism, base, etc.)
            if let Some(op_chain_spec) = generated_chain_value_parser(s) {
                return Ok(op_chain_spec);
            }

            // Otherwise, parse as genesis file/JSON with XLayer extensions
            Ok(Arc::new(parse_genesis(s)?.into()))
        }
    }
}
