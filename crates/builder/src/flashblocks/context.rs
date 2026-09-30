use crate::{
    flashblocks::utils::execution::{ExecutionInfo, TxnExecutionResult},
    metrics::BuilderMetrics,
    signer::Signer,
    traits::PayloadTxsBounds,
};
use std::{sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use alloy_consensus::{
    conditional::BlockConditionalAttributes, transaction::Recovered, Eip658Value, Transaction,
};
use alloy_eips::eip2718::WithEncoded;
use alloy_eips::{Encodable2718, Typed2718};
use alloy_evm::Database;
use alloy_op_evm::{block::receipt_builder::OpReceiptBuilder, block::OpTxEnv, OpEvm, OpEvmContext};
use alloy_primitives::{BlockHash, Bytes, Log, B256, U256};
use alloy_rpc_types_eth::Withdrawals;
use core::fmt::Debug;
use op_alloy_consensus::{OpDepositReceipt, OpTxType};
use op_revm::{L1BlockInfo, OpSpecId};

use rcs_filter::{FilterHandle, LocalDenyReason, PreScreen, Screen, ScreenInput};
use reth_basic_payload_builder::PayloadConfig;
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_evm::{
    eth::receipt_builder::ReceiptBuilderCtx, precompiles::PrecompilesMap, ConfigureEvm, Evm,
    EvmEnv, EvmError, InvalidTxError,
};
use reth_node_api::PayloadBuilderError;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{GaslessContract, OpEvmConfig, OpNextBlockEnvAttributes};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::OpPayloadBuilderAttributes;
use reth_optimism_payload_builder::{
    config::{OpDAConfig, OpGasLimitConfig},
    error::OpPayloadBuilderError,
};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_optimism_txpool::{
    conditional::MaybeConditionalTransaction,
    estimated_da_size::DataAvailabilitySized,
    interop::{is_valid_interop, MaybeInteropTransaction},
};
use reth_payload_builder::PayloadId;
use reth_primitives_traits::{InMemorySize, SealedHeader, SignedTransaction};
use reth_revm::{context::Block, State};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::{
    context::result::ResultAndState, interpreter::as_u64_saturated, DatabaseCommit, Inspector,
};

use super::rcs_capture::{CaptureInvariantError, CaptureOutcome, RcsCaptureControl, RcsInspector};

/// Container type that holds all necessities to build a new payload.
#[derive(Debug)]
pub struct FlashblocksBuilderCtx {
    /// The type that knows how to perform system calls and configure the evm.
    pub evm_config: OpEvmConfig,
    /// The DA config for the payload builder
    pub da_config: OpDAConfig,
    // Gas limit configuration for the payload builder
    pub gas_limit_config: OpGasLimitConfig,
    /// The chainspec
    pub chain_spec: Arc<OpChainSpec>,
    /// How to build the payload.
    pub config: PayloadConfig<OpPayloadBuilderAttributes<OpTransactionSigned>>,
    /// Evm Settings
    pub evm_env: EvmEnv<OpSpecId>,
    /// Block env attributes for the current block.
    pub block_env_attributes: OpNextBlockEnvAttributes,
    /// Marker to check whether the job has been cancelled.
    pub cancel: CancellationToken,
    /// The builder signer
    pub builder_signer: Option<Signer>,
    /// The metrics for the builder
    pub metrics: Arc<BuilderMetrics>,
    /// Max gas that can be used by a transaction.
    pub max_gas_per_txn: Option<u64>,
    /// Configuration for bridge transaction interception.
    pub bridge_intercept_config: xlayer_bridge_intercept::BridgeInterceptConfig,
    /// On-chain gasless whitelist contract, derived from the chain id by `OpEvmConfig` (consensus
    /// uniform with the block executor; `None` on non-gasless chains). Used to detect zero-priced,
    /// whitelisted txs during block building.
    pub gasless_contract: Option<GaslessContract>,
    /// Per-block gas budget for gasless transactions (in gas units). `None` = unlimited.
    pub gasless_block_gas_limit: Option<u64>,
    /// RCS Filter handle. `None` when the risk-control master switch is disabled.
    pub filter: Option<Arc<FilterHandle>>,
}

/// Per-flashblock execution capacity passed to the transaction executor.
#[derive(Debug, Clone, Copy)]
pub(super) struct TransactionLimits {
    pub block_gas: u64,
    pub block_da: Option<u64>,
    pub block_da_footprint: Option<u64>,
}

/// Maps a non-`Complete` [`CaptureOutcome`] to a fail-closed screening decision, emitting a
/// structured `error!` at every real budget-overflow / invariant decision point and recording the
/// matching local-deny reason metric exactly once. Returns `None` only for `Complete` (the caller
/// screens the captured stream instead). An enabled filter receiving `Passthrough` is a
/// state-machine bug and fails closed exactly like an observation-invariant violation — it is never
/// allowed to fall back to raw logs.
///
/// Diagnostic fields are bounded to `tx_hash`, `observed_count`/`attempted`, and `limit`; the
/// transfer list, individual from/to/value tuples, and the observation queue are NEVER logged.
fn fail_closed_screen(
    filter: &FilterHandle,
    tx_hash: B256,
    outcome: &CaptureOutcome,
) -> Option<Screen> {
    match outcome {
        CaptureOutcome::Complete(_) => None,
        CaptureOutcome::LimitExceeded { attempted, limit } => {
            error!(
                target: "rcs_filter::capture",
                %tx_hash,
                observed_count = *attempted,
                limit = *limit,
                "native-transfer observation budget exceeded; failing closed (Screen::Deny)"
            );
            filter.record_local_deny(LocalDenyReason::NativeTransferLimit);
            Some(Screen::Deny)
        }
        // R9 item 1: `RealLogMismatch` is its OWN arm, emitting EXACTLY the four bounded fields —
        // `tx_hash`, `first_bad_index`, `observed`, `expected` — and NEVER the real-log list, the
        // observation queue, or any from/to/value content. It must not be merged with `Passthrough`
        // (the earlier merged arm dropped these fields and logged only `tx_hash`).
        CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::RealLogMismatch { first_bad_index, observed, expected },
        } => {
            error!(
                target: "rcs_filter::capture",
                %tx_hash,
                first_bad_index = *first_bad_index,
                observed = *observed,
                expected = *expected,
                "real-log subsequence mismatch; failing closed (Screen::Deny)"
            );
            filter.record_local_deny(LocalDenyReason::ObservationInvariant);
            Some(Screen::Deny)
        }
        // An enabled filter receiving `Passthrough` is a state-machine bug: it stays a
        // `tx_hash`-only fail-closed log (no extra fields) and never falls back to raw logs. Any
        // future content-free `CaptureInvariantError` kind will force an explicit decision here at
        // compile time, so a new invariant can never silently inherit the mismatch fields.
        CaptureOutcome::Passthrough => {
            error!(
                target: "rcs_filter::capture",
                %tx_hash,
                "capture invariant violated; failing closed (Screen::Deny)"
            );
            filter.record_local_deny(LocalDenyReason::ObservationInvariant);
            Some(Screen::Deny)
        }
    }
}

/// Builder-internal seam letting the panic-safe [`CaptureGuard`] drive the capture lifecycle on the
/// EVM without knowing the concrete inner inspector type. Implemented for the flashblocks `OpEvm`
/// (delegating to the composite's inner inspector via `components_mut`) and, in tests, for a
/// lifecycle-counting double.
trait RcsCaptureScope {
    fn scope_start_capture(&mut self);
    fn scope_finish_capture(&mut self, result_logs: &[Log]) -> CaptureOutcome;
    fn scope_abort_capture(&mut self);
}

impl<DB, I> RcsCaptureScope for OpEvm<DB, I, PrecompilesMap>
where
    DB: Database,
    I: Inspector<OpEvmContext<DB>> + RcsCaptureControl,
{
    fn scope_start_capture(&mut self) {
        self.components_mut().1.start_capture();
    }

    fn scope_finish_capture(&mut self, result_logs: &[Log]) -> CaptureOutcome {
        self.components_mut().1.finish_capture(result_logs)
    }

    fn scope_abort_capture(&mut self) {
        self.components_mut().1.abort_capture();
    }
}

