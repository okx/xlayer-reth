//! [`XLayerAAEvm`] — the [`Evm`] impl that routes every transaction
//! through [`XLayerAAHandler`].
//!
//! This is the decorator glue. Internally we still build an
//! `op_revm::OpEvm` so all of the op-stack lifecycle (gas, L1 cost, context
//! setters) works unchanged; on every `transact_raw` we swap the handler
//! out for [`XLayerAAHandler`] so type-`0x7B` transactions get the AA
//! branches and everything else delegates down to the upstream
//! `op_revm::OpHandler`.

use alloy_evm::{Database, Evm, EvmEnv, IntoTxEnv};
use alloy_op_evm::error::{map_op_err, OpTxError};
use alloy_primitives::{Address, Bytes};
use core::{
    fmt::Debug,
    marker::PhantomData,
    ops::{Deref, DerefMut},
};
use op_revm::{transaction::error::OpTransactionError, L1BlockInfo, OpHaltReason, OpSpecId};
use revm::{
    context::{result::ExecResultAndState, BlockEnv, CfgEnv, TxEnv},
    context_interface::{result::EVMError, ContextTr, JournalTr},
    handler::{
        instructions::EthInstructions, EthFrame, Handler, PrecompileProvider, SystemCallEvm,
    },
    inspector::Inspector,
    interpreter::{interpreter::EthInterpreter, InterpreterResult},
    Context, Journal,
};

use crate::{handler::XLayerAAHandler, tx_env::XLayerAATransaction};

/// Type alias for the full context shape the AA EVM operates on.
pub type XLayerAAContext<DB> =
    Context<BlockEnv, XLayerAATransaction<TxEnv>, CfgEnv<OpSpecId>, DB, Journal<DB>, L1BlockInfo>;

/// XLayerAA EVM.
///
/// Wraps an [`op_revm::OpEvm`] constructed over [`XLayerAAContext`] so that
/// every non-handler API surface (context accessors, block/cfg env,
/// precompile slots) remains the upstream op-stack one. `transact_raw` is
/// the one non-delegated path: it runs [`XLayerAAHandler`] directly, which
/// branches AA vs. deposit vs. regular internally.
pub struct XLayerAAEvm<DB: Database, I, P, Tx = XLayerAATransaction<TxEnv>> {
    inner: op_revm::OpEvm<
        XLayerAAContext<DB>,
        I,
        EthInstructions<EthInterpreter, XLayerAAContext<DB>>,
        P,
    >,
    inspect: bool,
    _tx: PhantomData<Tx>,
}

impl<DB: Database, I, P, Tx> Debug for XLayerAAEvm<DB, I, P, Tx> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("XLayerAAEvm").field("inspect", &self.inspect).finish_non_exhaustive()
    }
}

impl<DB: Database, I, P, Tx> XLayerAAEvm<DB, I, P, Tx> {
    /// Wraps an already-constructed `op_revm::OpEvm`.
    pub const fn new(
        evm: op_revm::OpEvm<
            XLayerAAContext<DB>,
            I,
            EthInstructions<EthInterpreter, XLayerAAContext<DB>>,
            P,
        >,
        inspect: bool,
    ) -> Self {
        Self { inner: evm, inspect, _tx: PhantomData }
    }

    /// Shared reference to the underlying context.
    pub const fn ctx(&self) -> &XLayerAAContext<DB> {
        &self.inner.0.ctx
    }

    /// Mutable reference to the underlying context.
    pub const fn ctx_mut(&mut self) -> &mut XLayerAAContext<DB> {
        &mut self.inner.0.ctx
    }
}

impl<DB: Database, I, P, Tx> Deref for XLayerAAEvm<DB, I, P, Tx> {
    type Target = XLayerAAContext<DB>;
    fn deref(&self) -> &Self::Target {
        self.ctx()
    }
}

impl<DB: Database, I, P, Tx> DerefMut for XLayerAAEvm<DB, I, P, Tx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_mut()
    }
}

impl<DB, I, P, Tx> Evm for XLayerAAEvm<DB, I, P, Tx>
where
    DB: Database,
    I: Inspector<XLayerAAContext<DB>, EthInterpreter>,
    P: PrecompileProvider<XLayerAAContext<DB>, Output = InterpreterResult>,
    Tx: IntoTxEnv<Tx> + Into<XLayerAATransaction<TxEnv>>,
{
    type DB = DB;
    type Tx = Tx;
    type Error = EVMError<DB::Error, OpTxError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = P;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        &self.ctx().block
    }

    fn chain_id(&self) -> u64 {
        self.ctx().cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<
        ExecResultAndState<revm::context_interface::result::ExecutionResult<Self::HaltReason>>,
        Self::Error,
    > {
        // Install the AA-capable tx on the context; the handler reads it
        // from `ctx.tx()` in each phase.
        let aa_tx: XLayerAATransaction<TxEnv> = tx.into();
        self.ctx_mut().tx = aa_tx;

        // Run the XLayerAA-aware handler. It dispatches internally —
        // deposit / regular flows go through `op_revm::OpHandler`, AA
        // transactions get the XLayerAA branches. Then snapshot the state
        // diff from the journal, matching `op_revm::OpEvm::replay`.
        let mut handler = XLayerAAHandler::<
            op_revm::OpEvm<
                XLayerAAContext<DB>,
                I,
                EthInstructions<EthInterpreter, XLayerAAContext<DB>>,
                P,
            >,
            EVMError<DB::Error, OpTransactionError>,
            EthFrame<EthInterpreter>,
        >::new();
        let result = handler.run(&mut self.inner).map_err(map_op_err)?;
        let state = self.inner.0.ctx.journal_mut().finalize();
        Ok(ExecResultAndState::new(result, state))
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<
        ExecResultAndState<revm::context_interface::result::ExecutionResult<Self::HaltReason>>,
        Self::Error,
    > {
        // System calls (EIP-2935 historical block hashes, EIP-4788 beacon
        // roots, EIP-7002 / EIP-7251 consolidation requests) are
        // synthesized by the block executor at the start/end of every
        // block — they carry no AA payload, so we delegate straight to
        // `op_revm::OpEvm`'s system-call entry point, mirroring
        // [`alloy_op_evm::OpEvm::transact_system_call`]. The system tx
        // built inside `system_call_with_caller` is independent of
        // `ctx.tx`, so we don't need to install an `XLayerAATransaction`
        // first.
        self.inner.system_call_with_caller(caller, contract, data).map_err(map_op_err)
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Context { block: block_env, cfg: cfg_env, journaled_state, .. } = self.inner.0.ctx;
        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        (
            &self.inner.0.ctx.journaled_state.database,
            &self.inner.0.inspector,
            &self.inner.0.precompiles,
        )
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        (
            &mut self.inner.0.ctx.journaled_state.database,
            &mut self.inner.0.inspector,
            &mut self.inner.0.precompiles,
        )
    }
}
