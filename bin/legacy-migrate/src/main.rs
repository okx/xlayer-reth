mod command;
mod migrate;

use clap::Parser;

use reth_optimism_cli::{chainspec::OpChainSpecParser};
use reth_optimism_node::OpNode;
use reth_tracing::{RethTracer, Tracer};

#[tokio::main]
async fn main() {
    let _ = RethTracer::new().init().expect("Failed to initialize tracing");

    if let Err(err) = command::Command::<OpChainSpecParser>::parse().execute::<OpNode>().await {
        eprintln!("Migration failed: {}", err);
        std::process::exit(1);
    }
}
