#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

use clap::Parser;
use reth::version::{
    default_reth_version_metadata, try_init_version_metadata, RethCliVersionConsts,
};
use reth_optimism_cli::{chainspec::OpChainSpecParser, Cli};
use reth_optimism_node::{args::RollupArgs, OpNode};
use tracing::info;

pub const NODE_RETH_CLIENT_VERSION: &str = concat!("base/v", env!("CARGO_PKG_VERSION"));

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Rollup")]
pub(crate) struct Args {
    #[command(flatten)]
    /// Upstream reth args
    pub rollup_args: RollupArgs,
    // XLayer specific args we defined below.
    // ...
}

fn main() {
    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    let default_version_metadata = default_reth_version_metadata();
    try_init_version_metadata(RethCliVersionConsts {
        name_client: "XLayer Reth Node".to_string().into(),
        cargo_pkg_version: format!(
            "{}/{}",
            default_version_metadata.cargo_pkg_version,
            env!("CARGO_PKG_VERSION")
        )
        .into(),
        p2p_client_version: format!(
            "{}/{}",
            default_version_metadata.p2p_client_version, NODE_RETH_CLIENT_VERSION
        )
        .into(),
        extra_data: format!("{}/{}", default_version_metadata.extra_data, NODE_RETH_CLIENT_VERSION)
            .into(),
        ..default_version_metadata
    })
    .expect("Unable to init version metadata");

    if let Err(err) = Cli::<OpChainSpecParser, Args>::parse().run(async move |builder, args| {
        info!(message = "starting custom XLayer reth");

        let node_builder = builder.node(OpNode::new(args.rollup_args));
        let handle = node_builder.launch_with_debug_capabilities().await?;
        handle.node_exit_future.await
    }) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
