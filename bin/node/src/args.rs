use clap::Args;
use std::time::Duration;
use url::Url;

use xlayer_bridge_intercept::BridgeInterceptConfig;
use xlayer_builder::args::BuilderArgs;
use xlayer_monitor::FullLinkMonitorArgs;

/// Bridge intercept CLI arguments
#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
pub struct XLayerInterceptArgs {
    /// Enable bridge transaction interception for payload builder
    #[arg(
        long = "xlayer.intercept.enabled",
        default_value = "false",
        help = "Enable bridge transaction interception for payload builder"
    )]
    pub enabled: bool,

    /// PolygonZkEVMBridge contract address to monitor
    #[arg(
        long = "xlayer.intercept.bridge-contract",
        value_name = "ADDRESS",
        help = "PolygonZkEVMBridge contract address to monitor"
    )]
    pub bridge_contract: Option<String>,

    /// Target token address to block (use '*' or empty for wildcard).
    /// If omitted when interception is enabled, defaults to wildcard (blocks ALL bridge txs).
    #[arg(
        long = "xlayer.intercept.target-token",
        value_name = "ADDRESS",
        help = "Token address to block (use '*' or omit to block all bridge txs)"
    )]
    pub target_token: Option<String>,
}

impl XLayerInterceptArgs {
    /// Convert CLI args into a [`BridgeInterceptConfig`].
    /// Calls [`validate`] first so all error-checking lives in one place.
    pub fn to_bridge_intercept_config(&self) -> Result<BridgeInterceptConfig, String> {
        use alloy_primitives::Address;

        self.validate()?;

        // When disabled, skip all address parsing — the values are irrelevant and may
        // contain unparseable strings that would panic at the `.expect()` calls below.
        if !self.enabled {
            return Ok(BridgeInterceptConfig::default());
        }

        // After validation we know: if enabled, bridge_contract is Some and parseable;
        // target_token (when Some and not "*"/empty) is also parseable.
        let bridge_contract_address = self
            .bridge_contract
            .as_deref()
            .map(|a| a.parse::<Address>().expect("validated"))
            .unwrap_or(Address::ZERO);

        let (target_token_address, wildcard) = if let Some(token) = &self.target_token {
            if token.is_empty() || token == "*" {
                (Address::ZERO, true)
            } else {
                (token.parse::<Address>().expect("validated"), false)
            }
        } else if self.enabled {
            // --xlayer.intercept.target-token not provided: defaulting to wildcard mode,
            // which blocks ALL bridge transactions for the configured contract.
            tracing::warn!(
                target: "xlayer::intercept",
                "--xlayer.intercept.target-token not set; defaulting to wildcard mode \
                 which will block ALL bridge transactions for the configured contract. \
                 Use --xlayer.intercept.target-token <ADDRESS> to restrict to a specific token."
            );
            (Address::ZERO, true)
        } else {
            (Address::ZERO, false)
        };

        Ok(BridgeInterceptConfig {
            enabled: self.enabled,
            bridge_contract_address,
            target_token_address,
            wildcard,
        })
    }

    /// Validate intercept configuration.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.bridge_contract.is_none() {
            return Err(
                "--xlayer.intercept.bridge-contract is required when interception is enabled"
                    .to_string(),
            );
        }
        if let Some(addr) = &self.bridge_contract
            && addr.parse::<alloy_primitives::Address>().is_err()
        {
            return Err(format!("Invalid bridge contract address: {addr}"));
        }
        if let Some(token) = &self.target_token
            && !token.is_empty()
            && token != "*"
            && token.parse::<alloy_primitives::Address>().is_err()
        {
            return Err(format!("Invalid target token address: {token}"));
        }
        Ok(())
    }
}

/// X Layer specific configuration flags
#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
#[command(next_help_heading = "X Layer")]
pub struct XLayerArgs {
    /// Flashblock builder configuration
    #[command(flatten)]
    pub builder: BuilderArgs,

