#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

mod args_xlayer;

use std::path::Path;
use std::sync::Arc;

use args_xlayer::XLayerArgs;
use clap::Parser;
use tracing::{error, info};

use reth::{
    builder::{EngineNodeLauncher, Node, NodeHandle, TreeConfig},
    providers::providers::BlockchainProvider,
    version::{default_reth_version_metadata, try_init_version_metadata, RethCliVersionConsts},
};
use reth_optimism_cli::{chainspec::OpChainSpecParser, Cli};
use reth_optimism_node::{args::RollupArgs, OpNode};

use xlayer_innertx::{
    db_utils::initialize_inner_tx_db,
    exex_utils::post_exec_exex_inner_tx,
    rpc_utils::{XlayerInnerTxExt, XlayerInnerTxExtApiServer},
};
use xlayer_rpc::xlayer_ext::{XlayerRpcExt, XlayerRpcExtApiServer};

pub const XLAYER_RETH_CLIENT_VERSION: &str = concat!("xlayer/v", env!("CARGO_PKG_VERSION"));

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Rollup")]
struct Args {
    /// Upstream rollup args
    #[command(flatten)]
    pub rollup_args: RollupArgs,

    /// X Layer specific configuration
    #[command(flatten)]
    pub xlayer_args: XLayerArgs,
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
            default_version_metadata.p2p_client_version, XLAYER_RETH_CLIENT_VERSION
        )
        .into(),
        extra_data: format!(
            "{}/{}",
            default_version_metadata.extra_data, XLAYER_RETH_CLIENT_VERSION
        )
        .into(),
        ..default_version_metadata
    })
    .expect("Unable to init version metadata");

    Cli::<OpChainSpecParser, Args>::parse()
        .run(|builder, args| async move {
            info!(message = "starting custom XLayer node");

            // Validate XLayer configuration
            if let Err(e) = args.xlayer_args.validate() {
                eprintln!("XLayer configuration error: {e}");
                std::process::exit(1);
            }

            // Log XLayer feature status
            info!(
                bridge_intercept_enabled = args.xlayer_args.intercept.enabled,
                apollo_enabled = args.xlayer_args.apollo.enabled,
                inner_tx_enabled = args.xlayer_args.enable_inner_tx,
                "XLayer features configuration"
            );

            let op_node = OpNode::new(args.rollup_args.clone());

            let data_dir = builder.config().datadir();
            if args.xlayer_args.enable_inner_tx {
                let db = data_dir.db();
                let db_path = db.parent().unwrap_or_else(|| Path::new("/")).to_str().unwrap();
                match initialize_inner_tx_db(db_path) {
                    Ok(_) => info!(target: "reth::cli", "xlayer db initialize_inner_tx_db"),
                    Err(e) => {
                        error!(target: "reth::cli", "xlayer db failed to initialize_inner_tx_db {:#?}", e)
                    }
                }
            }

            let NodeHandle { node: _node, node_exit_future } = builder
                .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
                .with_components(op_node.components())
                .with_add_ons(op_node.add_ons())
                .on_component_initialized(move |_ctx| {
                    // TODO: Initialize XLayer components here
                    // - Bridge intercept configuration
                    // - Apollo configuration
                    // - Inner transaction tracking
                    Ok(())
                })
                .install_exex_if(
                    args.xlayer_args.enable_inner_tx,
                    "xlayer-innertx",
                    move |ctx| async move { Ok(post_exec_exex_inner_tx(ctx)) },
                )
                .extend_rpc_modules(move |ctx| {
                    // TODO: Add XLayer RPC extensions here
                    // - Bridge intercept RPC methods
                    // - Apollo RPC methods
                    // - Inner transaction RPC methods

                    // TODO: implement legacy rpc routing for innertx rpc
                    let new_op_eth_api = ctx.registry.eth_api().clone();
                    let custom_rpc = XlayerInnerTxExt { backend: Arc::new(new_op_eth_api.clone()) };
                    ctx.modules.merge_configured(custom_rpc.into_rpc())?;
                    info!(target:"reth::cli", "xlayer innertx rpc enabled");

                    // Register XLayer RPC extension for eth_minGasPrice
                    let xlayer_rpc = XlayerRpcExt { backend: Arc::new(new_op_eth_api) };
                    ctx.modules.merge_configured(xlayer_rpc.into_rpc())?;
                    info!(target:"reth::cli", "xlayer rpc extension enabled (eth_minGasPrice)");

                    info!(message = "XLayer RPC modules initialized");
                    Ok(())
                })
                .launch_with_fn(|builder| {
                    let engine_tree_config = TreeConfig::default()
                        .with_persistence_threshold(builder.config().engine.persistence_threshold)
                        .with_memory_block_buffer_target(
                            builder.config().engine.memory_block_buffer_target,
                        );

                    let launcher = EngineNodeLauncher::new(
                        builder.task_executor().clone(),
                        builder.config().datadir(),
                        engine_tree_config,
                    );

                    builder.launch_with(launcher)
                })
                .await?;

            node_exit_future.await
        })
        .unwrap();
}
