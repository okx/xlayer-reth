#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

mod args;
mod payload;

use payload::XLayerPayloadServiceBuilder;

use args::XLayerArgs;
use clap::Parser;
use std::sync::Arc;
use tracing::info;

use op_alloy_network::Optimism;
use op_rbuilder::args::OpRbuilderArgs;
use reth::rpc::eth::EthApiTypes;
use reth::{
    builder::{EngineNodeLauncher, Node, NodeHandle, TreeConfig},
    providers::providers::BlockchainProvider,
};
use reth_node_api::FullNodeComponents;
use reth_optimism_cli::Cli;
use reth_optimism_node::OpNode;
use reth_rpc_server_types::RethRpcModule;

use xlayer_chainspec::XLayerChainSpecParser;
use xlayer_flashblocks::handler::FlashblocksService;
use xlayer_flashblocks::subscription::FlashblocksPubSub;
use xlayer_legacy_rpc::{layer::LegacyRpcRouterLayer, LegacyRpcRouterConfig};
use xlayer_monitor::{start_monitor_handle, RpcMonitorLayer, XLayerMonitor};
use xlayer_rpc::xlayer_ext::{XlayerRpcExt, XlayerRpcExtApiServer};

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Rollup")]
struct Args {
    /// Upstream rollup args + flashblock specific args
    #[command(flatten)]
    pub node_args: OpRbuilderArgs,

    #[command(flatten)]
    pub xlayer_args: XLayerArgs,
}

fn main() {
    xlayer_version::init_version!();

    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    XLayerArgs::validate_init_command();

    Cli::<XLayerChainSpecParser, Args>::parse()
        .run(|builder, args| async move {
            info!(message = "starting custom X Layer node");

            // Validate X Layer configuration
            if let Err(e) = args.xlayer_args.validate() {
                eprintln!("X Layer configuration error: {e}");
                std::process::exit(1);
            }

            // Initialize global tracer if full link monitor is enabled
            if args.xlayer_args.monitor.enable {
                use std::path::PathBuf;
                use xlayer_trace_monitor::init_global_tracer;
                let output_path = PathBuf::from(&args.xlayer_args.monitor.output_path);
                init_global_tracer(true, Some(output_path));
                info!(target: "xlayer::monitor", "Global tracer initialized with output path: {}", args.xlayer_args.monitor.output_path);
            }

            let op_node = OpNode::new(args.node_args.rollup_args.clone());

            let genesis_block = builder.config().chain.genesis().number.unwrap_or_default();
            info!("X Layer genesis block = {}", genesis_block);

            // Clone xlayer_args early to avoid partial move issues
            let xlayer_args = args.xlayer_args.clone();

            let legacy_config = LegacyRpcRouterConfig {
                enabled: xlayer_args.legacy.legacy_rpc_url.is_some(),
                legacy_endpoint: xlayer_args.legacy.legacy_rpc_url.unwrap_or_default(),
                cutoff_block: genesis_block,
                timeout: xlayer_args.legacy.legacy_rpc_timeout,
            };

            // For X Layer full link monitor
            let monitor = XLayerMonitor::new(
                xlayer_args.monitor,
                args.node_args.flashblocks.enabled,
                xlayer_args.sequencer_mode,
            );

            let add_ons = op_node.add_ons().with_rpc_middleware((
                RpcMonitorLayer::new(monitor.clone()),    // Execute first
                LegacyRpcRouterLayer::new(legacy_config), // Execute second
            ));

            // Create the X Layer payload service builder
            // It handles both flashblocks and default modes internally
            let payload_builder = XLayerPayloadServiceBuilder::new(args.node_args.clone())?;

            let NodeHandle { node, node_exit_future } = builder
                .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
                .with_components(op_node.components().payload(payload_builder))
                .with_add_ons(add_ons)
                .on_component_initialized(move |_ctx| {
                    // TODO: Initialize X Layer components here
                    Ok(())
                })
                .extend_rpc_modules(move |ctx| {
                    let new_op_eth_api = Arc::new(ctx.registry.eth_api().clone());

                    // Initialize flashblocks RPC service if not in flashblocks sequencer mode
                    if !args.node_args.flashblocks.enabled {
                        if let Some(flashblock_rx) = new_op_eth_api.subscribe_received_flashblocks()
                        {
                            let service = FlashblocksService::new(
                                ctx.node().clone(),
                                flashblock_rx,
                                args.node_args.clone(),
                            )?;
                            service.spawn();
                            info!(target: "reth::cli", "xlayer flashblocks service initialized");
                        }

                        if xlayer_args.enable_flashblocks_subscription
                            && let Some(pending_blocks_rx) = new_op_eth_api.pending_block_rx()
                        {
                            let eth_pubsub = ctx.registry.eth_handlers().pubsub.clone();

                            let flashblocks_pubsub = FlashblocksPubSub::new(
                                eth_pubsub,
                                pending_blocks_rx,
                                Box::new(ctx.node().task_executor().clone()),
                                new_op_eth_api.converter().clone(),
                                xlayer_args.flashblocks_subscription_max_addresses,
                            );
                            ctx.modules.add_or_replace_if_module_configured(
                                RethRpcModule::Eth,
                                flashblocks_pubsub.into_rpc(),
                            )?;
                            info!(target: "reth::cli", "xlayer eth pubsub initialized");
                        }
                    }

                    // Register X Layer RPC
                    let xlayer_rpc = XlayerRpcExt { backend: new_op_eth_api };
                    ctx.modules.merge_configured(XlayerRpcExtApiServer::<Optimism>::into_rpc(
                        xlayer_rpc,
                    ))?;
                    info!(target: "reth::cli", "xlayer rpc extension enabled");

                    info!(message = "X Layer RPC modules initialized");
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

            // Start X Layer full link monitor handle
            start_monitor_handle(
                node.tasks(),
                monitor.clone(),
                node.provider().clone(),
                node.payload_builder_handle.clone(),
                node.add_ons_handle.engine_events.new_listener(),
            );

            node_exit_future.await
        })
        .unwrap();
}
