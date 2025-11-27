#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

use clap::Parser;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_consensus::OpBeaconConsensus;
use reth_optimism_evm::OpExecutorProvider;
use reth_optimism_node::OpNode;
use reth_tracing::{RethTracer, Tracer};
use std::{process::ExitCode, sync::Arc};
use tracing::{error, info};
use xlayer_chainspec::XLayerChainSpecParser;

mod import;
use import::ImportCommand;

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[tokio::main]
async fn main() -> ExitCode {
    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    // Initialize tracing
    let _guard = RethTracer::new().init().expect("Failed to initialize tracing");

    // Log starting message
    info!(target: "xlayer::import", "XLayer Reth Import starting");

    let components = |spec: Arc<OpChainSpec>| {
        (OpExecutorProvider::optimism(spec.clone()), Arc::new(OpBeaconConsensus::new(spec)))
    };

    // Parse and execute command
    let cmd = ImportCommand::<XLayerChainSpecParser>::parse();

    match cmd.execute::<OpNode, _>(components).await {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            error!(target: "xlayer::import", "Error: {:#?}", e);
            ExitCode::FAILURE
        }
    }
}
