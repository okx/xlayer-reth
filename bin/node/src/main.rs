#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

mod args;
mod payload;

use payload::XLayerPayloadServiceBuilder;

use args::XLayerArgs;
use clap::Parser;
use either::Either;
use std::sync::{Arc, OnceLock};
use tracing::info;

use reth::rpc::eth::EthApiTypes;
use reth::{
    builder::{DebugNodeLauncher, EngineNodeLauncher, Node, NodeHandle, TreeConfig},
    providers::providers::BlockchainProvider,
};
use reth_chainspec::ChainSpecProvider;
use reth_optimism_cli::Cli;
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::{args::RollupArgs, OpNode};
use reth_provider::CanonStateSubscriptions;
use reth_rpc_server_types::RethRpcModule;
use reth_tasks::Runtime;

use xlayer_chainspec::XLayerChainSpecParser;
use xlayer_flashblocks::{
    FlashblockStateCache, FlashblocksPersistCtx, FlashblocksPubSub, FlashblocksRpcCtx,
    FlashblocksRpcService, WsFlashBlockStream,
};
use xlayer_legacy_rpc::{layer::LegacyRpcRouterLayer, LegacyRpcRouterConfig};
use xlayer_monitor::{start_monitor_handle, RpcMonitorLayer, XLayerMonitor};
use xlayer_rpc::{
    DefaultRpcExt, DefaultRpcExtApiServer, FlashblocksEthApiExt, FlashblocksEthApiOverrideServer,
};

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Rollup")]
struct Args {
    /// Upstream rollup args
    #[command(flatten)]
    pub rollup_args: RollupArgs,

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

            let op_node = OpNode::new(args.rollup_args.clone());

            let genesis_block = builder.config().chain.genesis().number.unwrap_or_default();
            info!("X Layer genesis block = {}", genesis_block);

            // Clone xlayer_args early to avoid partial move issues
            let xlayer_args = args.xlayer_args.clone();
            let datadir = builder.config().datadir().clone();

            let legacy_config = LegacyRpcRouterConfig {
                enabled: xlayer_args.legacy.legacy_rpc_url.is_some(),
                legacy_endpoint: xlayer_args.legacy.legacy_rpc_url.unwrap_or_default(),
                cutoff_block: genesis_block,
                timeout: xlayer_args.legacy.legacy_rpc_timeout,
            };

            // For X Layer full link monitor
            let monitor = XLayerMonitor::new(
                xlayer_args.monitor,
                xlayer_args.builder.flashblocks.enabled,
                xlayer_args.sequencer_mode,
            );

            let add_ons = op_node.add_ons().with_rpc_middleware((
                RpcMonitorLayer::new(monitor.clone()),    // Execute first
                LegacyRpcRouterLayer::new(legacy_config), // Execute second
            ));

            // Parse and validate bridge intercept configuration
            let bridge_config = args
                .xlayer_args
                .intercept
                .to_bridge_intercept_config()
                .map_err(|e| eyre::eyre!("Bridge intercept config error: {e}"))?;

            if bridge_config.enabled {
                tracing::info!(
                    target: "xlayer::intercept",
                    bridge_contract = ?bridge_config.bridge_contract_address,
                    target_token = ?bridge_config.target_token_address,
                    wildcard = bridge_config.wildcard,
                    "Bridge transaction interception enabled"
                );
            }

            // Get the payload events tx for pre-warming the engine tree with locally built
            // pending flashblocks sequence.
            let events_sender = Arc::new(OnceLock::new());
            let tree_config = builder.config().engine.tree_config();

            // Create the X Layer payload service builder
            // It handles both flashblocks and default modes internally
            let payload_builder = XLayerPayloadServiceBuilder::new(
                args.xlayer_args.builder.clone(),
                args.xlayer_args.flashblocks_rpc.flashblock_url.is_some(),
                args.rollup_args.compute_pending_block,
                events_sender.clone(),
            )?
            .with_bridge_config(bridge_config);

