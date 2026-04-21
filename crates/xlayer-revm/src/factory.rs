//! [`XLayerAAEvmFactory`] — the [`EvmFactory`] the node wires into its
//! `ConfigureEvm` type.
//!
//! Produces [`XLayerAAEvm`]s whose `transact_raw` routes through
//! [`XLayerAAHandler`]. This is the single insertion point required to get
//! XLayerAA tx support on both the validator (block ingestion) and builder
//! (block production) paths — op-reth's `OpEvmConfig<_, _, _, EvmFactory>`
//! is already generic over the factory, so swapping this in touches no
//! upstream code.

use core::{fmt::Debug, marker::PhantomData};

use alloy_evm::{precompiles::PrecompilesMap, Database, EvmEnv, EvmFactory, IntoTxEnv};
use alloy_op_evm::error::OpTxError;
use op_revm::{precompiles::OpPrecompiles, L1BlockInfo, OpHaltReason, OpSpecId};
use revm::{
    context::{BlockEnv, TxEnv},
    context_interface::result::EVMError,
    inspector::NoOpInspector,
    Context, MainContext,
};

use crate::{
    evm::{XLayerAAContext, XLayerAAEvm},
    tx_env::XLayerAATransaction,
};

/// XLayerAA EVM factory. Generic over the outward-facing `Tx` type so
/// downstream crates can wrap `XLayerAATransaction<TxEnv>` in a newtype if
/// they need additional trait bounds (e.g. `FromRecoveredTx`).
pub struct XLayerAAEvmFactory<Tx = XLayerAATransaction<TxEnv>>(PhantomData<Tx>);

impl<Tx> Clone for XLayerAAEvmFactory<Tx> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Tx> Copy for XLayerAAEvmFactory<Tx> {}

impl<Tx> Default for XLayerAAEvmFactory<Tx> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<Tx> Debug for XLayerAAEvmFactory<Tx> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("XLayerAAEvmFactory").finish()
    }
}

impl<Tx> EvmFactory for XLayerAAEvmFactory<Tx>
where
    Tx: IntoTxEnv<Tx> + Into<XLayerAATransaction<TxEnv>> + Default + Clone + Debug,
{
    type Evm<DB: Database, I: revm::inspector::Inspector<XLayerAAContext<DB>>> =
        XLayerAAEvm<DB, I, PrecompilesMap, Tx>;
    type Context<DB: Database> = XLayerAAContext<DB>;
    type Tx = Tx;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError, OpTxError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId>,
    ) -> Self::Evm<DB, NoOpInspector> {
        let spec = input.cfg_env.spec;
        let ctx: XLayerAAContext<DB> = Context::mainnet()
            .with_tx(XLayerAATransaction::default())
            .with_cfg(input.cfg_env)
            .with_chain(L1BlockInfo::default())
            .with_block(input.block_env)
            .with_db(db);

        // XLayerAA precompile decorator layered onto the stock op
        // precompile set for `spec`. We use `PrecompilesMap` (not a direct
        // `PrecompileProvider` impl) so downstream callers can add or
        // override entries at runtime without pattern-matching on the
        // concrete type.
        let precompiles =
            PrecompilesMap::from_static(OpPrecompiles::new_with_spec(spec).precompiles());

        let inner = op_revm::OpEvm::new(ctx, NoOpInspector {}).with_precompiles(precompiles);
        XLayerAAEvm::new(inner, false)
    }

    fn create_evm_with_inspector<DB: Database, I: revm::inspector::Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let spec = input.cfg_env.spec;
        let ctx: XLayerAAContext<DB> = Context::mainnet()
            .with_tx(XLayerAATransaction::default())
            .with_cfg(input.cfg_env)
            .with_chain(L1BlockInfo::default())
            .with_block(input.block_env)
            .with_db(db);

        let precompiles =
            PrecompilesMap::from_static(OpPrecompiles::new_with_spec(spec).precompiles());
        let inner = op_revm::OpEvm::new(ctx, inspector).with_precompiles(precompiles);
        XLayerAAEvm::new(inner, true)
    }
}
