use clap::Parser;
use tracing::info;

use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs};
use reth_optimism_chainspec::OpChainSpec;

/// Migrate from legacy MDBX storage to new RocksDB + static files.
#[derive(Debug, Parser)]
pub struct Command<C: ChainSpecParser> {
    #[command(flatten)]
    env: EnvironmentArgs<C>,

    /// Block batch size for processing.
    #[arg(long, default_value = "10000")]
    batch_size: u64,

    /// Skip static file migration.
    #[arg(long)]
    skip_static_files: bool,

    /// Skip RocksDB migration.
    #[arg(long)]
    skip_rocksdb: bool,

    /// Keep migrated MDBX tables (don't drop them after migration).
    #[arg(long)]
    keep_mdbx: bool,
}

impl<C: ChainSpecParser<ChainSpec = OpChainSpec>> Command<C> {
    /// Execute the migration.
    pub async fn execute<N>(&self) -> anyhow::Result<()>
    where
        N: reth_cli_commands::common::CliNodeTypes<ChainSpec = C::ChainSpec>,
    {
        let Environment { provider_factory: _, .. } =
            self.env.init::<N>(AccessRights::RO).map_err(|e| anyhow::anyhow!("{}", e))?;

        info!("Hello, world!");
        Ok(())
    }
}

