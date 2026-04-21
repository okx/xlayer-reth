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
