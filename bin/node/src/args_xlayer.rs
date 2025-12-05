use clap::Args;
use std::time::Duration;

/// X Layer specific configuration flags
#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
#[command(next_help_heading = "X Layer")]
pub struct XLayerArgs {
    /// Bridge transaction interception configuration
    #[command(flatten)]
    pub intercept: XLayerInterceptArgs,

    /// Enable Apollo
    #[command(flatten)]
    pub apollo: ApolloArgs,

    /// Enable legacy rpc routing
    #[command(flatten)]
    pub legacy: LegacyRpcArgs,

    /// Enable inner transaction capture and storage
    #[arg(
        long = "xlayer.enable-innertx",
        help = "Enable inner transaction capture and storage (disabled by default)",
        default_value = "false"
    )]
    pub enable_inner_tx: bool,
}

impl XLayerArgs {
    /// Validate all X Layer configurations
    pub fn validate(&self) -> Result<(), String> {
        self.intercept.validate()
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
                    "Error: For --chain={}, you must use a genesis.json file instead of the chain name.\n\
                    Please specify the path to your genesis.json file, e.g.:\n\
                    xlayer-reth-node init --chain=/path/to/genesis.json",
                    chain
                );
                std::process::exit(1);
            }
        }
    }
}

/// X Layer Bridge transaction interception arguments
#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
pub struct XLayerInterceptArgs {
    /// Enable bridge transaction interception
    #[arg(
        long = "xlayer.intercept.enabled",
        help = "Enable bridge transaction interception for payload builder",
        default_value = "false"
    )]
    pub enabled: bool,

    /// Bridge contract address to monitor
    #[arg(
        long = "xlayer.intercept.bridge-contract",
        help = "PolygonZkEVMBridge contract address to monitor for interception",
        value_name = "ADDRESS"
    )]
    pub bridge_contract: Option<String>,

    /// Target token address to intercept
    #[arg(
        long = "xlayer.intercept.target-token",
        help = "Target token address to intercept (use empty string or '*' for wildcard mode)",
        value_name = "ADDRESS"
    )]
    pub target_token: Option<String>,
}

impl XLayerInterceptArgs {
    /// Validate the configuration
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
            return Err(format!("Invalid bridge contract address format: {addr}"));
        }

        if let Some(token) = &self.target_token
            && !token.is_empty()
            && token != "*"
            && token.parse::<alloy_primitives::Address>().is_err()
        {
            return Err(format!("Invalid target token address format: {token}"));
        }

        Ok(())
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

/// Apollo configuration arguments
#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
pub struct ApolloArgs {
    /// Enable Apollo
    #[arg(id = "apollo.enabled", long = "apollo.enabled", default_value_t = false)]
    pub enabled: bool,

    /// Configure Apollo app ID.
    #[arg(long = "apollo.app-id", default_value = "")]
    pub apollo_app_id: String,

    /// Configure Apollo IP.
    #[arg(long = "apollo.ip", default_value = "")]
    pub apollo_ip: String,

    /// Configure Apollo cluster.
    #[arg(long = "apollo.cluster", default_value = "")]
    pub apollo_cluster: String,

    /// Configure Apollo namespace.
    #[arg(long = "apollo.namespace", default_value = "")]
    pub apollo_namespace: String,
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
        let default_args = XLayerArgs::default();
        let args = CommandParser::<XLayerArgs>::parse_from(["reth"]).args;
        assert_eq!(args.intercept.enabled, default_args.intercept.enabled);
        assert_eq!(args.intercept.bridge_contract, default_args.intercept.bridge_contract);
        assert_eq!(args.intercept.target_token, default_args.intercept.target_token);
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_xlayer_args_disabled() {
        let args = XLayerArgs::default();
        assert!(!args.intercept.enabled);
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_parse_xlayer_intercept_enabled() {
        let args = CommandParser::<XLayerArgs>::parse_from([
            "reth",
            "--xlayer.intercept.enabled",
            "--xlayer.intercept.bridge-contract",
            "0x2a3DD3EB832aF982ec71669E178424b10Dca2EDe",
            "--xlayer.intercept.target-token",
            "0x75231F58b43240C9718Dd58B4967c5114342a86c",
        ])
        .args;

        assert!(args.intercept.enabled);
        assert_eq!(
            args.intercept.bridge_contract,
            Some("0x2a3DD3EB832aF982ec71669E178424b10Dca2EDe".to_string())
        );
        assert_eq!(
            args.intercept.target_token,
            Some("0x75231F58b43240C9718Dd58B4967c5114342a86c".to_string())
        );
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_parse_xlayer_intercept_wildcard() {
        let args = CommandParser::<XLayerArgs>::parse_from([
            "reth",
            "--xlayer.intercept.enabled",
            "--xlayer.intercept.bridge-contract",
            "0x2a3DD3EB832aF982ec71669E178424b10Dca2EDe",
            "--xlayer.intercept.target-token",
            "*",
        ])
        .args;

        assert!(args.intercept.enabled);
        assert_eq!(args.intercept.target_token, Some("*".to_string()));
    }

    #[test]
    fn test_parse_xlayer_intercept_only_bridge_contract() {
        let args = CommandParser::<XLayerArgs>::parse_from([
            "reth",
            "--xlayer.intercept.enabled",
            "--xlayer.intercept.bridge-contract",
            "0x2a3DD3EB832aF982ec71669E178424b10Dca2EDe",
        ])
        .args;

        assert!(args.intercept.enabled);
        assert_eq!(
            args.intercept.bridge_contract,
            Some("0x2a3DD3EB832aF982ec71669E178424b10Dca2EDe".to_string())
        );
        assert_eq!(args.intercept.target_token, None);
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_parse_xlayer_intercept_disabled_with_params() {
        // Even with bridge contract set, if not enabled, should parse successfully
        let args = CommandParser::<XLayerArgs>::parse_from([
            "reth",
            "--xlayer.intercept.bridge-contract",
            "0x2a3DD3EB832aF982ec71669E178424b10Dca2EDe",
        ])
        .args;

        assert!(!args.intercept.enabled);
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_xlayer_intercept_args_enabled_without_bridge_contract() {
        let args = XLayerInterceptArgs { enabled: true, bridge_contract: None, target_token: None };
        assert!(args.validate().is_err());
    }

    #[test]
    fn test_xlayer_intercept_invalid_bridge_address() {
        let args = XLayerInterceptArgs {
            enabled: true,
            bridge_contract: Some("invalid".to_string()),
            target_token: None,
        };

        assert!(args.validate().is_err());
    }

    #[test]
    fn test_xlayer_intercept_invalid_token_address() {
        let args = XLayerInterceptArgs {
            enabled: true,
            bridge_contract: Some("0x2a3DD3EB832aF982ec71669E178424b10Dca2EDe".to_string()),
            target_token: Some("invalid_address".to_string()),
        };

        assert!(args.validate().is_err());
    }

    #[test]
    fn test_xlayer_intercept_mixed_case_addresses() {
        let args = XLayerInterceptArgs {
            enabled: true,
            bridge_contract: Some("0x2A3DD3eb832Af982EC71669e178424b10DcA2ede".to_string()),
            target_token: Some("0x75231f58B43240c9718dd58b4967C5114342A86C".to_string()),
        };

        assert!(args.validate().is_ok());
    }
}
