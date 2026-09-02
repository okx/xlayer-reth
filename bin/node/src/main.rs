#![allow(missing_docs, rustdoc::missing_crate_level_docs)]

mod args;
mod payload;

use payload::XLayerPayloadServiceBuilder;

use args::XLayerArgs;
use clap::Parser;
use either::Either;
use std::str::FromStr;
use std::sync::Arc;
use tracing::info;

use op_alloy_network::Optimism;
use reth::rpc::eth::EthApiTypes;
use reth::{
    builder::{DebugNodeLauncher, EngineNodeLauncher, Node, NodeHandle},
    providers::providers::BlockchainProvider,
};
use reth_optimism_cli::Cli;
use reth_optimism_node::{args::RollupArgs, OpNode};
use reth_rpc_server_types::RethRpcModule;

use xlayer_chainspec::XLayerChainSpecParser;
use xlayer_flashblocks::handler::FlashblocksService;
use xlayer_flashblocks::subscription::FlashblocksPubSub;
use xlayer_legacy_rpc::{layer::LegacyRpcRouterLayer, LegacyRpcRouterConfig};
use xlayer_monitor::{start_monitor_handle, RpcMonitorLayer, XLayerMonitor};
use xlayer_rpc::xlayer_ext::{XlayerRpcExt, XlayerRpcExtApiServer};
use xlayer_rpc::{
    FlashblocksEthApiExt, FlashblocksEthApiOverrideServer, XlayerAuditApiServer, XlayerAuditRpc,
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
    // deps/optimism submodule pin: f63aa32a40ee6a873d57eaa5a84d23e9ee513b5c (v0.1.6-rc.2-3-gf63aa32a40)
    xlayer_version::init_version!();

    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    XLayerArgs::validate_init_command();

    // X Layer: swap `kms:<name>` references in secret-bearing flags for their
    // plaintext BEFORE clap parses. This is the single injection point for KMS —
    // it has to run pre-parse because `--rollup.builder-secret-key` is typed as
    // `Option<Signer>`, whose `FromStr` would reject a reference during parsing,
    // and it means no other crate is involved in resolution.
    let argv = match resolve_kms_secret_flags(std::env::args().collect()) {
        Ok(argv) => argv,
        Err(e) => {
            eprintln!("X Layer KMS configuration error: {e}");
            std::process::exit(1);
        }
    };

    Cli::<XLayerChainSpecParser, Args>::parse_from(argv)
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

            // Create the X Layer payload service builder
            // It handles both flashblocks and default modes internally
            let payload_builder = XLayerPayloadServiceBuilder::new(
                args.xlayer_args.builder.clone(),
                args.rollup_args.compute_pending_block,
            )?
            .with_bridge_config(bridge_config);

            let NodeHandle { node, node_exit_future } = builder
                .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
                .with_components(op_node.components().payload(payload_builder))
                .with_add_ons(add_ons)
                .extend_rpc_modules(move |ctx| {
                    let new_op_eth_api = Arc::new(ctx.registry.eth_api().clone());

                    // Initialize flashblocks RPC service if not in flashblocks sequencer mode
                    if !args.xlayer_args.builder.flashblocks.enabled {
                        if let Some(flashblock_rx) = new_op_eth_api.subscribe_received_flashblocks()
                        {
                            let service = FlashblocksService::new(
                                ctx.node().clone(),
                                flashblock_rx,
                                args.xlayer_args.builder.flashblocks,
                                args.rollup_args.flashblocks_url.is_some(),
                                datadir,
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
                                ctx.node().task_executor.clone(),
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
                    let xlayer_rpc = XlayerRpcExt { backend: new_op_eth_api.clone() };
                    ctx.modules.merge_configured(XlayerRpcExtApiServer::<Optimism>::into_rpc(
                        xlayer_rpc,
                    ))?;
                    info!(target: "reth::cli", "xlayer rpc extension enabled");

                    // Register the side-effect-free L1 deposit audit RPC.
                    let xlayer_audit_rpc =
                        XlayerAuditRpc { backend: new_op_eth_api.clone() };
                    ctx.modules.merge_configured(XlayerAuditApiServer::into_rpc(
                        xlayer_audit_rpc,
                    ))?;
                    info!(target: "reth::cli", "xlayer audit rpc extension enabled");

                    // Register X Layer flashblocks-aware transaction_count override.
                    // `add_or_replace_if_module_configured` (not `merge_configured`)
                    // replaces the default `eth_getTransactionCount` dispatch entry;
                    // `merge_configured` would collide on the duplicate method name.
                    let flashblocks_eth = FlashblocksEthApiExt::new((*new_op_eth_api).clone());
                    ctx.modules.add_or_replace_if_module_configured(
                        RethRpcModule::Eth,
                        FlashblocksEthApiOverrideServer::into_rpc(flashblocks_eth),
                    )?;
                    info!(target: "reth::cli", "xlayer flashblocks eth api overrides initialized");

                    info!(message = "X Layer RPC modules initialized");
                    Ok(())
                })
                .launch_with_fn(|builder| {
                    let engine_tree_config = builder.config().engine.tree_config();

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

/// X Layer: resolves `kms:<name>` references in secret-bearing CLI flags (and
/// their environment fallbacks) before clap ever sees them, so the crates that
/// consume these secrets stay untouched and only ever handle plaintext.
/// Non-KMS deployments behave exactly as upstream.
///
/// Three flags are handled, each through the carrier that keeps the resolved
/// secret off disk:
///
/// - `--rollup.builder-secret-key` / `BUILDER_SECRET_KEY`: the value is always
///   the secret itself (never a file), so a reference is simply replaced with
///   the resolved plaintext in argv/env.
/// - `--p2p-secret-key`: the value may be a reference, or an existing file whose
///   contents are one. Either is rewritten to reth's sibling
///   `--p2p-secret-key-hex`, which carries the key in memory.
/// - `--flashblocks.p2p_private_key_file` / `FLASHBLOCK_P2P_PRIVATE_KEY_FILE`:
///   same two shapes, but its consumer only accepts a file path, so the
///   resolved key is staged in an anonymous memfd and the value rewritten to
///   `/proc/self/fd/<n>` (Linux-only, like the node itself).
fn resolve_kms_secret_flags(mut argv: Vec<String>) -> eyre::Result<Vec<String>> {
    // The value itself is the secret; a reference resolves in place.
    let builder_key = |val: &str| -> eyre::Result<Option<String>> {
        if !xlayer_kms::is_kms_ref(val) {
            return Ok(None);
        }
        let plain =
            xlayer_kms::maybe_resolve(val).map_err(|e| eyre::eyre!("builder secret key: {e}"))?;
        Ok(Some(plain.trim().to_string()))
    };

    // Reference (direct or via file) resolves to plaintext hex.
    let node_p2p_key = |val: &str| -> eyre::Result<Option<String>> {
        let Some(reference) = kms_ref_in_value_or_file(val)? else { return Ok(None) };
        let plain = xlayer_kms::maybe_resolve(&reference)
            .map_err(|e| eyre::eyre!("p2p secret key: {e}"))?;
        let hex = plain.trim();
        // Validate here so a malformed KMS entry is attributed to KMS rather
        // than surfacing as a confusing clap error on a flag nobody passed.
        alloy_primitives::B256::from_str(hex)
            .map_err(|e| eyre::eyre!("p2p secret key from KMS is not 32 hex-encoded bytes: {e}"))?;
        Ok(Some(hex.to_string()))
    };

    // Reference (direct or via file) becomes a memfd path, because the
    // flashblocks service reads this flag strictly as a file.
    let flashblocks_key = |val: &str| -> eyre::Result<Option<String>> {
        let Some(reference) = kms_ref_in_value_or_file(val)? else { return Ok(None) };
        let plain = xlayer_kms::maybe_resolve(&reference)
            .map_err(|e| eyre::eyre!("flashblocks p2p private key: {e}"))?;
        Ok(Some(stage_in_memfd(plain.trim())?))
    };

    rewrite_flag(&mut argv, "--rollup.builder-secret-key", None, &builder_key)?;
    // The resolved plaintext must ride reth's sibling flag, hence the rename.
    rewrite_flag(&mut argv, "--p2p-secret-key", Some("--p2p-secret-key-hex"), &node_p2p_key)?;
    rewrite_flag(&mut argv, "--flashblocks.p2p_private_key_file", None, &flashblocks_key)?;

    // clap falls back to these env vars when the flag is absent; rewrite them the
    // same way. (reth's --p2p-secret-key has no env fallback.)
    rewrite_env("BUILDER_SECRET_KEY", &builder_key)?;
    rewrite_env("FLASHBLOCK_P2P_PRIVATE_KEY_FILE", &flashblocks_key)?;

    Ok(argv)
}

/// Env counterpart of [`rewrite_flag`]: replaces the variable's value with the
/// resolved one. Env vars carry no flag name, so no rename is involved.
fn rewrite_env(var: &str, resolve: &ValueRewrite) -> eyre::Result<()> {
    if let Ok(val) = std::env::var(var)
        && let Some(new_val) = resolve(&val)?
    {
        // SAFETY: called from main before any other thread is spawned, same as
        // the RUST_BACKTRACE set_var above.
        unsafe { std::env::set_var(var, new_val) };
    }
    Ok(())
}

/// Resolves one secret-bearing value: maps it to its replacement, or to `None`
/// to leave the argument untouched.
type ValueRewrite = dyn Fn(&str) -> eyre::Result<Option<String>>;

/// Rewrites every `--flag value` / `--flag=value` occurrence in `argv` whose
/// value `resolve` maps to a replacement. `rename` substitutes the flag itself
/// on rewritten occurrences — flag names are static, so the one rewrite that
/// moves a value onto a sibling flag passes it as data rather than computing it.
/// Values mapped to `None`, and flags that merely share the prefix (like
/// `--p2p-secret-key-hex` when scanning for `--p2p-secret-key`), are left
/// untouched.
///
/// Scanning is positional and stops at a literal `--`, after which clap treats
/// everything as positional arguments. One theoretical misfire remains: another
/// flag taking a value that is literally our flag string. Telling that apart
/// needs clap's full flag table; since a rewrite additionally requires the NEXT
/// token to resolve as a `kms:` reference, hitting it takes a deliberately
/// pathological command line, and the failure is a loud parse error, not a
/// silently wrong secret.
fn rewrite_flag(
    argv: &mut [String],
    flag: &str,
    rename: Option<&str>,
    resolve: &ValueRewrite,
) -> eyre::Result<()> {
    let new_flag = rename.unwrap_or(flag);
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == "--" {
            break;
        }
        if argv[i] == flag {
            if let Some(val) = argv.get(i + 1).cloned()
                && let Some(new_val) = resolve(&val)?
            {
                argv[i] = new_flag.to_string();
                argv[i + 1] = new_val;
            }
            i += 2;
        } else {
            let inline_val = argv[i]
                .strip_prefix(flag)
                .and_then(|rest| rest.strip_prefix('='))
                .map(str::to_string);
            if let Some(val) = inline_val
                && let Some(new_val) = resolve(&val)?
            {
                argv[i] = format!("{new_flag}={new_val}");
            }
            i += 1;
        }
    }
    Ok(())
}

/// Extracts a `kms:<name>` reference from a flag value that is nominally a file
/// path: either the value itself is a reference, or it names an existing file
/// whose contents are one. Returns `None` for everything else (plain key files,
/// paths that don't exist yet) so those keep their upstream behavior.
fn kms_ref_in_value_or_file(val: &str) -> eyre::Result<Option<String>> {
    if xlayer_kms::is_kms_ref(val) {
        return Ok(Some(val.to_string()));
    }
    let path = std::path::Path::new(val);
    if path.exists() {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| eyre::eyre!("failed to read secret key file {val}: {e}"))?;
        if xlayer_kms::is_kms_ref(&contents) {
            return Ok(Some(contents.trim().to_string()));
        }
    }
    Ok(None)
}

/// Stages `contents` in an anonymous, process-private memfd and returns a
/// `/proc/self/fd/<n>` path for it, so consumers that insist on reading a file
/// get one without the secret ever touching the filesystem. The fd is
/// deliberately leaked: the consumer reads the path lazily at service startup,
/// so it must stay valid for the life of the process.
#[cfg(target_os = "linux")]
fn stage_in_memfd(contents: &str) -> eyre::Result<String> {
    use std::io::Write as _;
    use std::os::fd::{FromRawFd as _, IntoRawFd as _};

    // SAFETY: memfd_create is passed a valid NUL-terminated name and no flags;
    // the returned fd is checked before being wrapped, and File::from_raw_fd
    // takes ownership of an fd nothing else holds.
    let raw = unsafe { libc::memfd_create(c"xlayer-kms-key".as_ptr(), 0) };
    if raw < 0 {
        return Err(eyre::eyre!("memfd_create failed: {}", std::io::Error::last_os_error()));
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
    file.write_all(contents.as_bytes())
        .map_err(|e| eyre::eyre!("failed to write key to memfd: {e}"))?;
    let raw = file.into_raw_fd(); // leak: keep the fd (and thus the path) alive
    Ok(format!("/proc/self/fd/{raw}"))
}

#[cfg(not(target_os = "linux"))]
fn stage_in_memfd(_contents: &str) -> eyre::Result<String> {
    Err(eyre::eyre!(
        "a kms:<name> reference for --flashblocks.p2p_private_key_file is only supported on Linux"
    ))
}

#[cfg(test)]
mod kms_flag_tests {
    use super::*;

    fn to_vec(args: &[&str]) -> Vec<String> {
        args.iter().map(ToString::to_string).collect()
    }

    /// A rewrite that marks any value ending in `!` — stands in for KMS
    /// resolution, which unit tests cannot perform.
    fn fake(val: &str) -> eyre::Result<Option<String>> {
        Ok(val.strip_suffix('!').map(|v| format!("plain-{v}")))
    }

    #[test]
    fn rewrite_flag_handles_space_and_equals_forms() {
        let mut argv = to_vec(&["node", "--key", "a!", "--key=b!", "--key", "keep"]);
        rewrite_flag(&mut argv, "--key", None, &fake).unwrap();
        assert_eq!(argv, to_vec(&["node", "--key", "plain-a", "--key=plain-b", "--key", "keep"]));
    }

    #[test]
    fn rewrite_flag_renames_rewritten_occurrences_only() {
        let mut argv = to_vec(&["node", "--key", "a!", "--key=b!", "--key", "keep"]);
        rewrite_flag(&mut argv, "--key", Some("--key-hex"), &fake).unwrap();
        assert_eq!(
            argv,
            to_vec(&["node", "--key-hex", "plain-a", "--key-hex=plain-b", "--key", "keep"])
        );
    }

    #[test]
    fn rewrite_flag_ignores_longer_flags_sharing_the_prefix() {
        let mut argv = to_vec(&["node", "--p2p-secret-key-hex=a!", "--p2p-secret-key-hex", "b!"]);
        rewrite_flag(&mut argv, "--p2p-secret-key", Some("--p2p-secret-key-hex"), &fake).unwrap();
        assert_eq!(
            argv,
            to_vec(&["node", "--p2p-secret-key-hex=a!", "--p2p-secret-key-hex", "b!"])
        );
    }

    #[test]
    fn rewrite_flag_stops_at_the_positional_separator() {
        // Past a literal `--`, clap treats everything as positional arguments.
        let mut argv = to_vec(&["node", "--", "--key", "a!", "--key=b!"]);
        rewrite_flag(&mut argv, "--key", None, &fake).unwrap();
        assert_eq!(argv, to_vec(&["node", "--", "--key", "a!", "--key=b!"]));
    }

    #[test]
    fn rewrite_flag_tolerates_missing_trailing_value() {
        // clap will report the missing value; the rewriter must not panic.
        let mut argv = to_vec(&["node", "--key"]);
        rewrite_flag(&mut argv, "--key", None, &fake).unwrap();
        assert_eq!(argv, to_vec(&["node", "--key"]));
    }

    #[test]
    fn plain_key_files_and_missing_paths_pass_through() {
        let dir = std::env::temp_dir().join(format!("xlayer-kms-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain-key");
        std::fs::write(&plain, "aa".repeat(32)).unwrap();
        assert_eq!(kms_ref_in_value_or_file(plain.to_str().unwrap()).unwrap(), None);
        assert_eq!(
            kms_ref_in_value_or_file(dir.join("does-not-exist").to_str().unwrap()).unwrap(),
            None
        );

        let referenced = dir.join("ref-key");
        std::fs::write(&referenced, "kms:my-key\n").unwrap();
        assert_eq!(
            kms_ref_in_value_or_file(referenced.to_str().unwrap()).unwrap(),
            Some("kms:my-key".to_string())
        );
        assert_eq!(kms_ref_in_value_or_file("kms:direct").unwrap(), Some("kms:direct".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn memfd_path_reads_back() {
        let path = stage_in_memfd("deadbeef").unwrap();
        assert!(path.starts_with("/proc/self/fd/"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "deadbeef");
    }

    #[cfg(not(feature = "kms"))]
    #[test]
    fn references_fail_fast_without_the_kms_feature() {
        let err = resolve_kms_secret_flags(to_vec(&[
            "node",
            "--rollup.builder-secret-key",
            "kms:builder",
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("KMS support is not compiled"), "{err}");
    }

    #[test]
    fn non_kms_argv_is_untouched() {
        let argv = to_vec(&[
            "node",
            "--rollup.builder-secret-key",
            &"aa".repeat(32),
            "--p2p-secret-key",
            "/nonexistent/p2p.key",
            "--flashblocks.p2p_private_key_file=",
        ]);
        assert_eq!(resolve_kms_secret_flags(argv.clone()).unwrap(), argv);
    }
}
