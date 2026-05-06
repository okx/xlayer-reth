//! [`JitOpEvm`] and [`JitEvmFactory`] — community-port architecture.
//!
//! [`JitOpEvm`] wraps `JitEvm<op_revm::OpEvm>` (revmc community PR #139 architecture):
//! the `JitEvm` wrapper overrides [`EvmTr::frame_run`] so every interpreter frame consults
//! the [`JitBackend`] for a compiled function. On miss the backend either returns
//! `Interpret(reason)` (and we fall back to the interpreter for this frame) or schedules
//! background compilation (and falls back this time).
//!
//! Compilation, hotness tracking, eviction, and memory accounting live entirely in
//! [`revmc::JitBackend`]; this module is just the OP Stack adapter.

use alloy_evm::{Database, Evm, EvmEnv, EvmFactory, precompiles::PrecompilesMap};
use alloy_op_evm::{
    OpTx,
    error::{OpTxError, map_op_err},
};
use alloy_primitives::{Address, Bytes};
use op_revm::{
    DefaultOp, OpBuilder, OpContext, OpHaltReason, OpSpecId, OpTransaction, OpTransactionError,
    handler::OpHandler,
    precompiles::OpPrecompiles,
};
use revm::{
    Context,
    context::{BlockEnv, ContextSetters, TxEnv},
    context_interface::{
        JournalTr,
        result::{EVMError, ResultAndState},
    },
    handler::{
        EthFrame, Handler, PrecompileProvider, instructions::EthInstructions,
        system_call::SystemCallEvm,
    },
    inspector::{InspectorHandler, NoOpInspector},
    interpreter::{InterpreterResult, interpreter::EthInterpreter},
    Inspector,
};
use revmc_runtime::{revm_evm::JitEvm, runtime::JitBackend};

/// Inner non-JIT EVM type (the standard `op_revm::OpEvm`).
type InnerOpEvm<DB, I, P> =
    op_revm::OpEvm<OpContext<DB>, I, EthInstructions<EthInterpreter, OpContext<DB>>, P>;

/// JIT-wrapped inner EVM. The `JitEvm` wrapper intercepts `frame_run` to dispatch through the
/// JIT backend; everything else delegates to the inner `OpEvm`.
type JittedInnerEvm<DB, I, P> = JitEvm<InnerOpEvm<DB, I, P>>;

/// JIT-accelerated OP Stack EVM.
///
/// Holds a `JitEvm<OpEvm>` and runs OP-specific transaction logic via [`OpHandler`].
/// `OpHandler` operates on the wrapper, so each `frame_run` call goes through the JIT
/// override. The JIT backend is shared across factories via `Arc` (cheaply clonable).
#[allow(missing_debug_implementations)]
pub struct JitOpEvm<DB: Database, I, P = PrecompilesMap> {
    /// `JitEvm<OpEvm>` — handler will operate on this.
    inner: JittedInnerEvm<DB, I, P>,
    /// Whether the configured inspector should be invoked.
    inspect: bool,
}

impl<DB: Database, I, P> JitOpEvm<DB, I, P> {
    /// Immutable reference to the OP EVM context.
    pub fn ctx(&self) -> &OpContext<DB> {
        &self.inner.inner().0.ctx
    }

    /// Mutable reference to the OP EVM context.
    pub fn ctx_mut(&mut self) -> &mut OpContext<DB> {
        &mut self.inner.inner_mut().0.ctx
    }
}

impl<DB, I, P> JitOpEvm<DB, I, P>
where
    DB: Database,
    P: PrecompileProvider<OpContext<DB>, Output = InterpreterResult>,
{
    /// Creates a new JIT-wrapped OP EVM.
    pub fn new(op_evm: InnerOpEvm<DB, I, P>, backend: JitBackend, inspect: bool) -> Self {
        Self { inner: JitEvm::new(op_evm, backend), inspect }
    }
}

impl<DB: Database, I, P> core::ops::Deref for JitOpEvm<DB, I, P> {
    type Target = OpContext<DB>;
    fn deref(&self) -> &Self::Target {
        self.ctx()
    }
}

impl<DB: Database, I, P> core::ops::DerefMut for JitOpEvm<DB, I, P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_mut()
    }
}

