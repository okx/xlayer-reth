use clap::Parser;
use tracing::info;

use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::OpNode;
use reth_tracing::{RethTracer, Tracer};
use xlayer_chainspec::XLayerChainSpecParser;

/// Migrate from legacy MDBX storage to new RocksDB + static files.
#[derive(Debug, Parser)]
pub struct MigrateCommand<C: ChainSpecParser> {
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

impl<C: ChainSpecParser<ChainSpec = OpChainSpec>> MigrateCommand<C> {
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

#[tokio::main]
async fn main() {
    let _ = RethTracer::new().init().expect("Failed to initialize tracing");

    if let Err(err) = MigrateCommand::<XLayerChainSpecParser>::parse().execute::<OpNode>().await {
        eprintln!("Migration failed: {}", err);
        std::process::exit(1);
    }
}
