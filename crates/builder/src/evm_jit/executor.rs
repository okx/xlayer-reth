//! [`JitExecutorBuilder`] — drop-in replacement for `OpExecutorBuilder` that wires in
//! [`JitEvmFactory`] so every block execution uses LLVM-JIT-compiled EVM bytecode.

use super::aot_store::PersistentArtifactStore;
use super::factory::JitEvmFactory;
use alloy_op_evm::OpBlockExecutorFactory;
use reth_node_api::NodeTypes;
use reth_node_builder::{components::ExecutorBuilder, node::FullNodeTypes, BuilderContext};
use reth_optimism_evm::{OpBlockAssembler, OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_forks::OpHardforks;
use reth_optimism_primitives::OpPrimitives;
use revmc_runtime::runtime::{ArtifactStore, JitBackend, RuntimeConfig, RuntimeTuning};
use std::{marker::PhantomData, path::PathBuf, sync::Arc};

/// Executor builder that uses [`JitEvmFactory`] for LLVM-JIT EVM execution.
///
/// Drop-in replacement for `reth_optimism_node::node::OpExecutorBuilder`. Configuration is
/// taken from environment variables at build time:
///
/// | Env var | Default | Meaning |
/// |---------|---------|---------|
/// | `XLAYER_JIT_ENABLED` | 1 | 1=on, 0=disabled |
/// | `XLAYER_JIT_BLOCKING` | 0 | 1=synchronous compile (no fallback) |
/// | `XLAYER_JIT_HOT_THRESHOLD` | 8 | Lookups before triggering compile |
/// | `XLAYER_JIT_WORKERS` | (auto) | Compilation worker thread count |
/// | `XLAYER_JIT_MAX_BYTECODE_LEN` | 0 (unbounded) | Max bytecode size to compile |
/// | `XLAYER_JIT_CODE_CACHE_BYTES` | 1 GiB | Resident code cache budget |
/// | `XLAYER_AOT_DIR` | (empty) | Directory for persistent AOT artifacts. Empty = in-memory only (JIT cold). |
#[derive(Debug, Default)]
pub struct JitExecutorBuilder;

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_flag(key: &str, default: bool) -> bool {
    match std::env::var(key).ok().as_deref() {
        Some("1") | Some("true") | Some("TRUE") | Some("yes") => true,
        Some("0") | Some("false") | Some("FALSE") | Some("no") => false,
        _ => default,
    }
}

/// Expand a leading `~/` to `$HOME/`. Returns the path unchanged if it
/// doesn't start with `~/` or if `$HOME` is unset.
fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Builds an optional persistent AOT artifact store from `XLAYER_AOT_DIR`.
///
/// Returns `None` when the env var is unset/empty (in-memory JIT mode).
/// Returns `None` and warns when the directory can't be opened.
fn build_aot_store_from_env() -> Option<Arc<dyn ArtifactStore>> {
    let dir = std::env::var("XLAYER_AOT_DIR").ok().filter(|d| !d.is_empty())?;
    let path = expand_tilde(&dir);
    let t_open = std::time::Instant::now();
    match PersistentArtifactStore::open(path.clone()) {
        Ok(store) => {
            let dlopen_us = t_open.elapsed().as_micros() as u64;
            let loaded = store.len();
            tracing::info!(
                target: "xlayer::jit::aot",
                dir = %path.display(),
                loaded,
                dlopen_us,
                n_manifests = loaded,
                "AOT store opened"
            );
            Some(Arc::new(store) as Arc<dyn ArtifactStore>)
        }
        Err(err) => {
            tracing::warn!(
                target: "xlayer::jit::aot",
                dir = %path.display(),
                error = %err,
                "failed to open AOT store; falling back to in-memory JIT"
            );
            None
        }
    }
}

/// Builds a `JitBackend` from `XLAYER_JIT_*` environment variables.
fn build_backend_from_env() -> JitBackend {
    let default_tuning = RuntimeTuning::default();
    let tuning = RuntimeTuning {
        jit_hot_threshold: env_or("XLAYER_JIT_HOT_THRESHOLD", default_tuning.jit_hot_threshold),
        jit_worker_count: env_or("XLAYER_JIT_WORKERS", default_tuning.jit_worker_count),
        jit_max_bytecode_len: env_or(
            "XLAYER_JIT_MAX_BYTECODE_LEN",
            default_tuning.jit_max_bytecode_len,
        ),
        resident_code_cache_bytes: env_or(
            "XLAYER_JIT_CODE_CACHE_BYTES",
            default_tuning.resident_code_cache_bytes,
        ),
        ..default_tuning
    };
    let store = build_aot_store_from_env();
    // When an AOT store is configured, switch the runtime into AOT mode so that
    // observed misses persist compiled artifacts to disk (otherwise JIT compilations
    // only populate the in-memory resident map and never reach the store).
    let aot_mode = store.is_some();
    let config = RuntimeConfig {
        enabled: env_flag("XLAYER_JIT_ENABLED", true),
        blocking: env_flag("XLAYER_JIT_BLOCKING", false),
        aot: aot_mode,
        tuning,
        store,
        ..RuntimeConfig::default()
    };
    let enabled = config.enabled;
    let blocking = config.blocking;
    match JitBackend::new(config) {
        Ok(backend) => {
            tracing::info!(
                target: "xlayer::jit",
                enabled,
                blocking,
                "JIT runtime enabled"
            );
            backend
        }
        Err(err) => {
            tracing::warn!(
                target: "xlayer::jit",
                error = %err,
                "Failed to initialize JIT backend; falling back to disabled"
            );
            JitBackend::disabled()
        }
    }
}

impl<Node> ExecutorBuilder<Node> for JitExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: OpHardforks, Primitives = OpPrimitives>>,
{
    type EVM = OpEvmConfig<
        <Node::Types as NodeTypes>::ChainSpec,
        <Node::Types as NodeTypes>::Primitives,
        OpRethReceiptBuilder,
        JitEvmFactory,
    >;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let chain_spec = ctx.chain_spec();
        let backend = build_backend_from_env();
        spawn_periodic_jit_stats(backend.clone());
        Ok(OpEvmConfig {
            executor_factory: OpBlockExecutorFactory::new(
                OpRethReceiptBuilder::default(),
                chain_spec.clone(),
                JitEvmFactory::new(backend),
            ),
            block_assembler: OpBlockAssembler::new(chain_spec),
            _pd: PhantomData,
        })
    }
}

