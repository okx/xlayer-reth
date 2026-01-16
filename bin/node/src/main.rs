#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

mod args_xlayer;

use std::sync::Arc;

use args_xlayer::XLayerArgs;
use clap::Parser;
use tracing::info;

use op_alloy_network::Optimism;
use op_rbuilder::{
    args::OpRbuilderArgs,
    builders::{BuilderConfig, FlashblocksServiceBuilder},
    traits::{NodeBounds, PoolBounds},
};
use reth::{
    builder::{EngineNodeLauncher, NodeHandle, TreeConfig},
    providers::providers::BlockchainProvider,
};
use reth_optimism_cli::Cli;
use reth_optimism_node::OpNode;

use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_node_builder::{
    BuilderContext,
    components::PayloadServiceBuilder,
    rpc::BasicEngineValidatorBuilder,
};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::{OpEngineApiBuilder, OpEngineValidatorBuilder};
use reth_payload_builder::PayloadBuilderHandle;
use reth_rpc_eth_api::EthApiTypes;
use reth_rpc_server_types::RethRpcModule;
use xlayer_chainspec::XLayerChainSpecParser;
use xlayer_flashblocks::handler::FlashblocksService;
use xlayer_flashblocks::subscription::FlashblocksPubSub;
use xlayer_legacy_rpc::{layer::LegacyRpcRouterLayer, LegacyRpcRouterConfig};
use xlayer_node::{XLayerNode, XLayerExecutorBuilder};
use xlayer_rpc::xlayer_ext::{XlayerRpcExt, XlayerRpcExtApiServer};

/// Flashblocks payload builder wrapper that accepts XLayerEvmConfig.
struct XLayerFlashblocksServiceBuilder(FlashblocksServiceBuilder);

impl<Node, Pool> PayloadServiceBuilder<Node, Pool, xlayer_evm::XLayerEvmConfig>
    for XLayerFlashblocksServiceBuilder
where
    Node: NodeBounds,
    Pool: PoolBounds,
{
    async fn spawn_payload_builder_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        _evm_config: xlayer_evm::XLayerEvmConfig,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>> {
        // Delegate to the original flashblocks builder using the standard OP EVM config.
        let base_evm = OpEvmConfig::optimism(ctx.chain_spec());
        <FlashblocksServiceBuilder as PayloadServiceBuilder<Node, Pool, OpEvmConfig>>::spawn_payload_builder_service(
            self.0,
            ctx,
            pool,
            base_evm,
        )
        .await
    }
}

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
            info!(message = "starting custom XLayer node");

            // Validate XLayer configuration
            if let Err(e) = args.xlayer_args.validate() {
                eprintln!("XLayer configuration error: {e}");
                std::process::exit(1);
            }

            // Create XLayerNode (re-exported OpNode)
            let xlayer_node = XLayerNode::new(args.node_args.rollup_args.clone());
            info!(target: "reth::cli", "XLayer node initialized with Poseidon precompile support");

            let genesis_block = builder.config().chain.genesis().number.unwrap_or_default();
            info!("XLayer genesis block = {}", genesis_block);

            let legacy_config = LegacyRpcRouterConfig {
                enabled: args.xlayer_args.legacy.legacy_rpc_url.is_some(),
                legacy_endpoint: args.xlayer_args.legacy.legacy_rpc_url.unwrap_or_default(),
                cutoff_block: genesis_block,
                timeout: args.xlayer_args.legacy.legacy_rpc_timeout,
            };

            // Should run as sequencer if flashblocks.enabled = true. Doing so means you are
            // running a flashblocks producing sequencer.
            let NodeHandle { node: _node, node_exit_future } = if args.node_args.flashblocks.enabled
            {
                let builder_config = BuilderConfig::try_from(args.node_args.clone())
                    .expect("Failed to convert builder args to builder config");

                builder
                    .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
                    .with_components(
                        xlayer_node
                            .components()
                            .executor(XLayerExecutorBuilder)
                            .payload(XLayerFlashblocksServiceBuilder(
                                FlashblocksServiceBuilder(builder_config),
                            )),
                    )
                    .with_add_ons(
                        xlayer_node
                            .add_ons_builder::<Optimism>()
                            .with_rpc_middleware(LegacyRpcRouterLayer::new(legacy_config))
                            .build::<
                                _,
                                OpEngineValidatorBuilder,
                                OpEngineApiBuilder<OpEngineValidatorBuilder>,
                                BasicEngineValidatorBuilder<OpEngineValidatorBuilder>,
                            >(),
                    )
                    .on_component_initialized(move |_ctx| {
                        // TODO: Initialize XLayer components here
                        Ok(())
                    })
                    .extend_rpc_modules(move |ctx| {
                        let new_op_eth_api = Arc::new(ctx.registry.eth_api().clone());

                        // Register XLayer RPC
                        let xlayer_rpc = XlayerRpcExt { backend: new_op_eth_api };
                        ctx.modules.merge_configured(xlayer_rpc.into_rpc())?;
                        info!(target: "reth::cli", "xlayer rpc extension enabled");

                        info!(message = "XLayer RPC modules initialized");
                        Ok(())
                    })
                    .launch_with_fn(|builder| {
                        let engine_tree_config = TreeConfig::default()
                            .with_persistence_threshold(
                                builder.config().engine.persistence_threshold,
                            )
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
                    .with_components(
                        xlayer_node.components()
                            .executor(XLayerExecutorBuilder) // ← 使用自定义 Executor with Poseidon
                    )
                    .with_add_ons(
                        xlayer_node
                            .add_ons_builder::<Optimism>()
                            .with_rpc_middleware(LegacyRpcRouterLayer::new(legacy_config))
                            .build::<
                                _,
                                OpEngineValidatorBuilder,
                                OpEngineApiBuilder<OpEngineValidatorBuilder>,
                                BasicEngineValidatorBuilder<OpEngineValidatorBuilder>,
                            >(),
                    )
                    .on_component_initialized(move |_ctx| {
                        // TODO: Initialize XLayer components here
                        Ok(())
                    })
                    .extend_rpc_modules(move |ctx| {
                        let new_op_eth_api = Arc::new(ctx.registry.eth_api().clone());

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

                        if args.xlayer_args.enable_flashblocks_subscription
                            && let Some(pending_blocks_rx) = new_op_eth_api.pending_block_rx()
                        {
                            let eth_pubsub = ctx.registry.eth_handlers().pubsub.clone();

                            let flashblocks_pubsub = FlashblocksPubSub::new(
                                eth_pubsub,
                                pending_blocks_rx,
                                Box::new(ctx.node().task_executor().clone()),
                                new_op_eth_api.tx_resp_builder().clone(),
                                args.xlayer_args.flashblocks_subscription_max_addresses,
                            );
                            ctx.modules.add_or_replace_if_module_configured(
                                RethRpcModule::Eth,
                                flashblocks_pubsub.into_rpc(),
                            )?;
                            info!(target: "reth::cli", "xlayer eth pubsub initialized");
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
                            .with_persistence_threshold(
                                builder.config().engine.persistence_threshold,
                            )
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
