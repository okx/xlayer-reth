use std::time::Instant;

use alloy_primitives::{Address, BlockNumber};
use eyre::Result;
use rayon::prelude::*;
use tracing::info;

use crate::progress::{log_progress, log_rocksdb_progress, PROGRESS_LOG_INTERVAL};
use reth_cli_commands::common::CliNodeTypes;
use reth_db::{cursor::DbCursorRO, tables, transaction::DbTx, DatabaseEnv};
use reth_db_api::models::{AccountBeforeTx, StorageBeforeTx};
use reth_provider::{
    BlockBodyIndicesProvider, DBProvider, ProviderFactory, StaticFileProviderFactory,
    StaticFileWriter, TransactionsProvider,
};
use reth_static_file_types::StaticFileSegment;

pub(crate) fn migrate_to_static_files<N: CliNodeTypes>(
    provider_factory: &ProviderFactory<reth_node_builder::NodeTypesWithDBAdapter<N, DatabaseEnv>>,
    to_block: BlockNumber,
    can_migrate_receipts: bool,
) -> Result<()> {
    let mut segments = vec![
        StaticFileSegment::TransactionSenders,
        StaticFileSegment::AccountChangeSets,
        StaticFileSegment::StorageChangeSets,
    ];
    if can_migrate_receipts {
        segments.push(StaticFileSegment::Receipts);
    }

    segments
        .into_par_iter()
        .try_for_each(|segment| migrate_segment::<N>(provider_factory, segment, to_block))?;

    Ok(())
}

pub(crate) fn migrate_segment<N: CliNodeTypes>(
    provider_factory: &ProviderFactory<reth_node_builder::NodeTypesWithDBAdapter<N, DatabaseEnv>>,
    segment: StaticFileSegment,
    to_block: BlockNumber,
) -> Result<()> {
    let static_file_provider = provider_factory.static_file_provider();
    let provider = provider_factory.provider()?.disable_long_read_transaction_safety();

    let highest = static_file_provider.get_highest_static_file_block(segment).unwrap_or(0);
    if highest >= to_block {
        info!(target: "reth::cli", ?segment, "Already up to date");
        return Ok(());
    }

    let start = highest.saturating_add(1);
    let total_blocks = to_block.saturating_sub(start) + 1;
    info!(target: "reth::cli", ?segment, from = start, to = to_block, total_blocks, "Migrating");

    let mut writer = static_file_provider.latest_writer(segment)?;
    let segment_start = Instant::now();
    let mut last_log = Instant::now();
    let mut blocks_processed = 0u64;
    let mut entries_processed = 0u64;

    match segment {
        StaticFileSegment::TransactionSenders => {
            for block in start..=to_block {
                if let Some(body) = provider.block_body_indices(block)? {
                    let senders = provider.senders_by_tx_range(
                        body.first_tx_num..body.first_tx_num + body.tx_count,
                    )?;
                    for (i, sender) in senders.into_iter().enumerate() {
                        writer.append_transaction_sender(body.first_tx_num + i as u64, &sender)?;
                        entries_processed += 1;
                    }
                }
                blocks_processed += 1;
                if last_log.elapsed() >= PROGRESS_LOG_INTERVAL {
                    log_progress(
                        segment,
                        blocks_processed,
                        total_blocks,
                        entries_processed,
                        segment_start.elapsed(),
                    );
                    last_log = Instant::now();
                }
            }
        }

        StaticFileSegment::AccountChangeSets => {
            let tx = provider.tx_ref();
            let mut cursor = tx.cursor_dup_read::<tables::AccountChangeSets>()?;

            let mut current_block = start;
            let mut block_changesets: Vec<AccountBeforeTx> = Vec::new();

            for result in cursor.walk_range(start..=to_block)? {
                let (block, changeset) = result?;

                if block != current_block {
                    if !block_changesets.is_empty() {
                        writer.append_account_changeset(
                            std::mem::take(&mut block_changesets),
                            current_block,
                        )?;
                    }
                    // Advance writer to handle gaps in changeset data
                    writer.ensure_at_block(block.saturating_sub(1))?;
                    blocks_processed += block - current_block;
                    current_block = block;

                    if last_log.elapsed() >= PROGRESS_LOG_INTERVAL {
                        log_progress(
                            segment,
                            blocks_processed,
                            total_blocks,
                            entries_processed,
                            segment_start.elapsed(),
                        );
                        last_log = Instant::now();
                    }
                }
                block_changesets.push(changeset);
                entries_processed += 1;
            }

            if !block_changesets.is_empty() {
                writer.append_account_changeset(block_changesets, current_block)?;
            }
        }

        StaticFileSegment::StorageChangeSets => {
            let tx = provider.tx_ref();
            let mut cursor = tx.cursor_dup_read::<tables::StorageChangeSets>()?;
            let start_key = reth_db_api::models::BlockNumberAddress((start, Default::default()));
            let end_key =
                reth_db_api::models::BlockNumberAddress((to_block, Address::new([0xff; 20])));

            let mut current_block = start;
            let mut block_changesets: Vec<StorageBeforeTx> = Vec::new();

            for result in cursor.walk_range(start_key..=end_key)? {
                let (key, entry) = result?;
                let block = key.block_number();

                if block != current_block {
                    if !block_changesets.is_empty() {
                        writer.append_storage_changeset(
                            std::mem::take(&mut block_changesets),
                            current_block,
                        )?;
                    }
                    // Advance writer to handle gaps in changeset data
                    writer.ensure_at_block(block.saturating_sub(1))?;
                    blocks_processed += block - current_block;
                    current_block = block;

                    if last_log.elapsed() >= PROGRESS_LOG_INTERVAL {
                        log_progress(
                            segment,
                            blocks_processed,
                            total_blocks,
                            entries_processed,
                            segment_start.elapsed(),
                        );
                        last_log = Instant::now();
                    }
                }
                block_changesets.push(StorageBeforeTx {
                    address: key.address(),
                    key: entry.key,
                    value: entry.value,
                });
                entries_processed += 1;
            }

            if !block_changesets.is_empty() {
                writer.append_storage_changeset(block_changesets, current_block)?;
            }
        }

        StaticFileSegment::Receipts => {
            let tx = provider.tx_ref();
            let mut cursor = tx.cursor_read::<tables::Receipts<_>>()?;
            for block in start..=to_block {
                if let Some(body) = provider.block_body_indices(block)? {
                    for tx_num in body.first_tx_num..body.first_tx_num + body.tx_count {
                        if let Some(receipt) = cursor.seek_exact(tx_num)?.map(|(_, r)| r) {
                            writer.append_receipt(tx_num, &receipt)?;
                            entries_processed += 1;
                        }
                    }
                }
                blocks_processed += 1;
                if last_log.elapsed() >= PROGRESS_LOG_INTERVAL {
                    log_progress(
                        segment,
                        blocks_processed,
                        total_blocks,
                        entries_processed,
                        segment_start.elapsed(),
                    );
                    last_log = Instant::now();
                }
            }
        }

        _ => (),
    }

    writer.commit()?;

    let elapsed = segment_start.elapsed();
    let rate = if elapsed.as_secs() > 0 {
        entries_processed / elapsed.as_secs()
    } else {
        entries_processed
    };
    info!(
        target: "reth::cli",
        ?segment,
        entries = entries_processed,
        elapsed_secs = elapsed.as_secs(),
        rate_per_sec = rate,
        "Done"
    );

    Ok(())
}

