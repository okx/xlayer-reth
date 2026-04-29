//! `legacy-migrate` command for migrating v1 storage layout to v2.
//!
//! Ported from upstream reth's `db migrate-v2` command
//! (`crates/cli/commands/src/db/migrate_v2.rs`). Adapted for XLayer:
//!   * wrapped in our CLI command shape (`EnvironmentArgs<OpChainSpec>`)
//!   * supports a non-zero genesis block (XLayer mainnet does not start at 0)
//!
//! The migration:
//!   1. Migrates changesets + receipts from MDBX to static files
//!   2. Flips `StorageSettings` to v2
//!   3. Clears recomputable MDBX tables + resets stage checkpoints
//!   4. Compacts MDBX (with atomic backup-and-swap)
//!
//! After this completes the node must be restarted so the pipeline can rebuild
//! the cleared tables.

use alloy_primitives::Address;
use clap::Parser;
use eyre::Result;
use reth_chainspec::ChainSpecProvider;
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, CliNodeTypes, Environment, EnvironmentArgs};
use reth_db::{
    mdbx::{self, ffi},
    models::StorageBeforeTx,
    DatabaseEnv,
};
use reth_db_api::{
    cursor::DbCursorRO,
    database::Database,
    table::Table,
    tables,
    transaction::{DbTx, DbTxMut},
};
use reth_node_builder::NodeTypesWithDBAdapter;
use reth_optimism_chainspec::OpChainSpec;
use reth_provider::{
    providers::ProviderNodeTypes, DBProvider, DatabaseProviderFactory, MetadataProvider,
    MetadataWriter, ProviderFactory, PruneCheckpointReader, StageCheckpointWriter,
    StaticFileProviderFactory, StaticFileWriter, StorageSettings,
};
use reth_prune_types::PruneSegment;
use reth_stages_types::{StageCheckpoint, StageId};
use reth_static_file_types::StaticFileSegment;
use reth_storage_api::StageCheckpointReader;
use tracing::info;

/// Migrate from legacy v1 storage layout to v2 (changesets + receipts → static files).
#[derive(Debug, Parser)]
pub struct LegacyMigrateCommand<C: ChainSpecParser> {
    #[command(flatten)]
    env: EnvironmentArgs<C>,
}

impl<C: ChainSpecParser<ChainSpec = OpChainSpec>> LegacyMigrateCommand<C> {
    /// Execute the full v1 → v2 migration.
    pub async fn execute<N>(&self, runtime: reth_tasks::Runtime) -> Result<()>
    where
        N: CliNodeTypes<ChainSpec = C::ChainSpec>,
        N::Primitives: reth_primitives_traits::NodePrimitives<
            Receipt: reth_db_api::table::Value + reth_codecs::Compact,
        >,
    {
        let Environment { provider_factory, .. } = self.env.init::<N>(AccessRights::RW, runtime)?;

        // === Phase 0: Preflight ===
        info!(target: "reth::cli", "Starting v1 → v2 storage migration");

        let provider = provider_factory.provider()?;
        let current_settings = provider.storage_settings()?;

        if current_settings.is_some_and(|s| s.is_v2()) {
            info!(target: "reth::cli", "Storage is already v2, nothing to do");
            return Ok(());
        }

        let tip =
            provider.get_stage_checkpoint(StageId::Execution)?.map(|c| c.block_number).unwrap_or(0);

        // XLayer-specific: chain may have a non-zero genesis block. We must never
        // scan or attempt to write static-file entries below it. The static-file
        // provider itself is already genesis-aware (see `EnvironmentArgs::init`
        // calling `with_genesis_block_number`); this floor only avoids walking
        // empty MDBX keys below genesis.
        let genesis_block = provider_factory.chain_spec().genesis_header().number;

        info!(target: "reth::cli", tip, genesis_block, "Chain tip block number");

        if tip < genesis_block {
            eyre::bail!(
                "Invalid chain state: execution checkpoint {} is before genesis block {}",
                tip,
                genesis_block,
            );
        }

        let sf_provider = provider_factory.static_file_provider();

        for segment in [StaticFileSegment::AccountChangeSets, StaticFileSegment::StorageChangeSets]
        {
            if sf_provider.get_highest_static_file_block(segment).is_some() {
                eyre::bail!(
                    "Static file segment {segment:?} already contains data. \
                     Cannot migrate — target must be empty."
                );
            }
        }

        drop(provider);

        // === Phase 1: Migrate changesets → static files ===
        Self::migrate_account_changesets(&provider_factory, tip, genesis_block)?;
        Self::migrate_storage_changesets(&provider_factory, tip, genesis_block)?;

        // === Phase 2: Migrate receipts → static files ===
        Self::migrate_receipts::<NodeTypesWithDBAdapter<N, DatabaseEnv>>(
            &provider_factory,
            tip,
            genesis_block,
        )?;

        // === Phase 3: Flip metadata to v2 ===
        info!(target: "reth::cli", "Writing StorageSettings v2 metadata");
        {
            let provider_rw = provider_factory.database_provider_rw()?;
            provider_rw.write_storage_settings(StorageSettings::v2())?;
            provider_rw.commit()?;
        }
        info!(target: "reth::cli", "Storage settings updated to v2");

        // === Phase 4: Clear recomputable tables ===
        Self::clear_recomputable_tables(&provider_factory, genesis_block)?;

        // === Phase 5: Compact MDBX (before pipeline, so it runs on a smaller DB) ===
        let db_path = provider_factory.db_ref().path().to_path_buf();
        Self::compact_mdbx(provider_factory.db_ref())?;

        // Drop to release DB handle for swap.
        drop(provider_factory);

        let compact_path = db_path.with_file_name("db_compact");
        Self::swap_compacted_db(&db_path, &compact_path)?;

        info!(
            target: "reth::cli",
            "Migration complete. Restart the node and let it run the pipeline to rebuild the remaining data."
        );
        Ok(())
    }

