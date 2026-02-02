use std::time::Instant;

use clap::Parser;
use eyre::Result;
use tracing::{info, warn};

use reth_db::{tables, DatabaseEnv};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs, CliNodeTypes};
use reth_optimism_chainspec::OpChainSpec;
use reth_provider::{ProviderFactory, StaticFileProviderFactory, MetadataWriter};
use reth_static_file_types::StaticFileSegment;
use reth_storage_api::{BlockNumReader, DBProvider};

use crate::migrate::{migrate_to_rocksdb, migrate_to_static_files};

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

        // Start tracking time
        let start_time = Instant::now();

        // Run static files and RocksDB migrations in parallel
        std::thread::scope(|s| {
            let static_files_handle = if !self.skip_static_files {
                Some(s.spawn(|| {
                    info!(target: "reth::cli", "Starting static files migration");
                    migrate_to_static_files::<N>(&provider_factory, to_block, can_migrate_receipts)
                }))
            } else {
                None
            };

            let rocksdb_handle = if !self.skip_rocksdb {
                Some(s.spawn(|| {
                    info!(target: "reth::cli", "Starting RocksDB migration");
                    migrate_to_rocksdb::<N>(&provider_factory, self.batch_size)
                }))
            } else {
                None
            };

            // Wait for static files migration
            if let Some(handle) = static_files_handle {
                handle.join().expect("static files thread panicked")?;
            }

            if let Some(handle) = rocksdb_handle {
                handle.join().expect("rocksdb thread panicked")?;
            }

            Ok::<_, eyre::Error>(())
        })?;

        // Finalize: update storage settings and optionally drop migrated MDBX tables
        info!(target: "reth::cli", "Finalizing migration");
        self.finalize::<N>(&provider_factory, can_migrate_receipts)?;

        let elapsed = start_time.elapsed();
        info!(
            target: "reth::cli",
            elapsed_secs = elapsed.as_secs(),
            "Migration completed"
        );

        Ok(())
    }

    fn finalize<N: CliNodeTypes>(
        &self,
        provider_factory: &ProviderFactory<
            reth_node_builder::NodeTypesWithDBAdapter<N, DatabaseEnv>,
        >,
        can_migrate_receipts: bool,
    ) -> Result<()> {
        use reth_db_api::transaction::DbTxMut;
        use reth_provider::StorageSettings;

        let provider = provider_factory.provider_rw()?;

        // Check if TransactionSenders actually has data in static files
        // If not, don't enable that setting to avoid consistency check failures
        let static_file_provider = provider_factory.static_file_provider();
        let senders_have_data = static_file_provider
            .get_highest_static_file_tx(StaticFileSegment::TransactionSenders)
            .is_some();

        if !senders_have_data {
            warn!(
                target: "reth::cli",
                "TransactionSenders has no data in static files, not enabling static file storage for senders"
            );
        }

        // Update storage settings - only enable senders if we have data
        let new_settings = StorageSettings::base()
            .with_receipts_in_static_files(can_migrate_receipts)
            .with_account_changesets_in_static_files(true)
            .with_transaction_senders_in_static_files(senders_have_data)
            .with_transaction_hash_numbers_in_rocksdb(true)
            .with_account_history_in_rocksdb(true)
            .with_storages_history_in_rocksdb(true);

        info!(target: "reth::cli", ?new_settings, "Writing storage settings");
        provider.write_storage_settings(new_settings)?;

        // Drop migrated MDBX tables unless --keep-mdbx is set
        if !self.keep_mdbx {
            let tx = provider.tx_ref();

            if !self.skip_static_files {
                info!(target: "reth::cli", "Dropping migrated static file tables from MDBX");
                tx.clear::<tables::TransactionSenders>()?;
                tx.clear::<tables::AccountChangeSets>()?;
                tx.clear::<tables::StorageChangeSets>()?;
                if can_migrate_receipts {
                    tx.clear::<tables::Receipts<<<N as reth_node_builder::NodeTypes>::Primitives as reth_primitives_traits::NodePrimitives>::Receipt>>()?;
                }
            }

            if !self.skip_rocksdb {
                info!(target: "reth::cli", "Dropping migrated RocksDB tables from MDBX");
                tx.clear::<tables::TransactionHashNumbers>()?;
                tx.clear::<tables::AccountsHistory>()?;
                tx.clear::<tables::StoragesHistory>()?;
            }
        } else {
            info!(target: "reth::cli", "Keeping MDBX tables (--keep-mdbx)");
        }

        provider.commit()?;

        Ok(())
    }
}
