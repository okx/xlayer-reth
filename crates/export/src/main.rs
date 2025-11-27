#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

use std::process::ExitCode;

use clap::Parser;
use reth_cli_util::sigsegv_handler;
use reth_optimism_node::OpNode;
use reth_tracing::{RethTracer, Tracer};
use tracing::{error, info};
use xlayer_chainspec::XLayerChainSpecParser;

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

    // Initialize tracing
    let _guard = RethTracer::new().init().expect("Failed to initialize tracing");

    info!(target: "xlayer::export", "XLayer Reth Export starting");

    // Parse and execute command
    let cmd = ExportCommand::<XLayerChainSpecParser>::parse();

    match cmd.execute::<OpNode>().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            error!(target: "xlayer::export", "Error: {:#?}", e);
            ExitCode::FAILURE
        }
    }
}