    fn migrate_account_changesets<N: ProviderNodeTypes>(
        factory: &ProviderFactory<N>,
        tip: u64,
        genesis_block: u64,
    ) -> Result<()> {
        info!(target: "reth::cli", "Migrating AccountChangeSets → static files");
        let provider = factory.provider()?.disable_long_read_transaction_safety();
        let sf_provider = factory.static_file_provider();

        let mut cursor = provider.tx_ref().cursor_read::<tables::AccountChangeSets>()?;

        // Floor at max(prune_checkpoint + 1, genesis_block). Pruning tells us how
        // much MDBX has been pruned away; genesis tells us where the chain even
        // starts. Scanning below either is wasted work.
        let first_block = provider
            .get_prune_checkpoint(PruneSegment::AccountHistory)?
            .and_then(|cp| cp.block_number)
            .map_or(0, |b| b + 1)
            .max(genesis_block);

        let mut writer = sf_provider.latest_writer(StaticFileSegment::AccountChangeSets)?;
        // Fill the leading gap [genesis, first_block) with empty entries (only
        // when there's actually a gap from pruning). For non-zero genesis
        // chains we must first bridge the writer to genesis because its
        // `next_block_number` starts at genesis, not 0 — and upstream's
        // `ensure_at_block` opens with `increment_block(0)` for fresh writers.
        if first_block > genesis_block {
            if genesis_block > 0 && writer.current_block_number().is_none() {
                writer.increment_block(genesis_block)?;
            }
            writer.ensure_at_block(first_block - 1)?;
        }

        let mut count = 0u64;
        let mut walker = cursor.walk(Some(first_block))?.peekable();

        for block in first_block..=tip {
            let mut entries = Vec::new();

            while let Some(Ok((block_number, _))) = walker.peek() {
                if *block_number != block {
                    break;
                }
                let (_, entry) = walker.next().expect("peeked")?;
                entries.push(entry);
            }

            count += entries.len() as u64;
            writer.append_account_changeset(entries, block)?;
        }

        writer.commit()?;

        info!(target: "reth::cli", count, "AccountChangeSets migrated");
        Ok(())
    }