            let NodeHandle { node, node_exit_future } = builder
                .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
                .with_components(op_node.components().payload(payload_builder))
                .with_add_ons(add_ons)
                .extend_rpc_modules(move |ctx| {
                    let new_op_eth_api = Arc::new(ctx.registry.eth_api().clone());

                    let flashblocks_state = if let Some(flashblock_url) =
                        args.xlayer_args.flashblocks_rpc.flashblock_url
                    {
                        // Initialize flashblocks RPC
                        let flashblocks_state = FlashblockStateCache::new();
                        let canon_state_rx = ctx.provider().canonical_state_stream();
                        let service = FlashblocksRpcService::new(
                            args.xlayer_args.builder.flashblocks,
                            flashblocks_state.clone(),
                            ctx.node().task_executor.clone(),
                            FlashblocksRpcCtx {
                                provider: ctx.provider().clone(),
                                canon_state_rx,
                                evm_config: OpEvmConfig::optimism(ctx.provider().chain_spec()),
                                chain_spec: ctx.provider().chain_spec(),
                                tree_config,
                            },
                            FlashblocksPersistCtx {
                                datadir,
                                relay_flashblocks: args.rollup_args.flashblocks_url.is_some(),
                            },
                        )?;
                        service.spawn_prewarm(events_sender);
                        service.spawn_persistence()?;
                        service.spawn_rpc(WsFlashBlockStream::new(flashblock_url));
                        info!(target: "reth::cli", "xlayer flashblocks service initialized");

                        // Initialize custom flashblocks subscription
                        if args.xlayer_args.flashblocks_rpc.enable_flashblocks_subscription {
                            let flashblocks_pubsub = FlashblocksPubSub::new(
                                ctx.registry.eth_handlers().pubsub.clone(),
                                flashblocks_state.subscribe_pending_sequence(),
                                Box::new(ctx.node().task_executor.clone()),
                                new_op_eth_api.converter().clone(),
                                args.xlayer_args.flashblocks_rpc.flashblocks_subscription_max_addresses,
                            );
                            ctx.modules.add_or_replace_if_module_configured(
                                RethRpcModule::Eth,
                                flashblocks_pubsub.into_rpc(),
                            )?;
                            info!(target: "reth::cli", "xlayer flashblocks pubsub initialized");
                        }

                        // Register flashblocks Eth API overrides
                        let flashblocks_eth = FlashblocksEthApiExt::new(
                            ctx.registry.eth_api().clone(),
                            flashblocks_state.clone(),
                        );
                        ctx.modules.add_or_replace_if_module_configured(
                            RethRpcModule::Eth,
                            FlashblocksEthApiOverrideServer::into_rpc(flashblocks_eth),
                        )?;
                        info!(target: "reth::cli", "xlayer flashblocks eth api overrides initialized");
                        Some(flashblocks_state)
                    } else {
                        None
                    };

                    // Register X Layer RPC
                    let xlayer_rpc = DefaultRpcExt::new(flashblocks_state);
                    ctx.modules.merge_configured(DefaultRpcExtApiServer::into_rpc(
                        xlayer_rpc,
                    ))?;
                    info!(target: "reth::cli", "xlayer eth rpc extension enabled");
                    info!(message = "X Layer RPC modules initialized");
                    Ok(())
                })
                .launch_with_fn(|builder| {
                    let engine_tree_config = TreeConfig::default()
                        .with_persistence_threshold(builder.config().engine.persistence_threshold)
                        .with_memory_block_buffer_target(
                            builder.config().engine.memory_block_buffer_target,
                        );

                    let dev_mode = builder.config().dev.dev;
                    if dev_mode {
                        tracing::warn!("Running in debug mode");
                        let launcher = DebugNodeLauncher::new(EngineNodeLauncher::new(
                            builder.task_executor().clone(),
                            builder.config().datadir(),
                            engine_tree_config,
                        ));

                        Either::Left(builder.launch_with(launcher))
                    } else {
                        let launcher = EngineNodeLauncher::new(
                            builder.task_executor().clone(),
                            builder.config().datadir(),
                            engine_tree_config,
                        );

                        Either::Right(builder.launch_with(launcher))
                    }
                })
                .await?;

            // Start X Layer full link monitor handle
            start_monitor_handle(
                node.tasks(),
                monitor,
                node.provider().clone(),
                node.payload_builder_handle.clone(),
                node.add_ons_handle.engine_events.new_listener(),
            );

            node_exit_future.await
        })
        .unwrap();
}
