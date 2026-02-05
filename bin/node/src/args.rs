use clap::Args;
use std::time::Duration;
use url::Url;

/// X Layer specific configuration flags
#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
#[command(next_help_heading = "X Layer")]
pub struct XLayerArgs {
    /// Enable legacy rpc routing
    #[command(flatten)]
    pub legacy: LegacyRpcArgs,

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

impl XLayerArgs {
    /// Validate all X Layer configurations
    pub fn validate(&self) -> Result<(), String> {
        self.legacy.validate()
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
        ])
        .args;

        assert!(args.enable_flashblocks_subscription);
        assert!(args.legacy.legacy_rpc_url.is_some());
        assert_eq!(args.legacy.legacy_rpc_timeout, Duration::from_secs(45));
        assert_eq!(args.flashblocks_subscription_max_addresses, 2000);
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_xlayer_args_with_invalid_legacy_url() {
        let args = XLayerArgs {
            legacy: LegacyRpcArgs {
                legacy_rpc_url: Some("invalid-url".to_string()),
                legacy_rpc_timeout: Duration::from_secs(30),
            },
            enable_flashblocks_subscription: false,
            flashblocks_subscription_max_addresses: 1000,
        };

        let result = args.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid legacy RPC URL"));
    }
}
