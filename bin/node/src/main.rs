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
    builder::{DebugNodeLauncher, EngineNodeLauncher, Node, NodeHandle},
    providers::providers::BlockchainProvider,
};
use reth_chainspec::ChainSpecProvider;
use reth_node_builder::rpc::BasicEngineValidatorBuilder;
use reth_optimism_cli::Cli;
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::{args::RollupArgs, OpEngineValidatorBuilder, OpNode};
use reth_provider::CanonStateSubscriptions;
use reth_rpc_builder::config::RethRpcServerConfig;
use reth_rpc_server_types::RethRpcModule;

use xlayer_chainspec::XLayerChainSpecParser;
use xlayer_flashblocks::{
    FlashblockSequenceValidator, FlashblockStateCache, FlashblocksPersistCtx, FlashblocksPubSub,
    FlashblocksRpcCtx, FlashblocksRpcService, WsFlashBlockStream, XLayerEngineValidatorBuilder,
};
use xlayer_legacy_rpc::{layer::LegacyRpcRouterLayer, LegacyRpcRouterConfig};
use xlayer_monitor::{start_monitor_handle, RpcMonitorLayer, XLayerMonitor};
use xlayer_rpc::{
    DefaultRpcExt, DefaultRpcExtApiServer, FlashblocksEthApiExt, FlashblocksEthApiOverrideServer,
    FlashblocksEthFilterExt, FlashblocksFilterOverrideServer,
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

            // For X Layer full link monitor
            let monitor = XLayerMonitor::new(
                xlayer_args.monitor,
                xlayer_args.builder.flashblocks.enabled,
                xlayer_args.sequencer_mode,
            );

            let add_ons = op_node.add_ons().with_rpc_middleware((
                RpcMonitorLayer::new(monitor.clone()),    // Execute first
                LegacyRpcRouterLayer::new(xlayer_args.legacy.to_legacy_rpc_config(genesis_block)), // Execute second
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

            // Create the X Layer payload service builder
            // It handles both flashblocks and default modes internally
            let fb_p2p_status = Arc::new(OnceLock::new());
            let payload_builder = XLayerPayloadServiceBuilder::new(
                args.xlayer_args.builder.clone(),
                args.rollup_args.compute_pending_block,
                xlayer_args.sequencer_mode,
                fb_p2p_status.clone(),
            )?
            .with_bridge_config(bridge_config);

            // Get the engine validator for flashblocks RPC.
            let engine_validator = Arc::new(OnceLock::new());

            // Replace the default engine validator with `XLayerEngineValidator`, sharing
            // the engine's PayloadProcessor while unifying both the engine validator and
            // flashblocks sequence validator.
            //
            // When flashblocks RPC is enabled, this provides:
            // 1. Flashblocks pre-warming (skip re-execution for already validated blocks)
            // 2. Shared PayloadProcessor (same sparse trie + execution cache)
            // 3. Safeguard against races between engine and flashblocks sequence validator,
            //    no double re-validation of the same block/payload.
            //
            // When flashblocks RPC is disabled, it is a transparent pass-through to the
            // underlying engine validator.
            let tree_config = builder.config().engine.tree_config();
            let engine_builder = XLayerEngineValidatorBuilder::new(
                BasicEngineValidatorBuilder::<OpEngineValidatorBuilder>::default(),
                engine_validator.clone(),
            );
            let add_ons = add_ons.with_engine_validator(engine_builder);

            let NodeHandle { node, node_exit_future } = builder
                .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
                .with_components(op_node.components().payload(payload_builder))
                .with_add_ons(add_ons)
                .extend_rpc_modules(move |ctx| {
                    let new_op_eth_api = Arc::new(ctx.registry.eth_api().clone());

                    let flashblocks_state = if let Some(flashblock_url) =
                        args.xlayer_args.flashblocks_rpc.flashblock_url
                    {
                        let engine_validator = engine_validator
                            .get()
                            .expect("XLayerEngineValidator must be set")
                            .clone();

                        // Initialize flashblocks RPC with the engine's changeset cache
                        let flashblocks_state = FlashblockStateCache::new(
                            ctx.provider().canonical_in_memory_state(),
                            engine_validator.get_changeset_cache(),
                        );

                        // Initialize the flashblocks validator
                        engine_validator.set_flashblocks(
                            FlashblockSequenceValidator::new(
                                OpEvmConfig::optimism(ctx.provider().chain_spec()),
                                ctx.provider().clone(),
                                ctx.provider().chain_spec(),
                                flashblocks_state.clone(),
                                engine_validator.get_payload_processor(),
                                ctx.node().task_executor.clone(),
                                tree_config,
                            ),
                            flashblocks_state.clone(),
                        );

                        let canon_state_rx = ctx.provider().canonical_state_stream();
                        let service = FlashblocksRpcService::new(
                            args.xlayer_args.builder.flashblocks,
                            flashblocks_state.clone(),
                            ctx.node().task_executor.clone(),
                            FlashblocksRpcCtx {
                                canon_state_rx,
                                debug_state_comparison: args
                                    .xlayer_args
                                    .flashblocks_rpc
                                    .flashblocks_debug_state_comparison,
                            },
                            FlashblocksPersistCtx {
                                datadir,
                            },
                        )?;
                        service.spawn_persistence()?;
                        service.spawn_rpc(
                            engine_validator,
                            WsFlashBlockStream::new(flashblock_url),
                        );
                        info!(target: "reth::cli", "xlayer flashblocks service initialized");

                        // Initialize custom flashblocks subscription
                        if args
                            .xlayer_args
                            .flashblocks_rpc
                            .enable_flashblocks_subscription
                        {
                            let flashblocks_pubsub = FlashblocksPubSub::new(
                                ctx.registry.eth_handlers().pubsub.clone(),
                                flashblocks_state.subscribe_pending_sequence(),
                                Box::new(ctx.node().task_executor.clone()),
                                new_op_eth_api.converter().clone(),
                                args.xlayer_args
                                    .flashblocks_rpc
                                    .flashblocks_subscription_max_addresses,
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

                        // Register flashblocks filter override (eth_getLogs)
                        let flashblocks_filter = FlashblocksEthFilterExt::new(
                            ctx.registry.eth_api().clone(),
                            ctx.registry.eth_handlers().filter.clone(),
                            flashblocks_state.clone(),
                            ctx.config().rpc.eth_config().filter_config(),
                        );
                        ctx.modules.add_or_replace_if_module_configured(
                            RethRpcModule::Eth,
                            FlashblocksFilterOverrideServer::into_rpc(flashblocks_filter),
                        )?;
                        info!(target: "reth::cli", "xlayer flashblocks filter overrides initialized");
                        Some(flashblocks_state)
                    } else {
                        None
                    };

                    // Register X Layer RPC
                    let peer_status = fb_p2p_status.get().cloned();
                    if xlayer_args.sequencer_mode && peer_status.is_none() {
                        tracing::warn!(target: "reth::cli", "fb_p2p_status not initialized — eth_flashblocksPeerStatus will return null on sequencer");
                    }
                    let xlayer_rpc = DefaultRpcExt::new(flashblocks_state, peer_status);
                    ctx.modules
                        .merge_configured(DefaultRpcExtApiServer::into_rpc(xlayer_rpc))?;
                    info!(target: "reth::cli", "xlayer eth rpc extension enabled");
                    info!(message = "X Layer RPC modules initialized");
                    Ok(())
                })
                .launch_with_fn(|builder| {
                    let engine_tree_config = builder.config().engine.tree_config();

                    let dev_mode = builder.config().dev.dev;
                    if dev_mode {
                        tracing::warn!("Running in debug mode");
                        let launcher = DebugNodeLauncher::new(
                            EngineNodeLauncher::new(
                                builder.task_executor().clone(),
                                builder.config().datadir(),
                                engine_tree_config,
                            )
                        );

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
