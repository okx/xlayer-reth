//! Test seams for this crate's own integration tests (`tests/`), compiled only with the
//! non-default `test-utils` feature.
//!
//! The RCS series' loop-level tests drive the real candidate and sequencer loops and read the
//! loop's test-only probes. Those live behind `pub(super)` / `#[cfg(test)]`, which an
//! integration test cannot reach, so this module exposes exactly what those tests use and
//! nothing else: one-line delegations to the two loops, public mirrors of the three
//! module-private types they exchange, and read/reset accessors for the five probes. The
//! production items keep their visibility; `test-utils` is enabled from `[dev-dependencies]`
//! only, so no production build compiles this module.

use std::sync::Arc;

use alloy_evm::Database;
use rcs_filter::callstack::CallObservation;
use rcs_filter::{FilterConfig, FilterHandle, Screen};
use reth_node_api::PayloadBuilderError;
use reth_revm::State;
use reth_transaction_pool::TransactionPool;

use super::context::{
    self, DEPOSIT_INSPECTOR_CONSTRUCTIONS, LAST_CANDIDATE_OBSERVATIONS,
    LAST_DEPOSIT_IDENTIFICATION, LAST_DEPOSIT_OBSERVATIONS, LAST_SCREEN_DECISION,
};
use super::FlashblocksBuilderCtx;
use crate::traits::PayloadTxsBounds;

pub use super::utils::execution::ExecutionInfo;
pub use crate::signer::Signer;

/// Public mirror of the per-flashblock execution capacity the candidate loop takes.
#[derive(Debug, Clone, Copy)]
pub struct TransactionLimits {
    pub block_gas: u64,
    pub block_da: Option<u64>,
    pub block_da_footprint: Option<u64>,
}

impl From<TransactionLimits> for context::TransactionLimits {
    fn from(limits: TransactionLimits) -> Self {
        let TransactionLimits { block_gas, block_da, block_da_footprint } = limits;
        Self { block_gas, block_da, block_da_footprint }
    }
}

/// Public mirror of why a deposit's executed code could not be identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepositNotIdentified {
    NotCaptured,
    Incomplete,
    BudgetExceeded,
    SnapshotMismatch,
    MatchBudget,
}

impl From<context::DepositNotIdentified> for DepositNotIdentified {
    fn from(reason: context::DepositNotIdentified) -> Self {
        match reason {
            context::DepositNotIdentified::NotCaptured => Self::NotCaptured,
            context::DepositNotIdentified::Incomplete => Self::Incomplete,
            context::DepositNotIdentified::BudgetExceeded => Self::BudgetExceeded,
            context::DepositNotIdentified::SnapshotMismatch => Self::SnapshotMismatch,
            context::DepositNotIdentified::MatchBudget => Self::MatchBudget,
        }
    }
}

/// Public mirror of what the sequencer path concluded about one deposit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepositIdentification {
    Allow,
    DenyWorthy,
    AuditWorthy,
    NotIdentified(DepositNotIdentified),
}

impl From<context::DepositIdentification> for DepositIdentification {
    fn from(identification: context::DepositIdentification) -> Self {
        match identification {
            context::DepositIdentification::Allow => Self::Allow,
            context::DepositIdentification::DenyWorthy => Self::DenyWorthy,
            context::DepositIdentification::AuditWorthy => Self::AuditWorthy,
            context::DepositIdentification::NotIdentified(reason) => {
                Self::NotIdentified(reason.into())
            }
        }
    }
}

/// The two real execution loops, under their production names.
pub trait LoopSeams {
    /// [`FlashblocksBuilderCtx::execute_sequencer_transactions`].
    fn execute_sequencer_transactions<DB: Database>(
        &self,
        db: &mut State<DB>,
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
    fn execute_sequencer_transactions<DB: Database>(
        &self,
        db: &mut State<DB>,
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
        FlashblocksBuilderCtx::execute_best_transactions(
            self,
            info,
            db,
            best_txs,
            tx_pool,
            limits.into(),
        )
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

/// Number of deposit-path inspectors constructed on this thread since the last reset.
pub fn deposit_inspector_constructions() -> usize {
    DEPOSIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.get())
}

pub fn reset_deposit_inspector_constructions() {
    DEPOSIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.set(0));
}

/// The last deposit identification the sequencer path produced on this thread.
pub fn last_deposit_identification() -> Option<DepositIdentification> {
    LAST_DEPOSIT_IDENTIFICATION.with(|d| d.get()).map(Into::into)
}

pub fn clear_last_deposit_identification() {
    LAST_DEPOSIT_IDENTIFICATION.with(|d| d.set(None));
}

/// The observation set of the last deposit whose capture completed on this thread.
pub fn last_deposit_observations() -> Option<Vec<CallObservation>> {
    LAST_DEPOSIT_OBSERVATIONS.with(|o| o.borrow().clone())
}

pub fn clear_last_deposit_observations() {
    LAST_DEPOSIT_OBSERVATIONS.with(|o| *o.borrow_mut() = None);
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
