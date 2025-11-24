#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

use clap::Parser;
use eyre::Result;
use reth::version::{default_reth_version_metadata, try_init_version_metadata, RethCliVersionConsts};
use reth_cli_util::sigsegv_handler;
use reth_optimism_cli::chainspec::OpChainSpecParser;
use reth_optimism_node::OpNode;
use tracing::info;

mod export;
use export::ExportCommand;

pub const XLAYER_RETH_CLIENT_VERSION: &str = concat!("xlayer/v", env!("CARGO_PKG_VERSION"));

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
    try_init_version_metadata(RethCliVersionConsts {
        name_client: "XLayer Reth Export".to_string().into(),
        cargo_pkg_version: format!(
            "{}/{}",
            default_version_metadata.cargo_pkg_version,
            env!("CARGO_PKG_VERSION")
        )
        .into(),
        p2p_client_version: format!(
            "{}/{}",
            default_version_metadata.p2p_client_version,
            XLAYER_RETH_CLIENT_VERSION
        )
        .into(),
        extra_data: format!(
            "{}/{}",
            default_version_metadata.extra_data,
            XLAYER_RETH_CLIENT_VERSION
        )
        .into(),
        ..default_version_metadata
    })
    .unwrap();

    // Initialize tracing/logging
    reth_cli_util::sigsegv_handler::install();

    info!(target: "xlayer::export", "XLayer Reth Export starting");

    let cmd = ExportCommand::<OpChainSpecParser>::parse();

    cmd.execute::<OpNode>().await
}