/// Panic-safe RAII capture guard (spec §5.2, R9 item 3). Constructing it calls `start_capture` on
/// the inner inspector; on `Drop` it calls `abort_capture` UNLESS it was explicitly disarmed by a
/// successful [`CaptureGuard::finish`]. This makes cleanup impossible to forget on any early return
/// or unwind — a panic caught upstream still aborts exactly once — so correctness does not rest on
/// caller discipline, and the guard never aborts twice (`finish` disarms before returning; the
/// `Err`/panic path aborts once via `Drop`).
struct CaptureGuard<'a, E: RcsCaptureScope> {
    scope: &'a mut E,
    armed: bool,
}

impl<'a, E: RcsCaptureScope> CaptureGuard<'a, E> {
    /// Arms capture: calls `start_capture` on construction.
    fn new(scope: &'a mut E) -> Self {
        scope.scope_start_capture();
        Self { scope, armed: true }
    }

    /// The armed scope, for driving the real candidate `evm.transact`.
    fn scope_mut(&mut self) -> &mut E {
        self.scope
    }

    /// Closes the capture window normally and disarms the guard so its `Drop` is a no-op — exactly
    /// one lifecycle-ending call (this `finish`), never a `finish` followed by a `Drop` abort.
    fn finish(mut self, result_logs: &[Log]) -> CaptureOutcome {
        let outcome = self.scope.scope_finish_capture(result_logs);
        self.armed = false;
        outcome
    }
}

impl<E: RcsCaptureScope> Drop for CaptureGuard<'_, E> {
    fn drop(&mut self) {
        if self.armed {
            self.scope.scope_abort_capture();
        }
    }
}

impl FlashblocksBuilderCtx {
    pub(super) fn with_cancel(self, cancel: CancellationToken) -> Self {
        Self { cancel, ..self }
    }

    /// Returns the parent block the payload will be build on.
    pub fn parent(&self) -> &SealedHeader {
        &self.config.parent_header
    }

    /// Returns the parent hash
    pub fn parent_hash(&self) -> BlockHash {
        self.parent().hash()
    }

    /// Returns the timestamp
    pub fn timestamp(&self) -> u64 {
        self.attributes().timestamp()
    }

    /// Returns the builder attributes.
    pub(super) const fn attributes(&self) -> &OpPayloadBuilderAttributes<OpTransactionSigned> {
        &self.config.attributes
    }

    /// Returns the withdrawals if shanghai is active.
    pub fn withdrawals(&self) -> Option<&Withdrawals> {
        self.chain_spec
            .is_shanghai_active_at_timestamp(self.attributes().timestamp())
            .then(|| &self.attributes().withdrawals)
    }

    /// Returns the block gas limit to target.
    pub fn block_gas_limit(&self) -> u64 {
        match self.gas_limit_config.gas_limit() {
            Some(gas_limit) => gas_limit,
            None => self.attributes().gas_limit.unwrap_or(self.evm_env.block_env.gas_limit),
        }
    }

    /// Returns the block number for the block.
    pub fn block_number(&self) -> u64 {
        as_u64_saturated!(self.evm_env.block_env.number)
    }

    /// Returns the current base fee
    pub fn base_fee(&self) -> u64 {
        self.evm_env.block_env.basefee
    }

    /// Returns the current blob gas price.
    pub fn get_blob_gasprice(&self) -> Option<u64> {
        self.evm_env.block_env.blob_gasprice().map(|gasprice| gasprice as u64)
    }

    /// Returns the blob fields for the header.
    ///
    /// This will return the culmative DA bytes * scalar after Jovian
    /// after Ecotone, this will always return Some(0) as blobs aren't supported
    /// pre Ecotone, these fields aren't used.
    pub fn blob_fields(&self, info: &ExecutionInfo) -> (Option<u64>, Option<u64>) {
        // For payload validation
        if let Some(blob_fields) = info.optional_blob_fields {
            return blob_fields;
        }
        // Compute from execution info
        if self.is_jovian_active() {
            let scalar =
                info.da_footprint_scalar.expect("Scalar must be defined for Jovian blocks");
            let result = info.cumulative_da_bytes_used * scalar as u64;
            (Some(0), Some(result))
        } else if self.is_ecotone_active() {
            (Some(0), Some(0))
        } else {
            (None, None)
        }
    }

    /// Returns the extra data for the block.
    ///
    /// After holocene this extracts the extradata from the payload
    pub fn extra_data(&self) -> Result<Bytes, PayloadBuilderError> {
        if self.is_jovian_active() {
            self.attributes()
                .get_jovian_extra_data(
                    self.chain_spec.base_fee_params_at_timestamp(self.attributes().timestamp),
                )
                .map_err(PayloadBuilderError::other)
        } else if self.is_holocene_active() {
            self.attributes()
                .get_holocene_extra_data(
                    self.chain_spec.base_fee_params_at_timestamp(self.attributes().timestamp),
                )
                .map_err(PayloadBuilderError::other)
        } else {
            Ok(Default::default())
        }
    }

    /// Returns the current fee settings for transactions from the mempool
    pub fn best_transaction_attributes(&self) -> BestTransactionsAttributes {
        BestTransactionsAttributes::new(self.base_fee(), self.get_blob_gasprice())
    }

    /// Returns the unique id for this payload job.
    pub fn payload_id(&self) -> PayloadId {
        self.attributes().payload_id()
    }

    /// Returns true if regolith is active for the payload.
    pub fn is_regolith_active(&self) -> bool {
        self.chain_spec.is_regolith_active_at_timestamp(self.attributes().timestamp())
    }

    /// Returns true if ecotone is active for the payload.
    pub fn is_ecotone_active(&self) -> bool {
        self.chain_spec.is_ecotone_active_at_timestamp(self.attributes().timestamp())
    }

    /// Returns true if canyon is active for the payload.
    pub fn is_canyon_active(&self) -> bool {
        self.chain_spec.is_canyon_active_at_timestamp(self.attributes().timestamp())
    }

    /// Returns true if holocene is active for the payload.
    pub fn is_holocene_active(&self) -> bool {
        self.chain_spec.is_holocene_active_at_timestamp(self.attributes().timestamp())
    }

    /// Returns true if isthmus is active for the payload.
    pub fn is_isthmus_active(&self) -> bool {
        self.chain_spec.is_isthmus_active_at_timestamp(self.attributes().timestamp())
    }

    /// Returns true if isthmus is active for the payload.
    pub fn is_jovian_active(&self) -> bool {
        self.chain_spec.is_jovian_active_at_timestamp(self.attributes().timestamp())
    }

    /// Returns the chain id
    pub fn chain_id(&self) -> u64 {
        self.chain_spec.chain_id()
    }
}

impl FlashblocksBuilderCtx {
    /// Constructs a receipt for the given transaction.
    pub fn build_receipt<E: Evm>(
        &self,
        ctx: ReceiptBuilderCtx<'_, OpTxType, E>,
        deposit_nonce: Option<u64>,
    ) -> OpReceipt {
        let receipt_builder = self.evm_config.block_executor_factory().receipt_builder();
        match receipt_builder.build_receipt(ctx) {
            Ok(receipt) => receipt,
            Err(ctx) => {
                let receipt = alloy_consensus::Receipt {
                    // Success flag was added in `EIP-658: Embedding transaction status code
                    // in receipts`.
                    status: Eip658Value::Eip658(ctx.result.is_success()),
                    cumulative_gas_used: ctx.cumulative_gas_used,
                    logs: ctx.result.into_logs(),
                };

                receipt_builder.build_deposit_receipt(OpDepositReceipt {
                    inner: receipt,
                    deposit_nonce,
                    // The deposit receipt version was introduced in Canyon to indicate an
                    // update to how receipt hashes should be computed
                    // when set. The state transition process ensures
                    // this is only set for post-Canyon deposit
                    // transactions.
                    deposit_receipt_version: self.is_canyon_active().then_some(1),
                })
            }
        }
    }