    /// Flashblocks RPC configuration
    #[command(flatten)]
    pub flashblocks_rpc: FlashblocksRpcArgs,

    /// Enable legacy rpc routing
    #[command(flatten)]
    pub legacy: LegacyRpcArgs,

    /// Full link monitor configuration
    #[command(flatten)]
    pub monitor: FullLinkMonitorArgs,

    /// Bridge intercept configuration
    #[command(flatten)]
    pub intercept: XLayerInterceptArgs,

    #[arg(
        long = "xlayer.sequencer-mode",
        help = "Enable sequencer mode for the node (default: false, i.e., RPC mode). This flag can be used by various business logic components to determine node behavior.",
        default_value = "false"
    )]
    pub sequencer_mode: bool,
}

impl XLayerArgs {
    /// Validate all X Layer configurations
    pub fn validate(&self) -> Result<(), String> {
        self.legacy.validate()?;
        self.monitor.validate()?;
        self.intercept.validate()?;
        Ok(())
    }

    /// Validate init command arguments for xlayer-mainnet and xlayer-testnet
    ///
    /// If --chain=xlayer-mainnet or --chain=xlayer-testnet is specified in init command,
    /// it must be provided as a genesis.json file path, not as a chain name.
    pub fn validate_init_command() {
        let args: Vec<String> = std::env::args().collect();

        // Check if this is an init command
        if args.len() < 2 || args[1] != "init" {
            return;
        }

        // Find --chain argument
        let mut chain_value: Option<String> = None;
        for (i, arg) in args.iter().enumerate() {
            if arg == "--chain" && i + 1 < args.len() {
                chain_value = Some(args[i + 1].clone());
                break;
            } else if arg.starts_with("--chain=") {
                chain_value = Some(arg.strip_prefix("--chain=").unwrap().to_string());
                break;
            }
        }

        if let Some(chain) = chain_value {
            // Check if chain is xlayer-mainnet or xlayer-testnet
            if chain == "xlayer-mainnet" || chain == "xlayer-testnet" {
                eprintln!(
                    "Error: For --chain={chain}, you must use a genesis.json file instead of the chain name.\n\
                    Please specify the path to your genesis.json file, e.g.:\n\
                    xlayer-reth-node init --chain=/path/to/genesis.json"
                );
                std::process::exit(1);
            }
        }
    }
}

/// X Layer legacy RPC arguments
#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
pub struct LegacyRpcArgs {
    /// Legacy RPC endpoint URL for routing historical data
    #[arg(long = "rpc.legacy-url", value_name = "URL")]
    pub legacy_rpc_url: Option<String>,

    /// Timeout for legacy RPC requests
    #[arg(
        long = "rpc.legacy-timeout",
        value_name = "DURATION",
        default_value = "30s",
        value_parser = humantime::parse_duration,
        requires = "legacy_rpc_url"
    )]
    pub legacy_rpc_timeout: Duration,
}

impl LegacyRpcArgs {
    /// Validate legacy RPC configuration
    pub fn validate(&self) -> Result<(), String> {
        if let Some(url_str) = &self.legacy_rpc_url {
            // Validate URL format
            Url::parse(url_str)
                .map_err(|e| format!("Invalid legacy RPC URL '{url_str}': {e:?}"))?;

            // Validate timeout is reasonable (not zero and not excessively long)
            if self.legacy_rpc_timeout.is_zero() {
                return Err("Legacy RPC timeout must be greater than zero".to_string());
            }

            // Warn if timeout is excessively long (more than 5 minutes)
            if self.legacy_rpc_timeout > Duration::from_secs(300) {
                tracing::warn!(
                    "Warning: Legacy RPC timeout is set to {:?}, which is unusually long",
                    self.legacy_rpc_timeout
                );
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
pub struct FlashblocksRpcArgs {
    /// Enable flashblocks RPC
    #[arg(
        long = "xlayer.flashblocks-url",
        help = "URL of the flashblocks RPC endpoint (disabled by default)"
    )]
    pub flashblock_url: Option<Url>,

    /// Enable custom flashblocks subscription
    #[arg(
        long = "xlayer.flashblocks-subscription",
        help = "Enable custom flashblocks subscription (disabled by default)",
        default_value = "false"
    )]
    pub enable_flashblocks_subscription: bool,

