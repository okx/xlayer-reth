#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

use clap::Parser;
use eyre::Result;
use reth::version::default_reth_version_metadata;
use reth_cli_util::sigsegv_handler;
use reth_optimism_cli::chainspec::OpChainSpecParser;
use reth_optimism_node::OpNode;
use reth_tracing::{RethTracer, Tracer};
use tracing::info;
use xlayer_reth_utils::version::init_version_metadata;

mod export;
use export::ExportCommand;

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[tokio::main]
async fn main() -> Result<()> {
    sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    let default_version_metadata = default_reth_version_metadata();
    init_version_metadata(default_version_metadata).expect("Unable to init version metadata");

    // Initialize tracing
    let _guard = RethTracer::new().init()?;

    info!(target: "xlayer::export", "XLayer Reth Export starting");

    let cmd = ExportCommand::<OpChainSpecParser>::parse();

    cmd.execute::<OpNode>().await
}