    /// Mirrors the gasless detection in the upstream block executor
    /// (`OpBlockExecutor::execute_transaction_without_commit`). The flashblocks builder executes
    /// pool transactions directly via [`Evm::transact`] rather than through the block executor, so
    /// the detection and base-fee relaxation have to be replicated here, otherwise zero-priced
    /// (whitelisted) transactions would be rejected by base-fee validation even when gasless is
    /// enabled.
    #[allow(clippy::type_complexity)]
    fn transact_maybe_gasless<DB, I>(
        &self,
        evm: &mut OpEvm<DB, I, PrecompilesMap>,
        tx: &Recovered<OpTransactionSigned>,
    ) -> Result<
        (ResultAndState<<OpEvm<DB, I, PrecompilesMap> as Evm>::HaltReason>, bool, CaptureOutcome),
        <OpEvm<DB, I, PrecompilesMap> as Evm>::Error,
    >
    where
        DB: Database,
        I: Inspector<OpEvmContext<DB>> + RcsCaptureControl,
    {
        // The gasless pre-check runs `getGaslessAllowance` as a system call on the same EVM. It
        // MUST run with the inspector inactive so the native-transfer observation seam never
        // captures anything from a non-real-user simulation.
        let is_gasless = self.is_gasless(evm, tx)?;
        let mut tx_env = self.evm_config.tx_env(tx);
        tx_env.set_gasless(is_gasless);
        // Gasless design (kona 1.6.0): no separate fee hook. With `is_gasless` set on the tx env,
        // the Optimism execution layer temporarily toggles `cfg.disable_base_fee` for this single
        // tx — a base-fee *validation* bypass only — and restores it after the tx returns (including
        // on error), so the toggle never leaks into the next tx in the block. `block.basefee` is
        // never mutated (`BASEFEE` still reports the real header base fee) and `OpHandler` skips fee
        // charge/reimbursement/reward, so a plain `transact` applies the full gasless policy.
        //
        // Capture is armed only around the real candidate `evm.transact` through a panic-safe RAII
        // guard: `start_capture` on construction, `abort_capture` on `Drop` unless disarmed by a
        // successful `finish`. Every early return / unwind below therefore leaves the inspector
        // inactive without relying on manual cleanup, and abort runs exactly once (spec §5.2, R9
        // item 3). So every early exit downstream sees an inactive inspector.
        let mut guard = CaptureGuard::new(evm);
        // On the `Err` path `?` early-returns while the guard is still armed, so its `Drop` runs a
        // single `abort_capture`; on `Ok`, `guard.finish` disarms it (no double-abort).
        let result = guard.scope_mut().transact(tx_env)?;
        let capture = guard.finish(result.result.logs());
        Ok((result, is_gasless, capture))
    }

    fn is_gasless<DB, I>(
        &self,
        evm: &mut OpEvm<DB, I, PrecompilesMap>,
        tx: &Recovered<OpTransactionSigned>,
    ) -> Result<bool, <OpEvm<DB, I, PrecompilesMap> as Evm>::Error>
    where
        DB: Database,
        I: Inspector<OpEvmContext<DB>>,
    {
        if tx.is_deposit() || tx.max_fee_per_gas() != 0 {
            return Ok(false);
        }
        match self.gasless_contract {
            // `GaslessContract::is_gasless` only fails on an unrecoverable EVM/db error during the
            // uncommitted system call; surface it as an EVM error so the caller can treat the tx
            // as fatal for this build attempt (matching the executor's behavior).
            Some(contract) => contract
                .is_gasless(evm, tx.inner())
                .map_err(|err| revm::context::result::EVMError::Custom(err.to_string())),
            None => Ok(false),
        }
    }

    /// Executes the sequencer-provided txs (`attributes().transactions`) via `transact_maybe_gasless`,
    /// so gasless txs get `is_gasless` set (a plain `evm.transact` would skip them at the base-fee
    /// check). Does NOT apply the per-block gasless gas budget — same reason as `execute_cached_transactions`.
    pub(super) fn execute_sequencer_transactions(
        &self,
        db: &mut State<impl Database>,
    ) -> Result<ExecutionInfo, PayloadBuilderError> {
        let mut info = ExecutionInfo::with_capacity(self.attributes().transactions.len());

        let mut evm = self.evm_config.evm_with_env(&mut *db, self.evm_env.clone());

        for sequencer_tx in &self.attributes().transactions {
            // A sequencer's block should never contain blob transactions.
            if sequencer_tx.value().is_eip4844() {
                return Err(PayloadBuilderError::other(
                    OpPayloadBuilderError::BlobTransactionRejected,
                ));
            }

            // Convert the transaction to a [Recovered<TransactionSigned>]. This is
            // purely for the purposes of utilizing the `evm_config.tx_env`` function.
            // Deposit transactions do not have signatures, so if the tx is a deposit, this
            // will just pull in its `from` address.
            let sequencer_tx = sequencer_tx.value().try_clone_into_recovered().map_err(|_| {
                PayloadBuilderError::other(OpPayloadBuilderError::TransactionEcRecoverFailed)
            })?;

            // Cache the depositor account prior to the state transition for the deposit nonce.
            //
            // Note that this *only* needs to be done post-regolith hardfork, as deposit nonces
            // were not introduced in Bedrock. In addition, regular transactions don't have deposit
            // nonces, so we don't need to touch the DB for those.
            let depositor_nonce = (self.is_regolith_active() && sequencer_tx.is_deposit())
                .then(|| {
                    evm.db_mut()
                        .load_cache_account(sequencer_tx.signer())
                        .map(|acc| acc.account_info().unwrap_or_default().nonce)
                })
                .transpose()
                .map_err(|_| {
                    PayloadBuilderError::other(OpPayloadBuilderError::AccountLoadFailed(
                        sequencer_tx.signer(),
                    ))
                })?;

            let (ResultAndState { result, state }, _is_gasless, _capture) =
                match self.transact_maybe_gasless(&mut evm, &sequencer_tx) {
                    Ok(res) => res,
                    Err(err) => {
                        if err.is_invalid_tx_err() {
                            warn!(
                                target: "payload_builder",
                                id = %self.payload_id(),
                                block_number = self.block_number(),
                                tx_hash = %sequencer_tx.tx_hash(),
                                %err,
                                "Error in sequencer transaction, skipping."
                            );
                            continue;
                        }
                        // this is an error that we should treat as fatal for this attempt
                        return Err(PayloadBuilderError::EvmExecutionError(Box::new(err)));
                    }
                };

            // add gas used by the transaction to cumulative gas used, before creating the receipt
            let gas_used = result.tx_gas_used();
            info.cumulative_gas_used += gas_used;

            if !sequencer_tx.is_deposit() {
                info.cumulative_da_bytes_used += op_alloy_flz::tx_estimated_size_fjord_bytes(
                    sequencer_tx.encoded_2718().as_slice(),
                );
            }

            let ctx = ReceiptBuilderCtx {
                tx_type: sequencer_tx.tx_type(),
                evm: &evm,
                result,
                state: &state,
                cumulative_gas_used: info.cumulative_gas_used,
            };

            info.receipts.push(self.build_receipt(ctx, depositor_nonce));

            // commit changes
            evm.db_mut().commit(state);

            // append sender and transaction to the respective lists
            info.executed_senders.push(sequencer_tx.signer());
            info.executed_transactions.push(sequencer_tx.into_inner());
        }

        let da_footprint_gas_scalar = self
            .chain_spec
            .is_jovian_active_at_timestamp(self.attributes().timestamp())
            .then(|| {
                L1BlockInfo::fetch_da_footprint_gas_scalar(evm.db_mut())
                    .expect("DA footprint should always be available from the database post jovian")
            });

        info.da_footprint_scalar = da_footprint_gas_scalar;

        Ok(info)
    }