pub(crate) fn migrate_to_rocksdb<N: CliNodeTypes>(
    provider_factory: &ProviderFactory<reth_node_builder::NodeTypesWithDBAdapter<N, DatabaseEnv>>,
    batch_size: u64,
) -> Result<()> {
    use reth_db_api::table::Table;

    [
        tables::TransactionHashNumbers::NAME,
        tables::AccountsHistory::NAME,
        tables::StoragesHistory::NAME,
    ]
    .into_par_iter()
    .try_for_each(|table| migrate_rocksdb_table::<N>(provider_factory, table, batch_size))?;

    Ok(())
}

pub(crate) fn migrate_rocksdb_table<N: CliNodeTypes>(
    provider_factory: &ProviderFactory<reth_node_builder::NodeTypesWithDBAdapter<N, DatabaseEnv>>,
    table: &'static str,
    batch_size: u64,
) -> Result<()> {
    use reth_db_api::table::Table;
    use reth_provider::RocksDBProviderFactory;

    let provider = provider_factory.provider()?.disable_long_read_transaction_safety();
    let rocksdb = provider_factory.rocksdb_provider();
    let tx = provider.tx_ref();

    info!(target: "reth::cli", table, "Migrating");

    let table_start = Instant::now();
    let mut last_log = Instant::now();

    let count = match table {
        tables::TransactionHashNumbers::NAME => {
            let mut cursor = tx.cursor_read::<tables::TransactionHashNumbers>()?;
            let mut batch = rocksdb.batch_with_auto_commit();
            let mut count = 0u64;

            for result in cursor.walk(None)? {
                let (hash, tx_num) = result?;
                batch.put::<tables::TransactionHashNumbers>(hash, &tx_num)?;
                count += 1;

                if count.is_multiple_of(batch_size) && last_log.elapsed() >= PROGRESS_LOG_INTERVAL {
                    log_rocksdb_progress(table, count, table_start.elapsed());
                    last_log = Instant::now();
                }
            }

            batch.commit()?;
            count
        }
        tables::AccountsHistory::NAME => {
            let mut cursor = tx.cursor_read::<tables::AccountsHistory>()?;
            let mut batch = rocksdb.batch_with_auto_commit();
            let mut count = 0u64;

            for result in cursor.walk(None)? {
                let (key, value) = result?;
                batch.put::<tables::AccountsHistory>(key, &value)?;
                count += 1;
                if count.is_multiple_of(batch_size) {
                    batch.commit()?;
                    batch = rocksdb.batch_with_auto_commit();

                    if last_log.elapsed() >= PROGRESS_LOG_INTERVAL {
                        log_rocksdb_progress(table, count, table_start.elapsed());
                        last_log = Instant::now();
                    }
                }
            }

            batch.commit()?;
            count
        }
        tables::StoragesHistory::NAME => {
            let mut cursor = tx.cursor_read::<tables::StoragesHistory>()?;
            let mut batch = rocksdb.batch_with_auto_commit();
            let mut count = 0u64;

            for result in cursor.walk(None)? {
                let (key, value) = result?;
                batch.put::<tables::StoragesHistory>(key, &value)?;
                count += 1;
                if count.is_multiple_of(batch_size) {
                    batch.commit()?;
                    batch = rocksdb.batch_with_auto_commit();

                    if last_log.elapsed() >= PROGRESS_LOG_INTERVAL {
                        log_rocksdb_progress(table, count, table_start.elapsed());
                        last_log = Instant::now();
                    }
                }
            }

            batch.commit()?;
            count
        }
        _ => 0,
    };

    let elapsed = table_start.elapsed();
    let rate = if elapsed.as_secs() > 0 { count / elapsed.as_secs() } else { count };
    info!(
        target: "reth::cli",
        table,
        entries = count,
        elapsed_secs = elapsed.as_secs(),
        rate_per_sec = rate,
        "Done"
    );

    Ok(())
}
