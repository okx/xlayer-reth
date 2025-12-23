use clap::Args;
use std::time::Duration;

/// X Layer specific configuration flags
#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
#[command(next_help_heading = "X Layer")]
pub struct XLayerArgs {
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
}
