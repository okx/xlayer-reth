//! XLayer chain specification parser

use crate::{XLAYER_MAINNET, XLAYER_TESTNET};
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
pub struct XLayerChainSpecParser;

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
    if let Some(legacy_block_value) = genesis.config.extra_fields.get("legacyXLayerBlock") {
        if let Some(legacy_block) = legacy_block_value.as_u64() {
            debug!("Overriding genesis.number from {:?} to {legacy_block}", genesis.number);
            genesis.number = Some(legacy_block);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_xlayer_mainnet() {
        let spec = XLayerChainSpecParser::parse("xlayer-mainnet").unwrap();
        assert_eq!(spec.chain().id(), 196);
    }

    #[test]
    fn test_parse_xlayer_testnet() {
        let spec = XLayerChainSpecParser::parse("xlayer-testnet").unwrap();
        assert_eq!(spec.chain().id(), 1952);
    }

    #[test]
    fn test_parse_optimism() {
        let spec = XLayerChainSpecParser::parse("optimism").unwrap();
        assert_eq!(spec.chain().id(), 10);
    }

    #[test]
    fn test_parse_base() {
        let spec = XLayerChainSpecParser::parse("base").unwrap();
        assert_eq!(spec.chain().id(), 8453);
    }

    #[test]
    fn test_supported_chains() {
        assert!(XLayerChainSpecParser::SUPPORTED_CHAINS.contains(&"xlayer-mainnet"));
        assert!(XLayerChainSpecParser::SUPPORTED_CHAINS.contains(&"xlayer-testnet"));
        assert!(XLayerChainSpecParser::SUPPORTED_CHAINS.contains(&"optimism"));
        assert!(XLayerChainSpecParser::SUPPORTED_CHAINS.contains(&"base"));
    }

    #[test]
    fn test_parse_all_supported_chains() {
        for &chain in XLayerChainSpecParser::SUPPORTED_CHAINS {
            assert!(XLayerChainSpecParser::parse(chain).is_ok(), "Failed to parse {chain}");
        }
    }

    #[test]
    fn test_legacy_xlayer_block_override() {
        use serde_json::json;

        // Create a genesis JSON with legacyXLayerBlock
        let genesis_json = json!({
            "config": {
                "chainId": 196,
                "homesteadBlock": 0,
                "eip150Block": 0,
                "eip155Block": 0,
                "eip158Block": 0,
                "byzantiumBlock": 0,
                "constantinopleBlock": 0,
                "petersburgBlock": 0,
                "istanbulBlock": 0,
                "berlinBlock": 0,
                "londonBlock": 0,
                "legacyXLayerBlock": 12345
            },
            "nonce": "0x0",
            "timestamp": "0x0",
            "extraData": "0x",
            "gasLimit": "0x1000000",
            "difficulty": "0x0",
            "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "coinbase": "0x0000000000000000000000000000000000000000",
            "alloc": {},
            "number": "0x0"
        });

        let genesis_str = serde_json::to_string(&genesis_json).unwrap();
        let result = parse_genesis(&genesis_str);

        assert!(result.is_ok(), "Failed to parse genesis with legacyXLayerBlock");
        let genesis = result.unwrap();

        // Verify that genesis.number was overridden to legacyXLayerBlock value
        assert_eq!(
            genesis.number,
            Some(12345),
            "genesis.number should be overridden by legacyXLayerBlock"
        );
    }

    #[test]
    fn test_genesis_without_legacy_block() {
        use serde_json::json;

        // Create a genesis JSON without legacyXLayerBlock
        let genesis_json = json!({
            "config": {
                "chainId": 196,
                "homesteadBlock": 0,
                "eip150Block": 0,
                "eip155Block": 0,
                "eip158Block": 0,
                "byzantiumBlock": 0,
                "constantinopleBlock": 0,
                "petersburgBlock": 0,
                "istanbulBlock": 0,
                "berlinBlock": 0,
                "londonBlock": 0
            },
            "nonce": "0x0",
            "timestamp": "0x0",
            "extraData": "0x",
            "gasLimit": "0x1000000",
            "difficulty": "0x0",
            "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "coinbase": "0x0000000000000000000000000000000000000000",
            "alloc": {},
            "number": "0x64"
        });

        let genesis_str = serde_json::to_string(&genesis_json).unwrap();
        let result = parse_genesis(&genesis_str);

        assert!(result.is_ok(), "Failed to parse genesis without legacyXLayerBlock");
        let genesis = result.unwrap();

        // Verify that genesis.number remains unchanged (0x64 = 100)
        assert_eq!(
            genesis.number,
            Some(100),
            "genesis.number should remain unchanged when legacyXLayerBlock is not present"
        );
    }
}
