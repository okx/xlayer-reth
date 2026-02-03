mod command;
mod migrate;
mod progress;
mod chainspec_parser;
mod xlayer_mainnet;

use clap::Parser;

use reth_optimism_node::OpNode;
use reth_tracing::{RethTracer, Tracer};

use crate::chainspec_parser::XLayerChainSpecParser;

#[tokio::main]
async fn main() {
    let _ = RethTracer::new().init().expect("Failed to initialize tracing");

    if let Err(err) = command::Command::<XLayerChainSpecParser>::parse().execute::<OpNode>().await {
        eprintln!("Migration failed: {}", err);
        std::process::exit(1);
    }
}