    /// Executes cached transactions received via P2P, used to replay previously sequenced flashblock
    /// transactions when the builder changes before the full block is built.
    /// Detects whether each `tx` should execute gaslessly and, if so, executes it through the
    /// standard EVM entry with a transaction-scoped base-fee validation bypass (see
    /// [`Self::transact_maybe_gasless`]) — not a separate fee hook.
    pub(super) fn execute_cached_flashblocks_transactions(
        &self,
        info: &mut ExecutionInfo,
        db: &mut State<impl Database>,
        cached_txs: Vec<WithEncoded<alloy_consensus::transaction::Recovered<OpTransactionSigned>>>,
    ) -> Result<(), PayloadBuilderError> {
        let tx_da_limit = self.da_config.max_da_tx_size();
        let block_gas_limit = self.block_gas_limit();
        let block_da_limit = self.da_config.max_da_block_size();
        let block_da_footprint_limit = info.da_footprint_scalar.map(|_| self.block_gas_limit());

        info!(
            target: "payload_builder",
            message = "Found cached flashblocks sequence transactions from p2p, replaying",
            parent_hash = ?self.parent_hash(),
            cached_tx_count = cached_txs.len(),
            block_da_limit = ?block_da_limit,
            tx_da_limit = ?tx_da_limit,
            block_gas_limit = ?block_gas_limit,
        );

        let mut evm = self.evm_config.evm_with_env(&mut *db, self.evm_env.clone());

        for with_encoded_tx in cached_txs {
            let (encoded_bytes, recovered_tx) = with_encoded_tx.split();
            let sender = recovered_tx.signer();

            // ensure transaction is valid
            let tx_da_size = op_alloy_flz::tx_estimated_size_fjord_bytes(encoded_bytes.as_ref());
            if let Err(result) = info.is_tx_over_limits(
                tx_da_size,
                block_gas_limit,
                tx_da_limit,
                block_da_limit,
                recovered_tx.gas_limit(),
                info.da_footprint_scalar,
                block_da_footprint_limit,
            ) {
                return Err(PayloadBuilderError::Other(
                    eyre::eyre!(
                        "invalid flashblocks sequence, tx {tx_hash} over block limits: {result}",
                        tx_hash = recovered_tx.tx_hash(),
                    )
                    .into(),
                ));
            }
            if recovered_tx.is_eip4844() {
                return Err(PayloadBuilderError::other(
                    OpPayloadBuilderError::BlobTransactionRejected,
                ));
            }
            if recovered_tx.is_deposit() {
                return Err(PayloadBuilderError::Other(
                    eyre::eyre!("invalid flashblocks sequence, deposit transaction rejected")
                        .into(),
                ));
            }

            // Ensure transaction execution is valid.
            let (ResultAndState { result, state }, _is_gasless, _capture) =
                match self.transact_maybe_gasless(&mut evm, &recovered_tx) {
                    Ok(res) => res,
                    Err(err) => {
                        trace!(
                            target: "payload_builder",
                            %err,
                            ?recovered_tx,
                            "Error replaying cached flashblock transaction"
                        );
                        return Err(PayloadBuilderError::EvmExecutionError(Box::new(err)));
                    }
                };

            // Add gas used by the transaction to cumulative gas used
            let gas_used = result.tx_gas_used();
            info.cumulative_gas_used += gas_used;
            // Record tx da size
            info.cumulative_da_bytes_used += tx_da_size;

            // Push transaction changeset and calculate header bloom filter for receipt.
            let ctx = ReceiptBuilderCtx {
                tx_type: recovered_tx.tx_type(),
                evm: &evm,
                result,
                state: &state,
                cumulative_gas_used: info.cumulative_gas_used,
            };
            info.receipts.push(self.build_receipt(ctx, None));

            // Commit changes
            evm.db_mut().commit(state);

            // update add to total fees. Gasless txs contribute no miner fee: they execute with an
            // effective gas price of 0, so `effective_tip_per_gas` returns `None` and `unwrap_or(0)`
            // yields a 0 tip for them (see the equivalent note in `execute_best_transactions`).
            let miner_fee = recovered_tx.effective_tip_per_gas(self.base_fee()).unwrap_or(0);
            info.total_fees += U256::from(miner_fee) * U256::from(gas_used);

            // Append sender and transaction to the respective lists
            info.executed_senders.push(sender);
            info.executed_transactions.push(recovered_tx.into_inner());
        }

        Ok(())
    }

    /// Executes the given best transactions and updates the execution info.
    ///
    /// Returns `Ok(Some(())` if the job was cancelled.
    pub(super) fn execute_best_transactions(
        &self,
        info: &mut ExecutionInfo,
        db: &mut State<impl Database>,
        best_txs: &mut impl PayloadTxsBounds,
        tx_pool: &impl TransactionPool,
        limits: TransactionLimits,
    ) -> Result<Option<()>, PayloadBuilderError> {
        // Two typed EVM-construction branches feed one generic loop. When the RCS Filter is enabled
        // the RCS capture inspector becomes the composite's inner inspector so internal native-token
        // transfers can be observed during the real-user candidate simulation; when it is disabled
        // the zero-overhead no-op inspector is kept and behavior is unchanged.
        if let Some(filter) = self.filter.clone() {
            let inspector = RcsInspector::new(filter.max_native_transfers_per_tx());
            let mut evm = self.evm_config.evm_with_env_and_inspector(
                &mut *db,
                self.evm_env.clone(),
                inspector,
            );
            self.run_tx_loop(&mut evm, info, best_txs, tx_pool, limits)
        } else {
            let mut evm = self.evm_config.evm_with_env(&mut *db, self.evm_env.clone());
            self.run_tx_loop(&mut evm, info, best_txs, tx_pool, limits)
        }
    }

