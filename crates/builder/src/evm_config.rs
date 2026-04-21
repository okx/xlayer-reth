//! XLayerAA-aware `OpEvmConfig` alias and constructor.
//!
//! `reth_optimism_evm::OpEvmConfig<ChainSpec, N, R, EvmFactory>` is already
//! generic over the `EvmFactory` parameter — the default is
//! `OpEvmFactory<OpTx>`, which we swap for [`XLayerAAEvmFactory`]. Every
//! non-AA transaction (deposit + regular) continues to flow through the
//! upstream op-stack lifecycle unchanged; type-`0x7B` transactions pick up
//! the XLayerAA branches inside [`XLayerAAHandler`].
//!
//! ## Wiring status
//!
//! This file exports [`XLayerEvmConfig`] and the [`xlayer_evm_config`]
//! constructor. The two direct call sites that must flip to use them:
//!
//! - `bin/node/src/payload.rs:108` — the `PayloadServiceBuilder` impl's
//!   third type parameter; currently `OpEvmConfig`.
//! - `crates/builder/src/default/service.rs:125` — the
//!   `OpPayloadBuilder::with_builder_config(..., OpEvmConfig::optimism(...), ...)`
//!   construction in the default builder path.
//!
//! Flashblocks-path sites (`crates/builder/src/flashblocks/service.rs:132,
//! 162` and `bin/node/src/main.rs:189`) are deliberately **not** part of
//! this milestone. They follow in a later PR — the flashblocks builder has
//! additional `p2p` / `FlashblocksSequenceValidator` trait surface that
//! has its own migration cost.

use core::marker::PhantomData;
use std::sync::Arc;

use alloy_op_evm::OpBlockExecutorFactory;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpBlockAssembler, OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_primitives::OpPrimitives;
use revm::context::TxEnv;
use xlayer_revm::{XLayerAAEvmFactory, XLayerAATransaction};

/// XLayerAA-capable [`OpEvmConfig`]: identical to the upstream
/// [`reth_optimism_evm::OpEvmConfig`] except the EVM factory produces an
/// [`XLayerAAEvm`](xlayer_revm::XLayerAAEvm) rather than an `OpEvm`.
///
/// TODO(xlayer-aa M3): the `ChainSpec` parameter is pinned to
/// [`OpChainSpec`] because xlayer-reth doesn't yet have its own
/// chainspec wrapper. Once `XLayerHardfork::XLayerAA` lands — or any
/// other XLayer-exclusive hardfork — we need:
///
/// 1. an `XLayerChainSpec` newtype wrapping `Arc<OpChainSpec>` (+ XLayer
///    fork activations currently scattered as module-level `const`s in
///    `crates/chainspec/src/lib.rs`), implementing `OpHardforks` +
///    `EthChainSpec<Header = Header>` (the only trait bounds
///    `OpEvmConfig` requires);
/// 2. a separate `XLayerHardforks` trait for AA-specific queries (e.g.
///    `is_xlayer_aa_active_at_timestamp`);
/// 3. this alias flipped to `OpEvmConfig<XLayerChainSpec, …>` and the
///    constructor updated to wrap `ctx.chain_spec()` into the newtype.
///
/// Pattern reference: tempo's `TempoChainSpec` / `TempoHardforks` split
/// (`/home/po/now/tempo/crates/chainspec/src/{spec,hardfork}.rs`). We
/// deliberately defer the wrapper until a second XLayer fork gives it a
/// reason to exist — introducing it now would add indirection without
/// a user for the extra type parameter.
pub type XLayerEvmConfig = OpEvmConfig<
    OpChainSpec,
    OpPrimitives,
    OpRethReceiptBuilder,
    XLayerAAEvmFactory<XLayerAATransaction<TxEnv>>,
>;

/// Builds a fresh [`XLayerEvmConfig`] over the supplied chain spec.
///
/// Mirrors `OpEvmConfig::optimism(chain_spec)` (the upstream helper that
/// hard-codes `OpEvmFactory<OpTx>`) but plugs in [`XLayerAAEvmFactory`]
/// instead so every EVM built by this config routes through
/// `XLayerAAHandler`.
pub fn xlayer_evm_config(chain_spec: Arc<OpChainSpec>) -> XLayerEvmConfig {
    OpEvmConfig {
        block_assembler: OpBlockAssembler::new(chain_spec.clone()),
        executor_factory: OpBlockExecutorFactory::new(
            OpRethReceiptBuilder::default(),
            chain_spec,
            XLayerAAEvmFactory::<XLayerAATransaction<TxEnv>>::default(),
        ),
        _pd: PhantomData,
    }
}