    /// Set the number of subscribed addresses in flashblocks subscription
    #[arg(
        long = "xlayer.flashblocks-subscription-max-addresses",
        help = "Set the number of subscribed addresses in flashblocks subscription",
        default_value = "1000"
    )]
    pub flashblocks_subscription_max_addresses: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Args, Parser};

    /// A helper type to parse Args more easily
    #[derive(Parser)]
    struct CommandParser<T: Args> {
        #[command(flatten)]
        args: T,
    }

    #[test]
    fn test_xlayer_args_default() {
        let args = CommandParser::<XLayerArgs>::parse_from(["reth"]).args;
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_legacy_rpc_disabled_by_default() {
        let args = LegacyRpcArgs::default();
        assert!(args.legacy_rpc_url.is_none());
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_legacy_rpc_valid_http_url() {
        let args = LegacyRpcArgs {
            legacy_rpc_url: Some("http://localhost:8545".to_string()),
            legacy_rpc_timeout: Duration::from_secs(30),
        };
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_legacy_rpc_valid_https_url() {
        let args = LegacyRpcArgs {
            legacy_rpc_url: Some("https://mainnet.infura.io/v3/YOUR-PROJECT-ID".to_string()),
            legacy_rpc_timeout: Duration::from_secs(30),
        };
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_legacy_rpc_valid_url_with_port() {
        let args = LegacyRpcArgs {
            legacy_rpc_url: Some("http://192.168.1.100:8545".to_string()),
            legacy_rpc_timeout: Duration::from_secs(30),
        };
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_legacy_rpc_invalid_url_format() {
        let args = LegacyRpcArgs {
            legacy_rpc_url: Some("not-a-valid-url".to_string()),
            legacy_rpc_timeout: Duration::from_secs(30),
        };
        let result = args.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid legacy RPC URL"));
    }

    #[test]
    fn test_legacy_rpc_empty_url() {
        let args = LegacyRpcArgs {
            legacy_rpc_url: Some("".to_string()),
            legacy_rpc_timeout: Duration::from_secs(30),
        };
        let result = args.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_legacy_rpc_invalid_scheme() {
        let args = LegacyRpcArgs {
            legacy_rpc_url: Some("ftp://example.com".to_string()),
            legacy_rpc_timeout: Duration::from_secs(30),
        };
        // This should pass validation (URL is valid, even if scheme is unusual)
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_legacy_rpc_zero_timeout() {
        let args = LegacyRpcArgs {
            legacy_rpc_url: Some("http://localhost:8545".to_string()),
            legacy_rpc_timeout: Duration::from_secs(0),
        };
        let result = args.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("timeout must be greater than zero"));
    }

    #[test]
    fn test_legacy_rpc_reasonable_timeout() {
        let args = LegacyRpcArgs {
            legacy_rpc_url: Some("http://localhost:8545".to_string()),
            legacy_rpc_timeout: Duration::from_secs(60),
        };
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_legacy_rpc_parse_with_url() {
        let args = CommandParser::<XLayerArgs>::parse_from([
            "reth",
            "--rpc.legacy-url",
            "http://localhost:8545",
            "--rpc.legacy-timeout",
            "30s",
        ])
        .args;

        assert_eq!(args.legacy.legacy_rpc_url, Some("http://localhost:8545".to_string()));
        assert_eq!(args.legacy.legacy_rpc_timeout, Duration::from_secs(30));
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_legacy_rpc_parse_url_only_uses_default_timeout() {
        let args = CommandParser::<XLayerArgs>::parse_from([
            "reth",
            "--rpc.legacy-url",
            "http://localhost:8545",
        ])
        .args;

        assert_eq!(args.legacy.legacy_rpc_url, Some("http://localhost:8545".to_string()));
        assert_eq!(args.legacy.legacy_rpc_timeout, Duration::from_secs(30)); // default
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_xlayer_args_with_valid_legacy_config() {
        let args = CommandParser::<XLayerArgs>::parse_from([
            "reth",
            "--rpc.legacy-url",
            "https://mainnet.infura.io/v3/test",
            "--rpc.legacy-timeout",
            "45s",
            "--xlayer.flashblocks-subscription",
            "--xlayer.flashblocks-subscription-max-addresses",
            "2000",
            "--xlayer.flashblocks-url",
            "ws://localhost:1111",
        ])
        .args;

        assert!(args.legacy.legacy_rpc_url.is_some());
        assert_eq!(args.legacy.legacy_rpc_timeout, Duration::from_secs(45));
        assert!(args.flashblocks_rpc.enable_flashblocks_subscription);
        assert_eq!(args.flashblocks_rpc.flashblocks_subscription_max_addresses, 2000);
        assert_eq!(
            args.flashblocks_rpc.flashblock_url,
            Some(Url::parse("ws://localhost:1111").unwrap())
        );
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_xlayer_args_with_invalid_legacy_url() {
        let args = XLayerArgs {
            legacy: LegacyRpcArgs {
                legacy_rpc_url: Some("invalid-url".to_string()),
                legacy_rpc_timeout: Duration::from_secs(30),
            },
            ..Default::default()
        };

        let result = args.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid legacy RPC URL"));
    }

    // --- XLayerInterceptArgs tests ---

    #[test]
    fn test_xlayer_intercept_args_disabled() {
        let args = XLayerInterceptArgs::default();
        assert!(!args.enabled);
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_parse_xlayer_intercept_enabled() {
        let args = CommandParser::<XLayerInterceptArgs>::parse_from([
            "reth",
            "--xlayer.intercept.enabled",
            "--xlayer.intercept.bridge-contract",
            "0x2a3dd3eb832af982ec71669e178424b10dca2ede",
            "--xlayer.intercept.target-token",
            "0x75231f58b43240c9718dd58b4967c5114342a86c",
        ])
        .args;
        assert!(args.enabled);
        assert!(args.validate().is_ok());
        let config = args.to_bridge_intercept_config().unwrap();
        assert!(config.enabled);
        assert!(!config.wildcard);
    }

    #[test]
    fn test_parse_xlayer_intercept_wildcard() {
        let args = CommandParser::<XLayerInterceptArgs>::parse_from([
            "reth",
            "--xlayer.intercept.enabled",
            "--xlayer.intercept.bridge-contract",
            "0x2a3dd3eb832af982ec71669e178424b10dca2ede",
            "--xlayer.intercept.target-token",
            "*",
        ])
        .args;
        assert!(args.enabled);
        assert!(args.validate().is_ok());
        let config = args.to_bridge_intercept_config().unwrap();
        assert!(config.enabled);
        assert!(config.wildcard);
    }

    #[test]
    fn test_parse_xlayer_intercept_only_bridge_contract() {
        let args = CommandParser::<XLayerInterceptArgs>::parse_from([
            "reth",
            "--xlayer.intercept.enabled",
            "--xlayer.intercept.bridge-contract",
            "0x2a3dd3eb832af982ec71669e178424b10dca2ede",
        ])
        .args;
        assert!(args.enabled);
        assert!(args.validate().is_ok());
        let config = args.to_bridge_intercept_config().unwrap();
        assert!(config.enabled);
        assert!(config.wildcard); // no token specified → defaults to wildcard
    }

    #[test]
    fn test_parse_xlayer_intercept_disabled_with_params() {
        let args = CommandParser::<XLayerInterceptArgs>::parse_from([
            "reth",
            "--xlayer.intercept.bridge-contract",
            "0x2a3dd3eb832af982ec71669e178424b10dca2ede",
        ])
        .args;
        assert!(!args.enabled);
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_xlayer_intercept_args_enabled_without_bridge_contract() {
        let args = XLayerInterceptArgs { enabled: true, bridge_contract: None, target_token: None };
        let result = args.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("bridge-contract"));
    }

    #[test]
    fn test_xlayer_intercept_to_config_specific_token() {
        let args = XLayerInterceptArgs {
            enabled: true,
            bridge_contract: Some("0x2a3dd3eb832af982ec71669e178424b10dca2ede".to_string()),
            target_token: Some("0x75231f58b43240c9718dd58b4967c5114342a86c".to_string()),
        };
        let config = args.to_bridge_intercept_config().unwrap();
        assert!(config.enabled);
        assert!(!config.wildcard);
        let expected: alloy_primitives::Address =
            "0x75231f58b43240c9718dd58b4967c5114342a86c".parse().unwrap();
        assert_eq!(config.target_token_address, expected);
    }

    #[test]
    fn test_xlayer_intercept_to_config_wildcard() {
        let args = XLayerInterceptArgs {
            enabled: true,
            bridge_contract: Some("0x2a3dd3eb832af982ec71669e178424b10dca2ede".to_string()),
            target_token: Some("*".to_string()),
        };
        let config = args.to_bridge_intercept_config().unwrap();
        assert!(config.enabled);
        assert!(config.wildcard);
    }

    #[test]
    fn test_xlayer_intercept_to_config_empty_token() {
        let args = XLayerInterceptArgs {
            enabled: true,
            bridge_contract: Some("0x2a3dd3eb832af982ec71669e178424b10dca2ede".to_string()),
            target_token: Some("".to_string()),
        };
        let config = args.to_bridge_intercept_config().unwrap();
        assert!(config.enabled);
        assert!(config.wildcard);
    }

    #[test]
    fn test_xlayer_intercept_invalid_bridge_address() {
        let args = XLayerInterceptArgs {
            enabled: true,
            bridge_contract: Some("not-an-address".to_string()),
            target_token: None,
        };
        let result = args.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("bridge contract address"));
    }

    #[test]
    fn test_xlayer_intercept_invalid_token_address() {
        let args = XLayerInterceptArgs {
            enabled: true,
            bridge_contract: Some("0x2a3dd3eb832af982ec71669e178424b10dca2ede".to_string()),
            target_token: Some("not-an-address".to_string()),
        };
        let result = args.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("token address"));
    }

    #[test]
    fn test_xlayer_intercept_to_config_disabled() {
        let args = XLayerInterceptArgs::default();
        let config = args.to_bridge_intercept_config().unwrap();
        assert!(!config.enabled);
    }

    #[test]
    fn test_xlayer_intercept_disabled_with_invalid_addresses_does_not_panic() {
        let args = XLayerInterceptArgs {
            enabled: false,
            bridge_contract: Some("not-an-address".to_string()),
            target_token: Some("also-garbage".to_string()),
        };
        let config = args.to_bridge_intercept_config().unwrap();
        assert!(!config.enabled);
    }

    #[test]
    fn test_xlayer_intercept_mixed_case_addresses() {
        let args = XLayerInterceptArgs {
            enabled: true,
            bridge_contract: Some("0x2A3DD3EB832aF982EC71669E178424b10Dca2EdE".to_string()),
            target_token: Some("0x75231F58B43240C9718DD58B4967C5114342a86c".to_string()),
        };
        assert!(args.validate().is_ok());
        let config = args.to_bridge_intercept_config().unwrap();
        assert!(config.enabled);
        assert!(!config.wildcard);
    }
}
