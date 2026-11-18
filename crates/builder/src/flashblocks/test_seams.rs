//! Test seams for this crate's own integration tests (`tests/`), compiled only with the
//! non-default `test-utils` feature.
//!
//! The RCS series' loop-level tests drive the real candidate and sequencer loops and read the
//! loop's test-only probes. Those live behind `pub(super)` / `#[cfg(test)]`, which an
//! integration test cannot reach, so this module exposes exactly what those tests use and
//! nothing else: one-line delegations to the two loops, the shared execution-capacity type they
//! exchange, the startup config builders, and read/reset accessors for the candidate probes.
//! The production items keep their visibility; `test-utils` is enabled from `[dev-dependencies]`
//! only, so no production build compiles this module.

use std::sync::Arc;

use alloy_evm::Database;
use rcs_filter::callstack::CallObservation;
use rcs_filter::{FilterConfig, FilterHandle, Screen};
use reth_node_api::PayloadBuilderError;
use reth_revm::State;
use reth_transaction_pool::TransactionPool;

use super::context::{LAST_CANDIDATE_OBSERVATIONS, LAST_SCREEN_DECISION};
use super::FlashblocksBuilderCtx;
use crate::traits::PayloadTxsBounds;

pub use super::utils::execution::ExecutionInfo;
pub use crate::signer::Signer;

/// The per-flashblock execution capacity the candidate loop takes, re-exported so a test can
/// name the production type directly instead of a field-for-field copy of it.
pub use super::context::TransactionLimits;

/// The two real execution loops, under their production names.
pub trait LoopSeams {
    /// [`FlashblocksBuilderCtx::execute_sequencer_transactions`].
    fn execute_sequencer_transactions(
        &self,
        db: &mut State<impl Database>,
    ) -> Result<ExecutionInfo, PayloadBuilderError>;

    /// [`FlashblocksBuilderCtx::execute_best_transactions`].
    fn execute_best_transactions(
        &self,
        info: &mut ExecutionInfo,
        db: &mut State<impl Database>,
        best_txs: &mut impl PayloadTxsBounds,
        tx_pool: &impl TransactionPool,
        limits: TransactionLimits,
    ) -> Result<Option<()>, PayloadBuilderError>;
}

impl LoopSeams for FlashblocksBuilderCtx {
    fn execute_sequencer_transactions(
        &self,
        db: &mut State<impl Database>,
    ) -> Result<ExecutionInfo, PayloadBuilderError> {
        FlashblocksBuilderCtx::execute_sequencer_transactions(self, db)
    }

    fn execute_best_transactions(
        &self,
        info: &mut ExecutionInfo,
        db: &mut State<impl Database>,
        best_txs: &mut impl PayloadTxsBounds,
        tx_pool: &impl TransactionPool,
        limits: TransactionLimits,
    ) -> Result<Option<()>, PayloadBuilderError> {
        FlashblocksBuilderCtx::execute_best_transactions(self, info, db, best_txs, tx_pool, limits)
    }
}

/// Startup mapping of the RCS Filter CLI args into a [`FilterConfig`].
pub fn build_rcs_filter_config(
    args: &crate::args::RcsFilterArgs,
) -> eyre::Result<Option<FilterConfig>> {
    super::build_rcs_filter_config(args)
}

/// Startup construction of the RCS Filter handle from the CLI args.
pub fn build_rcs_filter_handle(
    args: &crate::args::RcsFilterArgs,
) -> eyre::Result<Option<Arc<FilterHandle>>> {
    super::build_rcs_filter_handle(args)
}

/// The observation set of the last candidate whose capture completed on this thread.
pub fn last_candidate_observations() -> Option<Vec<CallObservation>> {
    LAST_CANDIDATE_OBSERVATIONS.with(|o| o.borrow().clone())
}

pub fn clear_last_candidate_observations() {
    LAST_CANDIDATE_OBSERVATIONS.with(|o| *o.borrow_mut() = None);
}

/// The last screening decision the candidate loop took on this thread.
pub fn last_screen_decision() -> Option<Screen> {
    LAST_SCREEN_DECISION.with(|d| d.get())
}

pub fn clear_last_screen_decision() {
    LAST_SCREEN_DECISION.with(|d| d.set(None));
}