    /// The per-flashblock transaction simulation loop, generic over the inner inspector so the
    /// Filter-disabled (`NoOpInspector`) and Filter-enabled (`RcsInspector`) EVMs share one body.
    /// Native-transfer capture is driven through the [`RcsCaptureControl`] seam bound on `I`.
    fn run_tx_loop<DB, I>(
        &self,
        evm: &mut OpEvm<DB, I, PrecompilesMap>,
        info: &mut ExecutionInfo,
        best_txs: &mut impl PayloadTxsBounds,
        tx_pool: &impl TransactionPool,
        limits: TransactionLimits,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        DB: Database + DatabaseCommit,
        I: Inspector<OpEvmContext<DB>> + RcsCaptureControl,
    {
        let execute_txs_start_time = Instant::now();
        let mut num_txs_considered = 0;
        let mut num_txs_simulated = 0;
        let mut num_txs_simulated_success = 0;
        let mut num_txs_simulated_fail = 0;
        let mut reverted_gas_used = 0;
        let base_fee = self.base_fee();

        let tx_da_limit = self.da_config.max_da_tx_size();

        debug!(
            target: "payload_builder",
            id = ?self.payload_id(),
            block_da_limit = ?limits.block_da,
            tx_da_limit = ?tx_da_limit,
            block_gas_limit = ?limits.block_gas,
            "Executing best transactions",
        );

        let block_attr = BlockConditionalAttributes {
            number: self.block_number(),
            timestamp: self.attributes().timestamp(),
        };

        while let Some(tx) = best_txs.next(()) {
            let interop = tx.interop_deadline();
            let conditional = tx.conditional().cloned();

            let tx_da_size = tx.estimated_da_size();
            let tx = tx.into_consensus();
            let tx_hash = tx.tx_hash();
            let log_txn = |result: TxnExecutionResult| {
                debug!(
                    target: "payload_builder",
                    id = ?self.payload_id(),
                    tx_hash = ?tx_hash,
                    tx_da_size = ?tx_da_size,
                    result = %result,
                    "Considering transaction",
                );
            };

            num_txs_considered += 1;

            // TODO: ideally we should get this from the txpool stream
            if let Some(conditional) = conditional
                && !conditional.matches_block_attributes(&block_attr)
            {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // TODO: remove this condition and feature once we are comfortable enabling interop for everything
            if cfg!(feature = "interop") {
                // We skip invalid cross chain txs, they would be removed on the next block update in
                // the maintenance job
                if let Some(interop) = interop
                    && !is_valid_interop(interop, self.config.attributes.timestamp())
                {
                    log_txn(TxnExecutionResult::InteropFailed);
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    continue;
                }
            }

            // ensure we still have capacity for this transaction
            if let Err(result) = info.is_tx_over_limits(
                tx_da_size,
                limits.block_gas,
                tx_da_limit,
                limits.block_da,
                tx.gas_limit(),
                info.da_footprint_scalar,
                limits.block_da_footprint,
            ) {
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                log_txn(result);
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // A sequencer's block should never contain blob or deposit transactions from the pool.
            if tx.is_eip4844() || tx.is_deposit() {
                log_txn(TxnExecutionResult::SequencerTransaction);
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // check if the job was cancelled, if so we can exit early
            if self.cancel.is_cancelled() {
                return Ok(Some(()));
            }

            // Reuse in-flight RCS state before repeating EVM execution. Deferring a transaction
            // only removes it and its nonce descendants from this iterator, not from txpool.
            if let Some(filter) = self.filter.as_ref() {
                match filter.pre_screen(&tx_hash) {
                    PreScreen::Execute => {}
                    PreScreen::Defer => {
                        best_txs.mark_invalid(tx.signer(), tx.nonce());
                        continue;
                    }
                    PreScreen::Drop => {
                        best_txs.mark_invalid(tx.signer(), tx.nonce());
                        let removed = tx_pool.remove_transaction(tx_hash).is_some();
                        filter.record_txpool_discard(removed);
                        continue;
                    }
                }
            }

            // Once the per-block gasless budget is spent, skip further gasless candidates without
            // simulating them.
            if info.gasless_budget_exhausted && tx.max_fee_per_gas() == 0 {
                log_txn(TxnExecutionResult::GaslessBlockGasLimitExceeded(
                    info.cumulative_gasless_gas_used,
                    0,
                    self.gasless_block_gas_limit.unwrap_or(0),
                ));
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            let tx_simulation_start_time = Instant::now();
            // Gasless: zero-priced, whitelisted txs are executed with a transaction-scoped base-fee
            // validation bypass (a per-tx `cfg.disable_base_fee` toggle applied and restored by the
            // Optimism layer, never a `block.basefee` mutation), gated on the chain's gasless
            // contract approving the tx. Non-gasless txs are unaffected — see
            // [`Self::transact_maybe_gasless`].
            let (ResultAndState { result, state }, is_gasless, capture_outcome) =
                match self.transact_maybe_gasless(&mut *evm, &tx) {
                    Ok(res) => res,
                    Err(err) => {
                        if let Some(err) = err.as_invalid_tx_err() {
                            if err.is_nonce_too_low() {
                                // if the nonce is too low, we can skip this transaction
                                log_txn(TxnExecutionResult::NonceTooLow);
                                warn!(
                                    target: "payload_builder",
                                    id = %self.payload_id(),
                                    block_number = self.block_number(),
                                    tx_hash = %tx.tx_hash(),
                                    %err,
                                    "skipping nonce too low transaction"
                                );
                            } else {
                                // if the transaction is invalid, we can skip it and all of its
                                // descendants
                                log_txn(TxnExecutionResult::InternalError(err.0.clone()));
                                warn!(
                                    target: "payload_builder",
                                    id = %self.payload_id(),
                                    block_number = self.block_number(),
                                    tx_hash = %tx.tx_hash(),
                                    %err,
                                    "skipping invalid transaction and its descendants"
                                );
                                best_txs.mark_invalid(tx.signer(), tx.nonce());
                            }

                            continue;
                        }
                        // this is an error that we should treat as fatal for this attempt
                        log_txn(TxnExecutionResult::EvmError);
                        return Err(PayloadBuilderError::evm(err));
                    }
                };

            self.metrics.tx_simulation_duration.record(tx_simulation_start_time.elapsed());
            self.metrics.tx_byte_size.record(tx.inner().size() as f64);
            num_txs_simulated += 1;

            let gas_used = result.tx_gas_used();

            if result.is_success() {
                log_txn(TxnExecutionResult::Success);
                num_txs_simulated_success += 1;
                self.metrics.successful_tx_gas_used.record(gas_used as f64);
            } else {
                num_txs_simulated_fail += 1;
                reverted_gas_used += gas_used as i32;
                self.metrics.reverted_tx_gas_used.record(gas_used as f64);
                log_txn(TxnExecutionResult::Reverted);
            }

            // add gas used by the transaction to cumulative gas used, before creating the
            // receipt
            if let Some(max_gas_per_txn) = self.max_gas_per_txn
                && gas_used > max_gas_per_txn
            {
                log_txn(TxnExecutionResult::MaxGasUsageExceeded);
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            if is_gasless
                && let Some(limit) = self.gasless_block_gas_limit
                && info.cumulative_gasless_gas_used.saturating_add(gas_used) > limit
            {
                log_txn(TxnExecutionResult::GaslessBlockGasLimitExceeded(
                    info.cumulative_gasless_gas_used,
                    gas_used,
                    limit,
                ));
                if !info.gasless_budget_exhausted {
                    warn!(
                        target: "payload_builder",
                        id = ?self.payload_id(),
                        gasless_gas_used = info.cumulative_gasless_gas_used,
                        limit,
                        "gasless block gas budget exhausted; skipping remaining gasless txs",
                    );
                }
                // Stop simulating further gasless candidates in this block (accross flashblocks).
                info.gasless_budget_exhausted = true;
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // Bridge interception check: if the transaction triggered a bridge event that
            // should be blocked, skip committing state and mark it for pool removal.
            if xlayer_bridge_intercept::intercept_bridge_transaction_if_need(
                result.logs(),
                tx.signer(),
                &self.bridge_intercept_config,
            )
            .is_err()
            {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // Run rule-driven RCS screening after simulation and before committing any state. The
            // observation stream (real logs interleaved with virtual native `Transfer`s) replaces
            // the raw logs; a budget overflow or observation-invariant failure fails closed to
            // `Screen::Deny` via the same downstream handling, never entering the pending buffer.
            if let Some(filter) = self.filter.as_ref() {
                let decision = if let CaptureOutcome::Complete(logs) = &capture_outcome {
                    filter.screen_tx(&ScreenInput {
                        tx_hash,
                        origin: tx.signer(),
                        tx_to: tx.to(),
                        nonce: tx.nonce(),
                        value: tx.value(),
                        block_height: self.block_number(),
                        logs,
                    })
                } else {
                    // Every non-`Complete` outcome fails closed (budget overflow, observation
                    // invariant, or — a state-machine bug — an enabled filter seeing `Passthrough`).
                    fail_closed_screen(filter, tx_hash, &capture_outcome).unwrap_or(Screen::Deny)
                };
                match decision {
                    Screen::Allow | Screen::AuditApproved => {}
                    Screen::Deny => {
                        best_txs.mark_invalid(tx.signer(), tx.nonce());
                        let removed = tx_pool.remove_transaction(tx_hash).is_some();
                        filter.record_txpool_discard(removed);
                        continue;
                    }
                    Screen::Drop => {
                        best_txs.mark_invalid(tx.signer(), tx.nonce());
                        let removed = tx_pool.remove_transaction(tx_hash).is_some();
                        filter.record_txpool_discard(removed);
                        if !removed {
                            debug!(
                                target: "rcs_filter",
                                %tx_hash,
                                "terminally rejected transaction was already absent from txpool"
                            );
                        }
                        continue;
                    }
                    Screen::AuditPending => {
                        best_txs.mark_invalid(tx.signer(), tx.nonce());
                        continue;
                    }
                }
            }

            info.cumulative_gas_used += gas_used;
            if is_gasless {
                info.cumulative_gasless_gas_used += gas_used;
            }
            // record tx da size
            info.cumulative_da_bytes_used += tx_da_size;

            // Push transaction changeset and calculate header bloom filter for receipt.
            let ctx = ReceiptBuilderCtx {
                tx_type: tx.tx_type(),
                evm: &*evm,
                result,
                state: &state,
                cumulative_gas_used: info.cumulative_gas_used,
            };
            info.receipts.push(self.build_receipt(ctx, None));

            // commit changes
            evm.db_mut().commit(state);

            // update add to total fees. Gasless txs contribute no miner fee: they execute with an
            // effective gas price of 0, so `effective_tip_per_gas` returns `None` for a zero-priced
            // tx under a non-zero base fee and `unwrap_or(0)` yields a 0 tip for them.
            let miner_fee = tx.effective_tip_per_gas(base_fee).unwrap_or(0);
            info.total_fees += U256::from(miner_fee) * U256::from(gas_used);

            // append sender and transaction to the respective lists
            info.executed_senders.push(tx.signer());
            info.executed_transactions.push(tx.into_inner());
        }

        let payload_transaction_simulation_time = execute_txs_start_time.elapsed();
        self.metrics.set_payload_builder_metrics(
            payload_transaction_simulation_time,
            num_txs_considered,
            num_txs_simulated,
            num_txs_simulated_success,
            num_txs_simulated_fail,
            reverted_gas_used,
        );

        debug!(
            target: "payload_builder",
            id = ?self.payload_id(),
            txs_executed = num_txs_considered,
            txs_applied = num_txs_simulated_success,
            txs_rejected = num_txs_simulated_fail,
            "Completed executing best transactions",
        );
        Ok(None)
    }
}

#[cfg(test)]
mod capture_decision_tests {
    use super::*;
    use crate::flashblocks::rcs_capture::CaptureInvariantError;

    fn test_filter() -> FilterHandle {
        rcs_filter::FilterHandle::for_test(
            rcs_filter::FilterConfig::default(),
            rcs_filter::rules::RuleSet::default(),
            Arc::new(rcs_filter::SystemClock),
        )
    }

    #[test]
    fn complete_is_not_fail_closed() {
        let filter = test_filter();
        assert_eq!(
            fail_closed_screen(&filter, B256::ZERO, &CaptureOutcome::Complete(vec![])),
            None
        );
    }

    #[test]
    fn limit_exceeded_fails_closed() {
        let filter = test_filter();
        assert_eq!(
            fail_closed_screen(
                &filter,
                B256::ZERO,
                &CaptureOutcome::LimitExceeded { attempted: 3, limit: 2 }
            ),
            Some(Screen::Deny)
        );
    }

    #[test]
    fn invariant_violation_fails_closed() {
        let filter = test_filter();
        let outcome = CaptureOutcome::InvariantViolation {
            reason: CaptureInvariantError::RealLogMismatch {
                first_bad_index: 0,
                observed: 1,
                expected: 2,
            },
        };
        assert_eq!(fail_closed_screen(&filter, B256::ZERO, &outcome), Some(Screen::Deny));
    }

    // The R10 direct-dispatch test 35 has been REWRITTEN (spec §8 test 35, R11; it supersedes the
    // R10 version) into the LOOP-LEVEL builder-path gating test in the `loop_level_fail_closed_tests`
    // module below: that test drives the REAL `execute_best_transactions` / `run_tx_loop` candidate
    // loop with a non-empty `RuleSet` and a real runtime `RealLogMismatch`, instead of calling
    // `fail_closed_screen` directly. The direct `fail_closed_screen` dispatch stays covered here by
    // `invariant_violation_fails_closed` / `enabled_filter_passthrough_fails_closed`, and the real
    // runtime outcome by `test_fixtures::real_runtime_reallogmismatch_outcome` (test 33).

    #[test]
    fn enabled_filter_passthrough_fails_closed() {
        // Review-focus: an enabled filter must never reuse raw logs on a `Passthrough`.
        let filter = test_filter();
        assert_eq!(
            fail_closed_screen(&filter, B256::ZERO, &CaptureOutcome::Passthrough),
            Some(Screen::Deny)
        );
    }

    /// Runs `f` under a capturing `tracing` subscriber and returns its result plus everything it
    /// logged, so a test can assert the level, target, and exact bounded fields of a diagnostic and
    /// prove no content leaks. One helper shared by the capture-diagnostic tests (R9 non-blocking
    /// cleanup: previously each test hand-rolled its own writer).
    fn capture_tracing<R>(f: impl FnOnce() -> R) -> (R, String) {
        use std::io::Write;
        use std::sync::Mutex;

        #[derive(Clone)]
        struct BufWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for BufWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl tracing_subscriber::fmt::MakeWriter<'_> for BufWriter {
            type Writer = BufWriter;
            fn make_writer(&self) -> BufWriter {
                self.clone()
            }
        }

        let buf = BufWriter(Arc::new(Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_target(true)
            .with_writer(buf.clone())
            .finish();
        let out = tracing::subscriber::with_default(subscriber, f);
        let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        (out, logged)
    }

    #[test]
    fn native_overflow_emits_error_with_bounded_fields_only() {
        let filter = test_filter();
        let (decision, out) = capture_tracing(|| {
            fail_closed_screen(
                &filter,
                B256::repeat_byte(0xCD),
                &CaptureOutcome::LimitExceeded { attempted: 10_001, limit: 10_000 },
            )
        });
        assert_eq!(decision, Some(Screen::Deny));
        assert!(out.contains("ERROR"), "expected error-level diagnostic, got: {out}");
        assert!(out.contains("rcs_filter::capture"), "expected target, got: {out}");
        assert!(out.contains("observed_count"), "expected observed_count field, got: {out}");
        assert!(out.contains("10001"), "expected attempted count value, got: {out}");
        assert!(out.contains("limit"), "expected limit field, got: {out}");
    }

    #[test]
    fn realogmismatch_emits_own_arm_four_fields_no_content() {
        // R9 item 1 / spec §8 test 31. The `RealLogMismatch` arm must be DISTINCT from `Passthrough`:
        // it emits EXACTLY tx_hash + first_bad_index + observed + expected at error level, while
        // `Passthrough` (an enabled filter seeing a no-op outcome) stays a tx_hash-only fail-closed
        // log. Neither may leak real-log or transfer content (there is none to leak — the decision
        // fn takes only the tx hash and bounded counts).
        let filter = test_filter();

        let (mismatch_decision, mismatch_out) = capture_tracing(|| {
            fail_closed_screen(
                &filter,
                B256::repeat_byte(0xAB),
                &CaptureOutcome::InvariantViolation {
                    reason: CaptureInvariantError::RealLogMismatch {
                        first_bad_index: 7,
                        observed: 9,
                        expected: 11,
                    },
                },
            )
        });
        assert_eq!(mismatch_decision, Some(Screen::Deny));
        assert!(mismatch_out.contains("ERROR"), "expected error level, got: {mismatch_out}");
        assert!(
            mismatch_out.contains("rcs_filter::capture"),
            "expected target, got: {mismatch_out}"
        );
        assert!(mismatch_out.contains("tx_hash"), "expected tx_hash field, got: {mismatch_out}");
        // The four bounded fields, by exact `field=value`, so a timestamp digit cannot spoof them.
        assert!(
            mismatch_out.contains("first_bad_index=7"),
            "expected first_bad_index=7, got: {mismatch_out}"
        );
        assert!(mismatch_out.contains("observed=9"), "expected observed=9, got: {mismatch_out}");
        assert!(mismatch_out.contains("expected=11"), "expected expected=11, got: {mismatch_out}");
        // It is NOT the LimitExceeded arm (which emits `observed_count`, not `first_bad_index`).
        assert!(
            !mismatch_out.contains("observed_count"),
            "RealLogMismatch must not emit the LimitExceeded `observed_count` field: {mismatch_out}"
        );

        let (passthrough_decision, passthrough_out) = capture_tracing(|| {
            fail_closed_screen(&filter, B256::repeat_byte(0xCD), &CaptureOutcome::Passthrough)
        });
        assert_eq!(passthrough_decision, Some(Screen::Deny));
        assert!(passthrough_out.contains("ERROR"), "expected error level, got: {passthrough_out}");
        assert!(
            passthrough_out.contains("tx_hash"),
            "Passthrough log must carry tx_hash, got: {passthrough_out}"
        );
        // Passthrough is tx_hash-only: it must NOT be merged with the RealLogMismatch arm and must
        // never carry the mismatch fields.
        assert!(
            !passthrough_out.contains("first_bad_index"),
            "Passthrough must not carry RealLogMismatch fields (distinct arms): {passthrough_out}"
        );
        assert!(
            !passthrough_out.contains("expected="),
            "Passthrough must stay tx_hash-only: {passthrough_out}"
        );
    }

    #[test]
    fn capture_guard_disarms_on_success_single_abort_on_err_no_double_abort() {
        // R9 item 3 / spec §8 test 34. Drive the guard against a lifecycle-counting scope (no EVM
        // needed) and assert: (a) a successful `finish` disarms so `Drop` does NOT abort; (b) the
        // `Err` path aborts exactly once via `Drop` (no double-abort); (c) a caught panic still
        // aborts exactly once and leaves the scope inactive.
        #[derive(Default)]
        struct CountingScope {
            starts: u32,
            finishes: u32,
            aborts: u32,
            active: bool,
        }
        impl RcsCaptureScope for CountingScope {
            fn scope_start_capture(&mut self) {
                self.starts += 1;
                self.active = true;
            }
            fn scope_finish_capture(&mut self, _result_logs: &[Log]) -> CaptureOutcome {
                self.finishes += 1;
                self.active = false;
                CaptureOutcome::Complete(Vec::new())
            }
            fn scope_abort_capture(&mut self) {
                self.aborts += 1;
                self.active = false;
            }
        }

        // (a) success → finish disarms → Drop is a no-op.
        let mut scope = CountingScope::default();
        {
            let guard = CaptureGuard::new(&mut scope);
            let outcome = guard.finish(&[]);
            assert!(matches!(outcome, CaptureOutcome::Complete(_)));
        }
        assert_eq!(scope.starts, 1, "start_capture runs once on construction");
        assert_eq!(scope.finishes, 1);
        assert_eq!(scope.aborts, 0, "a disarmed guard must not abort on Drop");
        assert!(!scope.active);

        // (b) Err path → guard dropped armed → exactly one abort, no double-abort.
        let mut scope = CountingScope::default();
        {
            let _guard = CaptureGuard::new(&mut scope);
            // Mirror the `evm.transact` Err path: return without `finish`; guard drops armed.
        }
        assert_eq!(scope.starts, 1);
        assert_eq!(scope.finishes, 0);
        assert_eq!(scope.aborts, 1, "the Err path aborts exactly once via Drop");
        assert!(!scope.active);

        // (c) caught panic → the guard's Drop still aborts once, leaving the scope inactive.
        let mut scope = CountingScope::default();
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // silence the expected panic in test output
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = CaptureGuard::new(&mut scope);
            panic!("simulated transact panic");
        }));
        std::panic::set_hook(prev_hook);
        assert!(result.is_err(), "the panic must propagate out of catch_unwind");
        assert_eq!(scope.starts, 1);
        assert_eq!(scope.aborts, 1, "a caught panic still aborts exactly once via Drop");
        assert!(!scope.active, "the inspector must be left inactive after an unwind");
    }
}

/// Spec §8 **test 35** (R11, GATING; supersedes the R10 direct-dispatch test 35) — a LOOP-LEVEL
/// builder-path fail-closed test driven through the ACTUAL `execute_best_transactions` /
/// `run_tx_loop` candidate loop (§5.1), not a direct `fail_closed_screen` call and not a
/// re-implementation. It proves, together in one test, that a REAL runtime `RealLogMismatch` on the
/// real candidate loop fails closed to `Screen::Deny` and that the loop then evicts the candidate
/// without buffering, committing, or emitting any (virtual) event — the four §5.10.2 requirements
/// a/b/c/d.
#[cfg(test)]
mod loop_level_fail_closed_tests {
    use super::*;
    use crate::flashblocks::rcs_capture::FORCE_REALLOG_MISMATCH;
    use alloy_consensus::TxEip1559;
    use alloy_genesis::Genesis;
    use alloy_primitives::{Address, TxKind};
    use alloy_rpc_types_engine::PayloadAttributes;
    use op_alloy_consensus::OpTypedTransaction;
    use op_alloy_rpc_types_engine::OpPayloadAttributes;
    use rcs_filter::rules::load_rules;
    use rcs_filter::test_support::{golden, log_builder};
    use rcs_filter::{BufferStatus, FilterConfig, SystemClock};
    use reth_optimism_txpool::OpPooledTransaction;
    use reth_payload_util::PayloadTransactions;
    use reth_transaction_pool::test_utils::{testing_pool, MockTransaction};
    use reth_transaction_pool::TransactionOrigin;
    use revm::context::{BlockEnv, CfgEnv};
    use revm::database::{CacheDB, EmptyDB};
    use revm::state::AccountInfo;

    /// A one-shot [`PayloadTransactions`] that yields a single candidate and RECORDS every
    /// `mark_invalid(sender, nonce)` call. `PayloadTransactionsFixed::mark_invalid` is a silent
    /// no-op, which would hide the loop's post-`Screen::Deny` `best_txs.mark_invalid` step; this
    /// wrapper makes that step observable (§8 test 35 (d)(i)).
    struct RecordingTxs {
        tx: Option<OpPooledTransaction>,
        marked: Vec<(Address, u64)>,
    }
    impl PayloadTransactions for RecordingTxs {
        type Transaction = OpPooledTransaction;
        fn next(&mut self, _ctx: ()) -> Option<OpPooledTransaction> {
            self.tx.take()
        }
        fn mark_invalid(&mut self, sender: Address, nonce: u64) {
            self.marked.push((sender, nonce));
        }
    }

    #[tokio::test]
    async fn loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt() {
        // ---- Non-empty RuleSet (RULE_SCENARIO_A): a filter whose rules DO buffer a normal tx. ----
        let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()]);
        let filter =
            Arc::new(FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock)));

        // (b) COMPANION CONTROL — the SAME non-empty rule set routes a *normal* `screen_tx` to
        // `AuditPending` and inserts a `BufferPool` entry. This makes the buffer-absence assertion on
        // the fail-closed path below meaningful: it proves the fail-closed path SKIPPED `screen_tx`,
        // not that the rule set was empty (an empty `RuleSet` + `buffered_len()==0` is NOT proof).
        let control_logs = [log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        let control_input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: U256::ZERO,
            block_height: 1_000_000,
            logs: &control_logs,
        };
        assert_eq!(
            filter.screen_tx(&control_input),
            Screen::AuditPending,
            "control: the non-empty rule set routes a normal tx to AuditPending"
        );
        assert_eq!(
            filter.buffer_status(&golden::tx_a()),
            Some(BufferStatus::NotSubmitted),
            "control: a normal AuditPending tx inserts a BufferPool entry"
        );
        assert_eq!(filter.buffered_len(), 1, "control tx now occupies the pending buffer");
        let buffered_before = filter.buffered_len();

        // ---- Build a REAL FlashblocksBuilderCtx with the RCS Filter enabled. ----
        // Chain spec from the shared test genesis template (known-good OP fork schedule), pinned to
        // chain id 1 and a base fee of 0 (a fixed point that stays 0 under the fee-market update).
        let mut genesis: Genesis =
            serde_json::from_str(include_str!("../tests/framework/artifacts/genesis.json.tmpl"))
                .expect("valid genesis template JSON");
        genesis.config.chain_id = 1;
        genesis.base_fee_per_gas = Some(0u64.into());
        let chain_spec = Arc::new(OpChainSpec::from_genesis(genesis));
        let evm_config = OpEvmConfig::optimism(chain_spec.clone());

        // Hand-built EVM env (same shape as the rcs_capture runtime fixture): the test owns spec and
        // base fee directly, so it does not depend on parent-header / fork-schedule coordination.
        let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
        cfg.chain_id = 1;
        let evm_env = EvmEnv::new(cfg, BlockEnv { gas_limit: 30_000_000, ..Default::default() });

        let payload_id = PayloadId::new([0x35; 8]);
        let rpc_attrs = OpPayloadAttributes {
            payload_attributes: PayloadAttributes {
                timestamp: 1_751_000_002,
                ..Default::default()
            },
            gas_limit: Some(30_000_000),
            ..Default::default()
        };
        let attributes =
            OpPayloadBuilderAttributes::from_rpc_attrs(B256::ZERO, payload_id, rpc_attrs)
                .expect("attrs decode");
        let config = PayloadConfig {
            parent_header: Arc::new(SealedHeader::seal_slow(alloy_consensus::Header::default())),
            parent_block_info: None,
            payload_id,
            attributes,
        };
        let block_env_attributes = OpNextBlockEnvAttributes {
            timestamp: 1_751_000_002,
            suggested_fee_recipient: Address::ZERO,
            prev_randao: B256::ZERO,
            gas_limit: 30_000_000,
            parent_beacon_block_root: None,
            extra_data: Bytes::new(),
        };
        let ctx = FlashblocksBuilderCtx {
            evm_config,
            da_config: OpDAConfig::default(),
            gas_limit_config: OpGasLimitConfig::default(),
            chain_spec,
            config,
            evm_env,
            block_env_attributes,
            cancel: CancellationToken::new(),
            builder_signer: None,
            metrics: Arc::new(BuilderMetrics::default()),
            max_gas_per_txn: None,
            bridge_intercept_config: Default::default(),
            gasless_contract: None,
            gasless_block_gas_limit: None,
            filter: Some(filter.clone()),
        };

        // ---- Real, EVM-executable candidate signed by a funded key. A plain value transfer emits
        // no logs, so the ONLY RealLog divergence is the one the seam injects below. ----
        let signer = crate::tests::funded_signer();
        let candidate = OpTypedTransaction::Eip1559(TxEip1559 {
            chain_id: 1,
            nonce: 0,
            gas_limit: 100_000,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(Address::repeat_byte(0x99)),
            value: U256::ZERO,
            ..Default::default()
        });
        let recovered = signer.sign_tx(candidate).expect("sign candidate");
        let candidate_hash: B256 = recovered.tx_hash();
        let encoded_len = recovered.encode_2718_len();
        let pooled = OpPooledTransaction::new(recovered, encoded_len);

        // Funded in-memory state so `evm.transact` returns `Ok` (reaches the screen decision).
        let mut cache = CacheDB::new(EmptyDB::default());
        cache.insert_account_info(
            signer.address,
            AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
        );
        let mut state = State::builder().with_database(cache).with_bundle_update().build();

        // The txpool holds the candidate so the `Screen::Deny` arm's `tx_pool.remove_transaction`
        // (observed via `record_txpool_discard`) is verifiable as a real pool-state change.
        let pool = testing_pool();
        let mock = MockTransaction::legacy()
            .with_sender(signer.address)
            .with_nonce(0)
            .with_gas_price(100)
            .with_hash(candidate_hash);
        pool.add_transaction(TransactionOrigin::External, mock).await.unwrap();
        assert!(pool.get(&candidate_hash).is_some(), "candidate is in the txpool before the loop");

        let mut best_txs = RecordingTxs { tx: Some(pooled), marked: Vec::new() };
        let mut info = ExecutionInfo::with_capacity(1);
        let limits =
            TransactionLimits { block_gas: 30_000_000, block_da: None, block_da_footprint: None };

        // (a) Controlled inspector-outcome seam REUSED BY THE LOOP (spec §5.10.2 refinement: a seam
        // is allowed only if the candidate loop reuses it, and the outcome must be a REAL
        // `finish_capture` result — not a hand-built `CaptureOutcome`). The loop's own `RcsInspector`
        // runs a real `evm.transact`; armed, `finish_capture` injects one extra real-log entry so the
        // REAL RealLog cross-check diverges and returns a genuine
        // `CaptureOutcome::InvariantViolation { RealLogMismatch }` on the live candidate path.
        FORCE_REALLOG_MISMATCH.with(|f| f.set(true));

        // (c) Drive the ACTUAL candidate loop.
        let result =
            ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);

        // The seam is one-shot and fired exactly once, on the armed candidate's real finish_capture.
        assert!(
            !FORCE_REALLOG_MISMATCH.with(|f| f.get()),
            "the controlled-outcome seam must be consumed by the loop's real finish_capture"
        );
        assert!(
            matches!(result, Ok(None)),
            "loop completes without cancellation/error: {result:?}"
        );

        // (d)(i) the fail-closed candidate is `mark_invalid`'d AND removed from the txpool.
        assert!(
            best_txs.marked.contains(&(signer.address, 0)),
            "the fail-closed candidate must be mark_invalid'd by the loop's Screen::Deny arm"
        );
        assert!(
            pool.get(&candidate_hash).is_none(),
            "the fail-closed candidate must be removed from the txpool (tx_pool.remove_transaction)"
        );

        // (d)(ii) + (d)(v) no execution / no receipt / no commit and NO virtual native event: the
        // `continue` fires before `info.executed_transactions`/`info.receipts`/`evm.db_mut().commit`,
        // so nothing (real or virtual) is written to any receipt/RPC/bloom output on this path.
        assert!(
            info.executed_transactions.is_empty(),
            "fail-closed: no transaction is appended to the block"
        );
        assert!(
            info.receipts.is_empty(),
            "fail-closed: no receipt is pushed (commit skipped) — so no virtual native event is emitted"
        );
        assert_eq!(
            info.cumulative_gas_used, 0,
            "fail-closed: no gas accounted (state was never committed)"
        );

        // (d)(iii) the tx never enters the RCS pending buffer: fail-closed SKIPS `screen_tx` (the only
        // AuditPending -> BufferPool insert path), so the buffer is unchanged and holds no entry for
        // it — meaningful precisely because the control above showed the same rule set DOES buffer.
        assert_eq!(
            filter.buffered_len(),
            buffered_before,
            "fail-closed must not add a BufferPool entry (screen_tx skipped)"
        );
        assert!(
            filter.buffer_status(&candidate_hash).is_none(),
            "no BufferEntry may exist for the failed-closed candidate"
        );

        // (d)(iv) no partial action / no submit: on this path only the metrics-only
        // `record_local_deny(ObservationInvariant)` fires — evidenced together by the empty pending
        // buffer, the absence of any executed tx / receipt, and the txpool eviction above.
    }
}

#[cfg(test)]
mod bridge_intercept_tests {
    use alloy_primitives::{address, LogData};
    use xlayer_bridge_intercept::{
        intercept_bridge_transaction_if_need, BridgeInterceptConfig, BridgeInterceptError,
        BRIDGE_EVENT_SIGNATURE,
    };