    fn migrate_storage_changesets<N: ProviderNodeTypes>(
        factory: &ProviderFactory<N>,
        tip: u64,
        genesis_block: u64,
    ) -> Result<()> {
        info!(target: "reth::cli", "Migrating StorageChangeSets → static files");
        let provider = factory.provider()?.disable_long_read_transaction_safety();
        let sf_provider = factory.static_file_provider();

        let mut cursor = provider.tx_ref().cursor_read::<tables::StorageChangeSets>()?;

        let first_block = provider
            .get_prune_checkpoint(PruneSegment::StorageHistory)?
            .and_then(|cp| cp.block_number)
            .map_or(0, |b| b + 1)
            .max(genesis_block);

        let mut writer = sf_provider.latest_writer(StaticFileSegment::StorageChangeSets)?;
        if first_block > genesis_block {
            if genesis_block > 0 && writer.current_block_number().is_none() {
                writer.increment_block(genesis_block)?;
            }
            writer.ensure_at_block(first_block - 1)?;
        }

        let mut count = 0u64;
        let mut walker = cursor.walk(Some((first_block, Address::ZERO).into()))?.peekable();

        for block in first_block..=tip {
            let mut entries = Vec::new();

            while let Some(Ok((key, _))) = walker.peek() {
                if key.block_number() != block {
                    break;
                }
                let (key, entry) = walker.next().expect("peeked")?;
                entries.push(StorageBeforeTx {
                    address: key.address(),
                    key: entry.key,
                    value: entry.value,
                });
            }

            count += entries.len() as u64;
            writer.append_storage_changeset(entries, block)?;
        }

        writer.commit()?;

        info!(target: "reth::cli", count, "StorageChangeSets migrated");
        Ok(())
    }

    fn migrate_receipts<N: ProviderNodeTypes>(
        factory: &ProviderFactory<N>,
        tip: u64,
        genesis_block: u64,
    ) -> Result<()>
    where
        N::Primitives: reth_primitives_traits::NodePrimitives<
            Receipt: reth_db_api::table::Value + reth_codecs::Compact,
        >,
    {
        let provider = factory.provider()?;
        if !provider.prune_modes_ref().receipts_log_filter.is_empty() {
            info!(
                target: "reth::cli",
                "Receipt log filter pruning is enabled, keeping receipts in MDBX"
            );
            return Ok(());
        }
        drop(provider);

        let sf_provider = factory.static_file_provider();
        let existing = sf_provider.get_highest_static_file_block(StaticFileSegment::Receipts);

        if existing.is_some_and(|b| b >= tip) {
            info!(target: "reth::cli", "Receipts already in static files, skipping");
            return Ok(());
        }

        info!(target: "reth::cli", "Migrating Receipts → static files");

        let provider = factory.provider()?.disable_long_read_transaction_safety();
        let prune_start = provider
            .get_prune_checkpoint(PruneSegment::Receipts)?
            .and_then(|cp| cp.block_number)
            .map_or(0, |b| b + 1);
        let first_block = prune_start.max(existing.map_or(0, |b| b + 1)).max(genesis_block);

        if first_block > genesis_block {
            let mut writer = sf_provider.latest_writer(StaticFileSegment::Receipts)?;
            if genesis_block > 0 && writer.current_block_number().is_none() {
                writer.increment_block(genesis_block)?;
            }
            writer.ensure_at_block(first_block - 1)?;
            writer.commit()?;
        }

        let before = sf_provider
            .get_highest_static_file_tx(StaticFileSegment::Receipts)
            .map_or(0, |tx| tx + 1);

        let block_range = first_block..=tip;

        let segment = reth_static_file::segments::Receipts;
        reth_static_file::segments::Segment::copy_to_static_files(&segment, provider, block_range)?;

        sf_provider.commit()?;

        let after = sf_provider
            .get_highest_static_file_tx(StaticFileSegment::Receipts)
            .map_or(0, |tx| tx + 1);
        let count = after - before;
        info!(target: "reth::cli", count, "Receipts migrated");
        Ok(())
    }