/// Spawns a background task that emits a single-line `info!` log every
/// `XLAYER_JIT_STATS_INTERVAL_MS` (default 1000 ms) with the current
/// [`JitBackend`] statistics snapshot. Disabled when the interval is `0`.
///
/// Each log line is grep-friendly:
///
/// ```text
/// INFO xlayer::jit::stats resident=4 compile_ok=4 compile_err=0 dispatched=4 \
///      lookups_hits=12345 lookups_misses=4 pending=0 evictions=0 code_bytes=1234567
/// ```
fn spawn_periodic_jit_stats(backend: JitBackend) {
    let interval_ms: u64 = env_or("XLAYER_JIT_STATS_INTERVAL_MS", 1000);
    if interval_ms == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_millis(interval_ms));
        // The first tick fires immediately; consume it so the first emitted line
        // reflects at least `interval_ms` of runtime activity.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let s = backend.stats();
            tracing::info!(
                target: "xlayer::jit::stats",
                resident = s.resident_entries,
                compile_ok = s.compilations_succeeded,
                compile_err = s.compilations_failed,
                dispatched = s.compilations_dispatched,
                lookup_hits = s.lookup_hits,
                lookup_misses = s.lookup_misses,
                pending = s.pending_jobs,
                evictions = s.evictions,
                code_bytes = s.jit_code_bytes,
                "jit_periodic_stats"
            );
        }
    });
}
