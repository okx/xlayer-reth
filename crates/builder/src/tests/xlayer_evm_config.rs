//! Regression tests for the wiring that `XLayerExecutorBuilder` and
//! `XLayerPayloadServiceBuilder` rely on.
//!
//! Both tests are **compile-time** — each pins an invariant that, if
//! it regressed, would make the XLayerAA executor / payload path
//! silently fall back to the upstream `OpEvmFactory<OpTx>` (or fail
//! to type-check downstream in reth's trait cascade). Catching these
//! via `cargo test --workspace` is far cheaper than surfacing them
//! as trait-bound failures in `bin/node/src/main.rs`.

use crate::{xlayer_evm_config, XLayerEvmConfig};
use op_alloy_rpc_types_engine::OpExecutionData;
use reth_evm::{ConfigureEngineEvm, ConfigureEvm};
use reth_optimism_chainspec::{OpChainSpec, OP_MAINNET};
use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_primitives::OpPrimitives;
use revm::context::TxEnv;
use xlayer_revm::{XLayerAAEvmFactory, XLayerAATransaction};

/// `XLayerEvmConfig` must resolve to the 4-param `OpEvmConfig`
/// expansion whose `EvmFactory` slot carries [`XLayerAAEvmFactory`] —
/// not the upstream default `OpEvmFactory<OpTx>`. The `let`-binding
/// below is a compile-time type-equality check: if the alias ever
/// drifts (e.g. someone swaps the factory back to the default), the
/// assignment fails to compile.
#[test]
fn xlayer_evm_config_alias_uses_xlayer_aa_factory() {
    let _cfg: OpEvmConfig<
        OpChainSpec,
        OpPrimitives,
        OpRethReceiptBuilder,
        XLayerAAEvmFactory<XLayerAATransaction<TxEnv>>,
    > = xlayer_evm_config(OP_MAINNET.clone());
}

/// The XLayer-flavored `OpEvmConfig` must still satisfy both
/// [`ConfigureEvm`] and [`ConfigureEngineEvm`]. The
/// [`ConfigureEngineEvm`] bound is the interesting one: upstream
/// `op-reth` originally impl'd it only for `OpEvmConfig<CS, N, R>`
/// (implicit default factory), so our XLayer-factory variant would
/// silently fail to satisfy the engine-API `newPayload` trait
/// surface. The patch in `deps/optimism` (submodule branch
/// `feat/xlayer-aa-v1`) widened the impl to be `EvmFactory`-generic;
/// this test fails to compile if that patch is ever dropped during
/// an upstream rebase.
#[test]
fn xlayer_evm_config_impls_configure_evm_and_engine_evm() {
    fn assert_impls<E>(_: &E)
    where
        E: ConfigureEvm<Primitives = OpPrimitives> + ConfigureEngineEvm<OpExecutionData> + 'static,
    {
    }
    let cfg: XLayerEvmConfig = xlayer_evm_config(OP_MAINNET.clone());
    assert_impls(&cfg);
}