impl<DB, I, P> Evm for JitOpEvm<DB, I, P>
where
    DB: Database,
    I: Inspector<OpContext<DB>>,
    P: PrecompileProvider<OpContext<DB>, Output = InterpreterResult>,
{
    type DB = DB;
    type Tx = OpTx;
    type Error = EVMError<DB::Error, OpTxError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = P;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        &self.inner.inner().0.ctx.block
    }

    fn cfg_env(&self) -> &revm::context::CfgEnv<OpSpecId> {
        &self.inner.inner().0.ctx.cfg
    }

    fn chain_id(&self) -> u64 {
        self.inner.inner().0.ctx.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        // OpTx → OpTransaction<TxEnv>
        let inner_tx: OpTransaction<TxEnv> = tx.into();
        // Set tx on the actual `OpEvm`'s context.
        self.inner.inner_mut().0.ctx.set_tx(inner_tx);
        // Run with OpHandler — it calls `evm.frame_run()` internally, which routes through
        // JitEvm's override. inspect=true → inspect_run; else → run.
        let result: Result<_, EVMError<DB::Error, OpTransactionError>> = if self.inspect {
            let mut handler = OpHandler::<
                JittedInnerEvm<DB, I, P>,
                EVMError<DB::Error, OpTransactionError>,
                EthFrame<EthInterpreter>,
            >::new();
            handler.inspect_run(&mut self.inner)
        } else {
            let mut handler = OpHandler::<
                JittedInnerEvm<DB, I, P>,
                EVMError<DB::Error, OpTransactionError>,
                EthFrame<EthInterpreter>,
            >::new();
            handler.run(&mut self.inner)
        };
        // Always finalize.
        let state = self.inner.inner_mut().0.ctx.journaled_state.finalize();
        let exec = result.map_err(map_op_err)?;
        Ok(ResultAndState::new(exec, state))
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        // Route through the inner OpEvm's standard system-call path. System calls are
        // not perf-critical and don't benefit from JIT; bypassing simplifies things.
        let exec_and_state = self
            .inner
            .inner_mut()
            .system_call_with_caller(caller, contract, data)
            .map_err(map_op_err)?;
        Ok(ResultAndState::new(exec_and_state.result, exec_and_state.state))
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Context { block: block_env, cfg: cfg_env, journaled_state, .. } =
            self.inner.into_inner().0.ctx;
        (journaled_state.database, EvmEnv::new(cfg_env, block_env))
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        let inner = self.inner.inner();
        (&inner.0.ctx.journaled_state.database, &inner.0.inspector, &inner.0.precompiles)
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        let inner = self.inner.inner_mut();
        (
            &mut inner.0.ctx.journaled_state.database,
            &mut inner.0.inspector,
            &mut inner.0.precompiles,
        )
    }
}

// ── Factory ──────────────────────────────────────────────────────────────────

/// Factory that produces [`JitOpEvm`] instances sharing a single [`JitBackend`].
#[derive(Clone, Debug)]
pub struct JitEvmFactory {
    backend: JitBackend,
}

impl JitEvmFactory {
    /// Creates a factory backed by the given `JitBackend` (shared via Arc internally).
    pub fn new(backend: JitBackend) -> Self {
        Self { backend }
    }

    /// Creates a factory with a disabled backend (no compilation, all-interpret).
    pub fn disabled() -> Self {
        Self { backend: JitBackend::disabled() }
    }

    /// Returns a clone of the shared backend handle.
    pub fn backend(&self) -> &JitBackend {
        &self.backend
    }
}

impl Default for JitEvmFactory {
    fn default() -> Self {
        Self::disabled()
    }
}

impl EvmFactory for JitEvmFactory {
    type Evm<DB: Database, I: Inspector<OpContext<DB>>> = JitOpEvm<DB, I, PrecompilesMap>;
    type Context<DB: Database> = OpContext<DB>;
    type Tx = OpTx;
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
        let spec_id = input.cfg_env.spec;
        let inner = Context::op()
            .with_db(db)
            .with_block(input.block_env)
            .with_cfg(input.cfg_env)
            .build_op_with_inspector(NoOpInspector {})
            .with_precompiles(PrecompilesMap::from_static(
                OpPrecompiles::new_with_spec(spec_id).precompiles(),
            ));
        JitOpEvm::new(inner, self.backend.clone(), false)
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let spec_id = input.cfg_env.spec;
        let inner = Context::op()
            .with_db(db)
            .with_block(input.block_env)
            .with_cfg(input.cfg_env)
            .build_op_with_inspector(inspector)
            .with_precompiles(PrecompilesMap::from_static(
                OpPrecompiles::new_with_spec(spec_id).precompiles(),
            ));
        JitOpEvm::new(inner, self.backend.clone(), true)
    }
}
