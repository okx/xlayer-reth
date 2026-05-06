//! [`JitExecutorBuilder`] — drop-in replacement for `OpExecutorBuilder` that wires in
//! [`JitEvmFactory`] so every block execution uses LLVM-JIT-compiled EVM bytecode.

use super::factory::JitEvmFactory;
use alloy_op_evm::OpBlockExecutorFactory;
use reth_node_api::NodeTypes;
use reth_node_builder::{components::ExecutorBuilder, node::FullNodeTypes, BuilderContext};
use reth_optimism_evm::{OpBlockAssembler, OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_forks::OpHardforks;
use reth_optimism_primitives::OpPrimitives;
use revmc_runtime::runtime::{JitBackend, RuntimeConfig, RuntimeTuning};
use std::marker::PhantomData;

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
    let config = RuntimeConfig {
        enabled: env_flag("XLAYER_JIT_ENABLED", true),
        blocking: env_flag("XLAYER_JIT_BLOCKING", false),
        tuning,
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
