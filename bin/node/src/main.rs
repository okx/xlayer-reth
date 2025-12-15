#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

mod args_xlayer;

use std::path::Path;
use std::sync::Arc;

use args_xlayer::{ApolloArgs, XLayerArgs};
use clap::Parser;
use tracing::{error, info};

use op_rbuilder::{
    args::OpRbuilderArgs,
    builders::{BuilderConfig, FlashblocksServiceBuilder},
};
use reth::{
    builder::{EngineNodeLauncher, Node, NodeHandle, TreeConfig},
    providers::providers::BlockchainProvider,
    version::{default_reth_version_metadata, try_init_version_metadata, RethCliVersionConsts},
};
use reth_optimism_cli::Cli;
use reth_optimism_node::OpNode;

use xlayer_apollo::{ApolloConfig, ApolloService};
use xlayer_chainspec::XLayerChainSpecParser;
use xlayer_innertx::{
    db_utils::initialize_inner_tx_db,
    rpc_utils::{XlayerInnerTxExt, XlayerInnerTxExtApiServer},
    subscriber_utils::initialize_innertx_replay,
};
use xlayer_legacy_rpc::{layer::LegacyRpcRouterLayer, LegacyRpcRouterConfig};
use xlayer_rpc::xlayer_ext::{XlayerRpcExt, XlayerRpcExtApiServer};

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Rollup")]
struct Args {
    /// Upstream rollup args + flashblock specific args
    #[command(flatten)]
    pub node_args: OpRbuilderArgs,

    /// X Layer specific configuration
    #[command(flatten)]
    pub xlayer_args: XLayerArgs,
}

pub const XLAYER_RETH_CLIENT_VERSION: &str = concat!("xlayer/v", env!("CARGO_PKG_VERSION"));

fn init_version_metadata() {
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
}

fn main() {
    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    // Initialize version metadata
    init_version_metadata();

    XLayerArgs::validate_init_command();

    Cli::<XLayerChainSpecParser, Args>::parse()
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

            let op_node = OpNode::new(args.node_args.rollup_args.clone());

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

            let genesis_block = builder.config().chain.genesis().number.unwrap_or_default();
            info!("XLayer genesis block = {}", genesis_block);

            let legacy_config = LegacyRpcRouterConfig {
                enabled: args.xlayer_args.legacy.legacy_rpc_url.is_some(),
                legacy_endpoint: args.xlayer_args.legacy.legacy_rpc_url.unwrap_or_default(),
                cutoff_block: genesis_block,
                timeout: args.xlayer_args.legacy.legacy_rpc_timeout,
            };

            // Build add-ons with RPC middleware
            // If not enabled, the layer will not do any re-routing.
            let add_ons = op_node.add_ons()
                .with_rpc_middleware(LegacyRpcRouterLayer::new(legacy_config));

            if args.xlayer_args.apollo.enabled {
                run_apollo(&args.xlayer_args.apollo).await;
            }

            let NodeHandle { node: _node, node_exit_future } = if args.node_args.flashblocks.enabled {
                let builder_config = BuilderConfig::try_from(args.node_args.clone())
                    .expect("Failed to convert builder args to builder config");

                builder
                    .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
                    .with_components(op_node.components().payload(FlashblocksServiceBuilder(builder_config)))
                    .with_add_ons(add_ons)
                    .on_component_initialized(move |_ctx| {
                        // TODO: Initialize XLayer components here
                        // - Inner transaction tracking
                        Ok(())
                    })
                    .extend_rpc_modules(move |ctx| {
                        let new_op_eth_api = Arc::new(ctx.registry.eth_api().clone());
                        if args.xlayer_args.enable_inner_tx {
                            // Initialize inner tx replay handler (uses canonical_state_stream)
                            // Note: This only processes real-time blocks, NOT synced blocks from Pipeline
                            initialize_innertx_replay(ctx.node());
                            info!(target: "reth::cli", "xlayer inner tx replay initialized (canonical_state_stream mode)");

                            // Register inner tx RPC
                            let custom_rpc = XlayerInnerTxExt { backend: new_op_eth_api.clone() };
                            ctx.modules.merge_configured(custom_rpc.into_rpc())?;
                            info!(target: "reth::cli", "xlayer innertx rpc enabled");
                        }

                        // Register XLayer RPC
                        let xlayer_rpc = XlayerRpcExt { backend: new_op_eth_api };
                        ctx.modules.merge_configured(xlayer_rpc.into_rpc())?;
                        info!(target: "reth::cli", "xlayer rpc extension enabled");

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
                    .await?
            } else {
                builder
                    .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
                    .with_components(op_node.components())
                    .with_add_ons(add_ons)
                    .on_component_initialized(move |_ctx| {
                        // TODO: Initialize XLayer components here
                        // - Inner transaction tracking
                        Ok(())
                    })
                    .extend_rpc_modules(move |ctx| {
                        let new_op_eth_api = Arc::new(ctx.registry.eth_api().clone());
                        if args.xlayer_args.enable_inner_tx {
                            // Initialize inner tx replay handler (uses canonical_state_stream)
                            // Note: This only processes real-time blocks, NOT synced blocks from Pipeline
                            initialize_innertx_replay(ctx.node());
                            info!(target: "reth::cli", "xlayer inner tx replay initialized (canonical_state_stream mode)");

                            // Register inner tx RPC
                            let custom_rpc = XlayerInnerTxExt { backend: new_op_eth_api.clone() };
                            ctx.modules.merge_configured(custom_rpc.into_rpc())?;
                            info!(target: "reth::cli", "xlayer innertx rpc enabled");
                        }

                        // Register XLayer RPC
                        let xlayer_rpc = XlayerRpcExt { backend: new_op_eth_api };
                        ctx.modules.merge_configured(xlayer_rpc.into_rpc())?;
                        info!(target: "reth::cli", "xlayer rpc extension enabled");

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
                    .await?
            };

            node_exit_future.await
        })
        .unwrap();
}

async fn run_apollo(apollo_args: &ApolloArgs) {
    tracing::info!(target: "xlayer-apollo", "[Apollo] Apollo enabled: {:?}", apollo_args.enabled);
    tracing::info!(target: "xlayer-apollo", "[Apollo] Apollo app ID: {:?}", apollo_args.apollo_app_id);
    tracing::info!(target: "xlayer-apollo", "[Apollo] Apollo IP: {:?}", apollo_args.apollo_ip);
    tracing::info!(target: "xlayer-apollo", "[Apollo] Apollo cluster: {:?}", apollo_args.apollo_cluster);
    tracing::info!(target: "xlayer-apollo", "[Apollo] Apollo namespace: {:?}", apollo_args.apollo_namespace);

    // Create Apollo config from args
    let apollo_config = ApolloConfig {
        meta_server: vec![apollo_args.apollo_ip.to_string()],
        app_id: apollo_args.apollo_app_id.to_string(),
        cluster_name: apollo_args.apollo_cluster.to_string(),
        namespaces: Some(apollo_args.apollo_namespace.split(',').map(|s| s.to_string()).collect()),
        secret: None,
    };

    tracing::info!(target: "xlayer-apollo", "[Apollo] Creating Apollo config");

    // Initialize Apollo singleton
    if let Err(e) = ApolloService::try_initialize(apollo_config).await {
        tracing::error!(target: "xlayer-apollo", "[Apollo] Failed to initialize Apollo: {:?}; Proceeding with node launch without Apollo", e);
    } else {
        tracing::info!(target: "xlayer-apollo", "[Apollo] Apollo initialized successfully")
    }
}
