#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

use clap::Parser;
use eyre::Result;
use reth::version::default_reth_version_metadata;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_cli::chainspec::OpChainSpecParser;
use reth_optimism_consensus::OpBeaconConsensus;
use reth_optimism_evm::OpExecutorProvider;
use reth_optimism_node::OpNode;
use reth_tracing::{RethTracer, Tracer};
use std::sync::Arc;
use tracing::info;
use xlayer_reth_utils::version::init_version_metadata;

mod import;
use import::ImportCommand;

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[tokio::main]
async fn main() -> Result<()> {
    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    // Initialize tracing
    let _guard = RethTracer::new().init()?;

    info!(target: "xlayer::import", "XLayer Reth Import starting");

    let default_version_metadata = default_reth_version_metadata();
    init_version_metadata(default_version_metadata).expect("Unable to init version metadata");

    let components = |spec: Arc<OpChainSpec>| {
        (OpExecutorProvider::optimism(spec.clone()), Arc::new(OpBeaconConsensus::new(spec)))
    };

    let cmd = ImportCommand::<OpChainSpecParser>::parse();

    cmd.execute::<OpNode, _>(components)
    .await
}
