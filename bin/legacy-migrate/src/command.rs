use clap::Parser;
use eyre::Result;
use tracing::{info, warn};

use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs};
use reth_optimism_chainspec::OpChainSpec;
use reth_storage_api::{BlockNumReader, DBProvider};

use crate::migrate::migrate_to_static_files;

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
    pub async fn execute<N>(&self) -> Result<()>
    where
        N: reth_cli_commands::common::CliNodeTypes<ChainSpec = C::ChainSpec>,
    {
        let Environment { provider_factory, .. } = self.env.init::<N>(AccessRights::RW)?;

        let provider = provider_factory.provider()?.disable_long_read_transaction_safety();
        let to_block = provider.best_block_number()?;
        let prune_modes = provider.prune_modes_ref().clone();
        drop(provider);

        info!(
            target: "reth::cli",
            to_block,
            batch_size = self.batch_size,
            "Migration parameters"
        );

        // Check if receipts can be migrated (no contract log pruning)
        let can_migrate_receipts = prune_modes.receipts_log_filter.is_empty();
        if !can_migrate_receipts {
            warn!(target: "reth::cli", "Receipts will NOT be migrated due to contract log pruning");
        }

        // Run static files and RocksDB migrations in parallel
        std::thread::scope(|s| {
            let _static_files_handle = if !self.skip_static_files {
                Some(s.spawn(|| {
                    info!(target: "reth::cli", "Starting static files migration");
                    migrate_to_static_files::<N>(
                        &provider_factory,
                        to_block,
                        can_migrate_receipts,
                    )
                }))
            } else {
                None
            };

            Ok::<_, eyre::Error>(())
        })?;

        Ok(())
    }
}
