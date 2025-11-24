#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

use clap::Parser;
use eyre::Result;
use reth_optimism_cli::chainspec::OpChainSpecParser;
use reth_optimism_consensus::OpBeaconConsensus;
use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_node::OpNode;
use std::sync::Arc;
use tracing::info;

mod import;
use import::ImportCommand;

pub const XLAYER_RETH_CLIENT_VERSION: &str = concat!("xlayer/v", env!("CARGO_PKG_VERSION"));

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing/logging
    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    info!(target: "xlayer::import", "XLayer Reth Import starting");

    let cmd = ImportCommand::<OpChainSpecParser>::parse();

    cmd.execute::<OpNode, _>(|chain_spec| {
        let evm_config = OpEvmConfig::new(chain_spec.clone(), OpRethReceiptBuilder::default());
        let consensus = Arc::new(OpBeaconConsensus::new(chain_spec));
        (evm_config, consensus)
    })
    .await
}
