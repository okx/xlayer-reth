use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_primitives::{Address, BlockNumber};
use clap::Parser;
use eyre::Result;
use rayon::prelude::*;
use reth_cli_commands::common::CliNodeTypes;
use reth_db::{cursor::DbCursorRO, tables, transaction::DbTx, DatabaseEnv};
use reth_db_api::models::{AccountBeforeTx};
use reth_provider::{
    BlockBodyIndicesProvider, BlockNumReader, DBProvider, MetadataWriter, ProviderFactory,
    StaticFileProviderFactory, StaticFileWriter, TransactionsProvider,
};
use reth_static_file_types::StaticFileSegment;

pub(crate) fn migrate_to_static_files<N: CliNodeTypes>(
    provider_factory: &ProviderFactory<
        reth_node_builder::NodeTypesWithDBAdapter<N, DatabaseEnv>,
    >,
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

    segments.into_par_iter().try_for_each(|segment| {
        migrate_segment::<N>(provider_factory, segment, to_block)
    })?;

    Ok(())
}

pub(crate) fn migrate_segment<N: CliNodeTypes>(
        provider_factory: &ProviderFactory<
            reth_node_builder::NodeTypesWithDBAdapter<N, DatabaseEnv>,
        >,
        segment: StaticFileSegment,
        to_block: BlockNumber,
) -> Result<()> {

    Ok(())
}