    const BRIDGE: alloy_primitives::Address = address!("2a3dd3eb832af982ec71669e178424b10dca2ede");
    const TOKEN: alloy_primitives::Address = address!("75231f58b43240c9718dd58b4967c5114342a86c");
    const OTHER: alloy_primitives::Address = address!("1111111111111111111111111111111111111111");
    const SENDER: alloy_primitives::Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    // Real mainnet BridgeEvent data — same constants as intercept crate tests.
    const DATA1: &str = "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
75231f58b43240c9718dd58b4967c5114342a86c\
0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000bf7624b8a72797fe35ba1505587fc8a39705740c\
000000000000000000000000000000000000000000000000008e1bc9bf040000\
00000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000001c97";

    fn make_bridge_log(addr: alloy_primitives::Address, data_hex: &str) -> alloy_primitives::Log {
        let data_bytes = alloy_primitives::hex::decode(data_hex).expect("valid hex");
        alloy_primitives::Log {
            address: addr,
            data: LogData::new(vec![BRIDGE_EVENT_SIGNATURE], data_bytes.into())
                .expect("valid log data"),
        }
    }

    /// Wildcard mode: any log from the bridge contract must block the transaction.
    /// This exercises the same code path used in `execute_best_transactions`.
    #[test]
    fn test_wildcard_blocks_bridge_tx() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: true,
        };
        let log = make_bridge_log(BRIDGE, DATA1);
        let result = intercept_bridge_transaction_if_need(&[log], SENDER, &config);
        assert!(matches!(result, Err(BridgeInterceptError::WildcardBlock { .. })));
    }

    /// Specific-token mode: only the matching token triggers interception.
    #[test]
    fn test_specific_token_blocks_bridge_tx() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: false,
        };
        let log = make_bridge_log(BRIDGE, DATA1);
        let result = intercept_bridge_transaction_if_need(&[log], SENDER, &config);
        assert!(matches!(result, Err(BridgeInterceptError::TargetTokenBlock { .. })));
    }

    /// A non-target token must not be blocked in specific-token mode.
    #[test]
    fn test_non_target_token_allowed() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: OTHER,
            wildcard: false,
        };
        let log = make_bridge_log(BRIDGE, DATA1);
        assert!(intercept_bridge_transaction_if_need(&[log], SENDER, &config).is_ok());
    }

    /// When the feature is disabled (default config), no transaction must be blocked —
    /// verifying that the hot path has zero overhead.
    #[test]
    fn test_disabled_config_allows_all() {
        let config = BridgeInterceptConfig::default();
        let log = make_bridge_log(BRIDGE, DATA1);
        assert!(intercept_bridge_transaction_if_need(&[log], SENDER, &config).is_ok());
    }
}
