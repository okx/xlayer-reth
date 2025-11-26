#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

use std::process::ExitCode;

use clap::Parser;
use reth::version::default_reth_version_metadata;
use reth_cli_util::sigsegv_handler;
use reth_optimism_node::OpNode;
use reth_tracing::{RethTracer, Tracer};
use tracing::{error, info};
use xlayer_chainspec::XLayerChainSpecParser;
use xlayer_reth_utils::version::init_version_metadata;

mod export;
use export::ExportCommand;

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[tokio::main]
async fn main() -> ExitCode {
    sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    // Initialize version metadata
    let default_version_metadata = default_reth_version_metadata();
    init_version_metadata(default_version_metadata).expect("Unable to init version metadata");

    // Initialize tracing
    let _guard = RethTracer::new().init().expect("Failed to initialize tracing");

    info!(target: "xlayer::export", "XLayer Reth Export starting");

    // Parse and execute command
    let cmd = ExportCommand::<XLayerChainSpecParser>::parse();
    let mut has_error = false;
    cmd.execute::<OpNode>().await.map_err(|e| {
        error!(target: "xlayer::export", "Error: {:#?}", e);
        has_error = true;
    }).unwrap_or(());

    if has_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