    /// Clears tables that can be recomputed by the pipeline and resets their
    /// stage checkpoints.
    fn clear_recomputable_tables<N: ProviderNodeTypes>(
        factory: &ProviderFactory<N>,
        genesis_block: u64,
    ) -> Result<()> {
        info!(target: "reth::cli", "Clearing recomputable MDBX tables");
        let db = factory.db_ref();

        macro_rules! clear_table {
            ($table:ty) => {{
                let tx = db.tx_mut()?;
                tx.clear::<$table>()?;
                tx.commit()?;
                info!(target: "reth::cli", table = <$table as Table>::NAME, "Cleared");
            }};
        }

        // Migrated changeset tables (now in static files)
        clear_table!(tables::AccountChangeSets);
        clear_table!(tables::StorageChangeSets);

        // Senders — rebuilt by SenderRecovery
        clear_table!(tables::TransactionSenders);

        // Indices — rebuilt by TransactionLookup / IndexAccountHistory / IndexStorageHistory
        clear_table!(tables::TransactionHashNumbers);
        clear_table!(tables::AccountsHistory);
        clear_table!(tables::StoragesHistory);

        // Plain state — superseded by hashed state in v2
        clear_table!(tables::PlainAccountState);
        clear_table!(tables::PlainStorageState);

        // Trie — rebuilt by MerkleExecute
        clear_table!(tables::AccountsTrie);
        clear_table!(tables::StoragesTrie);

        // Reset stage checkpoints so the pipeline rebuilds everything.
        // For non-zero genesis chains, set checkpoint to `genesis_block - 1`
        // so the next block the stage processes is `genesis_block` — the
        // first block with actual data. Resetting to 0 makes stages start
        // at block 1 and they fail with "append at block 0 but expected
        // block <genesis>" because the cleared tables' append invariant is
        // anchored to the genesis block. `saturating_sub(1)` keeps zero-genesis
        // chains at checkpoint 0 (backward compatible).
        let reset_to = genesis_block.saturating_sub(1);
        info!(target: "reth::cli", reset_to, "Resetting stage checkpoints");
        let provider_rw = factory.database_provider_rw()?;
        for stage in [
            StageId::SenderRecovery,
            StageId::TransactionLookup,
            StageId::IndexAccountHistory,
            StageId::IndexStorageHistory,
            StageId::MerkleExecute,
            StageId::MerkleUnwind,
        ] {
            provider_rw.save_stage_checkpoint(stage, StageCheckpoint::new(reset_to))?;
            info!(target: "reth::cli", %stage, reset_to, "Checkpoint reset");
        }
        provider_rw.save_stage_checkpoint_progress(StageId::MerkleExecute, vec![])?;
        provider_rw.commit()?;

        info!(target: "reth::cli", "Recomputable tables cleared");
        Ok(())
    }

    /// Creates a compacted copy of the MDBX database next to the original.
    fn compact_mdbx(db: &mdbx::DatabaseEnv) -> Result<()> {
        let db_path = db.path();
        let compact_path = db_path.with_file_name("db_compact");

        std::fs::create_dir_all(&compact_path)?;

        info!(target: "reth::cli", ?db_path, ?compact_path, "Compacting MDBX database");

        let compact_dest = compact_path.join("mdbx.dat");
        let dest_cstr = std::ffi::CString::new(
            compact_dest.to_str().ok_or_else(|| eyre::eyre!("compact path must be valid UTF-8"))?,
        )?;

        let flags = ffi::MDBX_CP_COMPACT | ffi::MDBX_CP_FORCE_DYNAMIC_SIZE;

        let rc = db.with_raw_env_ptr(|env_ptr| unsafe {
            ffi::mdbx_env_copy(env_ptr, dest_cstr.as_ptr(), flags)
        });

        if rc != 0 {
            eyre::bail!("mdbx_env_copy failed with error code {rc}: {}", unsafe {
                std::ffi::CStr::from_ptr(ffi::mdbx_strerror(rc)).to_string_lossy()
            });
        }

        info!(target: "reth::cli", "MDBX compaction complete");
        Ok(())
    }

    /// Atomically swaps the original MDBX database with the compacted copy.
    fn swap_compacted_db(db_path: &std::path::Path, compact_path: &std::path::Path) -> Result<()> {
        let backup_path = db_path.with_file_name("db_pre_compact");

        info!(target: "reth::cli", ?db_path, ?compact_path, "Swapping compacted database");

        std::fs::rename(db_path, &backup_path)?;

        if let Err(e) = std::fs::rename(compact_path, db_path) {
            // Best-effort rollback; if even this fails the original DB is at backup_path.
            let _ = std::fs::rename(&backup_path, db_path);
            return Err(e.into());
        }

        std::fs::remove_dir_all(&backup_path)?;

        info!(target: "reth::cli", "Database compaction swap complete");
        Ok(())
    }
}
