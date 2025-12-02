//! Command that exports blocks to an RLP encoded file, similar to go-ethereum's export command.
//!
//! This implementation:
//! - Reads blocks from the database
//! - Encodes blocks to RLP format
//! - Writes to a file (supports gzip compression)
//! - Handles interrupts gracefully (Ctrl+C)

use alloy_consensus::BlockHeader;
use alloy_rlp::Encodable;
use clap::Parser;
use eyre::{eyre, Result, WrapErr};
use rayon::iter::IntoParallelIterator;
use rayon::iter::ParallelIterator;
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs};
use reth_node_core::version::version_metadata;
use reth_optimism_chainspec::OpChainSpec;
use reth_provider::BlockNumReader;
use reth_storage_api::BlockReader;
use std::{
    fs::File,
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tracing::{error, info, warn};

/// Exports blocks to an RLP encoded file, similar to go-ethereum's export command.
#[derive(Debug, Parser)]
pub struct ExportCommand<C: ChainSpecParser> {
    #[command(flatten)]
    env: EnvironmentArgs<C>,

    /// The path to write the exported blocks.
    ///
    /// Blocks will be RLP encoded. If the file ends with .gz, it will be gzip compressed.
    #[arg(long = "exported-data", value_name = "EXPORTED_DATA", verbatim_doc_comment)]
    output_path: PathBuf,

    /// The starting block number (inclusive).
    #[arg(long, value_name = "START_BLOCK", default_value = "0")]
    start_block: u64,

    /// The ending block number (inclusive). If not specified, exports to the latest block.
    #[arg(long, value_name = "END_BLOCK")]
    end_block: Option<u64>,

    /// Batch size for reading blocks from database.
    #[arg(long, value_name = "BATCH_SIZE", default_value = "100000")]
    batch_size: u64,
}

impl<C: ChainSpecParser<ChainSpec = OpChainSpec>> ExportCommand<C> {
    /// Execute `export` command
    pub async fn execute<N>(self) -> Result<()>
    where
        N: reth_cli_commands::common::CliNodeTypes<ChainSpec = C::ChainSpec>,
    {
        info!(target: "reth::cli", "reth {} starting", version_metadata().short_version);
        info!(target: "reth::cli", "Exporting blockchain to file: {}", self.output_path.display());

        let Environment { provider_factory, .. } = self.env.init::<N>(AccessRights::RO)?;

        // Get the latest block number from the database
        let provider = provider_factory.provider()?;
        let latest_block =
            provider.last_block_number().wrap_err("Failed to get latest block number")?;
        let end_block = self.end_block.unwrap_or(latest_block);
        if end_block > latest_block {
            return Err(eyre!(
                "End block ({}) is greater than latest block ({})",
                end_block,
                latest_block
            ));
        }

        // Get the genesis block number from the chain spec and validate the start block
        let genesis_block_number = provider.chain_spec().genesis_header().number();
        if self.start_block < genesis_block_number {
            return Err(eyre!(
                "Start block ({}) is less than genesis block ({})",
                self.start_block,
                genesis_block_number
            ));
        }
        let start_block = self.start_block;

        let total_blocks = end_block - start_block + 1;
        info!(
            target: "reth::cli",
            "Exporting blocks {} to {} ({} blocks total)",
            start_block,
            end_block,
            total_blocks
        );

        // Setup interrupt handler
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        ctrlc::set_handler(move || {
            warn!(target: "reth::cli", "Received interrupt signal, shutting down gracefully...");
            shutdown_clone.store(true, Ordering::SeqCst);
        })
        .wrap_err("Failed to set interrupt handler")?;

        // Open output file
        let output_file = File::create(&self.output_path).wrap_err_with(|| {
            format!("Failed to create output file: {}", self.output_path.display())
        })?;

        // Determine if we should use gzip compression
        let use_compression = self.output_path.extension().and_then(|s| s.to_str()) == Some("gz");

        let mut writer: Box<dyn Write> = if use_compression {
            info!(target: "reth::cli", "Using gzip compression");
            Box::new(flate2::write::GzEncoder::new(output_file, flate2::Compression::default()))
        } else {
            Box::new(output_file)
        };

        // Export blocks in batches
        let mut current_block = start_block;
        let mut exported_blocks = 0u64;

        while current_block <= end_block && !shutdown.load(Ordering::SeqCst) {
            let batch_end = std::cmp::min(current_block + self.batch_size - 1, end_block);

            match provider.block_range(current_block..=batch_end) {
                Ok(blocks) => {
                    let blocks_rlp: Vec<Vec<u8>> = blocks
                        .into_par_iter()
                        .map(|block| {
                            let mut rlp_buf = Vec::new();
                            block.encode(&mut rlp_buf);
                            rlp_buf
                        })
                        .collect();
                    let blocks_rlp_concat = blocks_rlp.concat();

                    writer.write_all(&blocks_rlp_concat).wrap_err_with(|| {
                        format!(
                            "Failed to write block range {} to {} to file",
                            current_block, batch_end
                        )
                    })?;
                }
                Err(e) => {
                    error!(target: "reth::cli", "Error: {:#?}", e);
                    return Err(eyre!(e));
                }
            }

            exported_blocks += batch_end - current_block + 1;

            // Log progress periodically
            if exported_blocks.is_multiple_of(self.batch_size) {
                let progress = (exported_blocks as f64 / total_blocks as f64) * 100.0;
                info!(
                    target: "reth::cli",
                    "Exported {} blocks ({:.2}%)",
                    exported_blocks,
                    progress
                );
            }

            current_block = batch_end + 1;
        }

        // Flush and close the writer
        writer.flush().wrap_err("Failed to flush output file")?;

        if shutdown.load(Ordering::SeqCst) {
            warn!(
                target: "reth::cli",
                "Export interrupted! Exported {}/{} blocks",
                exported_blocks,
                total_blocks
            );
            return Err(eyre!(
                "Export was interrupted. Exported {}/{} blocks",
                exported_blocks,
                total_blocks
            ));
        }

        info!(
            target: "reth::cli",
            "Export complete! Exported {} blocks to {}",
            exported_blocks,
            self.output_path.display()
        );

        Ok(())
    }
}
