//! `xlayer_auditTransactions` JSON-RPC method: builds deposit txs from the request and executes
//! them **one at a time, in order**, against chain-tip state. `Allow` commits full state. `Deny`
//! discards the simulated state and continues. `Audit` discards the simulated state and stops,
//! leaving the tail `Unknown`. Native blacklist interception occurs inside EVM execution before
//! matcher classification; RCS synthetic-ban replay is responsible for making denied force
//! deposits reach that path.
//! A tx whose `source_hash` is in the request's `known_allowed` set skips this classification
//! entirely — it's executed and its state committed unconditionally as `Allow` (see
//! `AuditTransactionsRequest::known_allowed`'s doc comment for why this bypass is necessary).
//!
//! When the request declares at least one call-frame selector, the shared capture inspector is
//! attached in `CallstackOnly` mode and each transaction has its own observation budget
//! (`AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX`, 1 000). An ordinary entry whose observations cannot
//! be adjudicated is `malformed` and not committed. A `known_allowed` entry ignores its capture
//! outcome.
//!
//! This handler does no side-effecting screening (never touches `FilterHandle::screen_tx` or its
//! `BufferPool`): it runs a throwaway EVM overlay seeded from the current chain-tip state and
//! discards it after producing verdicts.
//!
//! State anchoring and task placement follow the same `Call` helper (`spawn_with_state_at_block`)
//! that `eth_call`/`eth_estimateGas` use, so this handler's EVM execution runs on reth's blocking
//! task pool rather than the async runtime thread.

use std::collections::HashSet;
use std::sync::Arc;

use alloy_consensus::transaction::Recovered;
use alloy_consensus::Transaction;
use alloy_eips::BlockId;
use alloy_evm::{Database, Evm, EvmFactory, IntoTxEnv};
use alloy_op_evm::OpEvmFactory;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use op_revm::OpSpecId;
use rcs_filter::audit::{callstack_only_inspector, observations_from, unadjudicable_observations};
use rcs_filter::callstack::ObservationUnavailable;
use rcs_filter::capture::{
    CaptureGuard, CaptureOutcome, RcsCaptureControl, RcsCaptureScope, RcsInspector,
};
use rcs_filter::rules::{load_rules, RawRule, RuleSet};
use reth_evm::{block::BlockExecutorFactory, ConfigureEvm, EvmEnv};
use reth_optimism_primitives::OpTransactionSigned;
use reth_rpc_eth_api::{
    helpers::{Call, LoadState},
    FromEvmError, RpcNodeCore,
};
use reth_storage_api::BlockReaderIdExt;
use revm::{context_interface::result::ResultAndState, DatabaseCommit};

use super::deposit::{build_deposit_tx, DepositTxRequest};
use super::verdict::AuditResult;
use super::verdict::ExecutedTx;
use super::verdict::Verdict;

/// Decides whether to commit one classified transaction's full simulated state and whether
/// ordered execution should continue. A blacklist FailedDeposit is produced only by the EVM's
/// native blacklist interceptor before classification, never synthesized here.
fn commit_and_continue(verdict: Verdict) -> (bool, bool) {
    match verdict {
        Verdict::Allow => (true, true),
        Verdict::Deny => (false, true),
        Verdict::Audit => (false, false),
        Verdict::Malformed => (false, true),
        Verdict::Unknown => unreachable!("verdict_for never produces {verdict:?}"),
    }
}

/// Builds the final, request-ordered `Vec<AuditResult>` from the per-exec-index execution
/// outcomes. `built[i]` maps original request index `i` to an index into `executed` (or `None`
/// if `build_deposit_tx` failed for that item — always `Malformed`, independent of execution).
/// `executed[exec_idx]` is `None` only when the single-pass execution loop stopped (on `Audit`)
/// before reaching that index — those become `Unknown`.
fn assemble_results(
    txs: &[DepositTxRequest],
    built: &[Option<usize>],
    executed: &mut [Option<AuditResult>],
) -> Vec<AuditResult> {
    let mut results = Vec::with_capacity(txs.len());
    for (i, tx_req) in txs.iter().enumerate() {
        match built[i] {
            None => results.push(AuditResult::malformed(tx_req.source_hash.clone())),
            Some(exec_idx) => match executed[exec_idx].take() {
                Some(result) => results.push(result),
                None => results.push(AuditResult::unknown(tx_req.source_hash.clone())),
            },
        }
    }
    results
}

/// Builds the forced-`Allow` result for a tx whose `source_hash` is in the caller-supplied
/// `known_allowed` set — skips rule (re-)classification entirely (see
/// `AuditTransactionsRequest::known_allowed`'s doc comment for why).
fn known_allowed_result(tx_hash: alloy_primitives::B256, source_hash: &str) -> AuditResult {
    AuditResult {
        tx_hash: format!("{tx_hash:#x}"),
        source_hash: source_hash.to_string(),
        verdict: Verdict::Allow,
        actions: None,
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct AuditTransactionsRequest {
    pub rules: Vec<RawRule>,
    pub txs: Vec<DepositTxRequest>,
    /// `source_hash` values RCS has already resolved to Allow: both original deposits that
    /// RCS's adjudicator approved in an earlier round and synthetic ban deposits prepended for
    /// replay. Executed and committed unconditionally, skipping rule (re-)classification — a
    /// `quota`-type rule match is unconditional on the tx's own logs and has no memory of a
    /// prior decision, so re-classifying it would just produce `audit` again forever.
    #[serde(default)]
    pub known_allowed: Vec<String>,
}

/// Per-transaction call-frame observation budget of `xlayer_auditTransactions`.
///
/// Applied to each transaction separately and never pooled across a batch. Over budget, an
/// ordinary deposit is the non-verdict and is not committed.
pub const AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX: usize = 1_000;

/// The RPC crate's EVM adapter for the shared [`RcsCaptureScope`].
///
/// `RcsCaptureScope` is owned by `rcs-filter` and the concrete EVM by its own crate, so
/// `impl RcsCaptureScope for <that EVM>` belongs in neither — that is an orphan-rule violation.
/// `impl ForeignTrait for LocalType` is permitted, so this crate wraps the EVM in a type it owns.
/// The delegation target is the `Evm` trait's own `components_mut()` accessor, exactly as the
/// builder's adapter uses.
struct AuditCaptureScope<'e, E>(&'e mut E);

impl<E> AuditCaptureScope<'_, E> {
    /// The wrapped EVM, for driving the real `transact` inside the capture window.
    fn evm_mut(&mut self) -> &mut E {
        self.0
    }
}

impl<E> RcsCaptureScope for AuditCaptureScope<'_, E>
where
    E: Evm,
    E::Inspector: RcsCaptureControl,
{
    fn scope_start_capture(&mut self) {
        self.0.components_mut().1.start_capture();
    }

    fn scope_finish_capture(&mut self, result_logs: &[alloy_primitives::Log]) -> CaptureOutcome {
        self.0.components_mut().1.finish_capture(result_logs)
    }

    fn scope_abort_capture(&mut self) {
        self.0.components_mut().1.abort_capture();
    }
}

/// Rejects a request whose own rules failed to compile, listing every rejection's rule id and
/// reason in the error detail so the caller can see WHICH rule was refused and WHY.
///
/// Fails the whole request rather than silently classifying against a rule set that is missing
/// the rule(s) the caller actually asked for — a caller testing a specific rule must not get a
/// misleadingly "clean" verdict back.
///
/// Returns the JSON-RPC error rather than a bool so the handler and its tests exercise the exact
/// same code — including the error code, the message and the detail array a caller sees.
fn validate_compiled_rules(rules: &RuleSet) -> Result<(), jsonrpsee::types::ErrorObjectOwned> {
    if rules.rejected.is_empty() {
        return Ok(());
    }
    Err(jsonrpsee::types::ErrorObjectOwned::owned(
        -32602,
        format!("{} rule(s) failed validation and were rejected", rules.rejected.len()),
        Some(
            rules
                .rejected
                .iter()
                .map(|r| serde_json::json!({"id": r.id, "reason": r.reason}))
                .collect::<Vec<_>>(),
        ),
    ))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditTransactionsResponse {
    pub results: Vec<AuditResult>,
}

#[rpc(server, namespace = "xlayer")]
pub trait XlayerAuditApi {
    /// Classifies each deposit tx in `req.txs` against `req.rules`, without side effects —
    /// except for txs whose `source_hash` is in `req.known_allowed`, which are force-classified
    /// `allow` without running `req.rules` against them at all (see
    /// `AuditTransactionsRequest::known_allowed`).
    ///
    /// **Only meaningful for deposits not yet recorded in `TxBlacklist`.** If a deposit passed
    /// here has *already* been blacklisted on-chain, the underlying EVM (see the L1 force-tx
    /// intercept design) skips its payload before this handler ever sees any logs from it — the
    /// resulting verdict will misleadingly read `allow` (no logs to match against), not a
    /// reflection of the prior blacklist decision. This does not affect on-chain safety (the
    /// blacklisted deposit's payload is already guaranteed to be skipped independently of
    /// anything this RPC reports), but callers re-querying a previously-decided deposit for
    /// audit/debugging purposes should not treat its verdict here as authoritative — check
    /// `TxBlacklist` directly instead.
    #[method(name = "auditTransactions")]
    async fn audit_transactions(
        &self,
        req: AuditTransactionsRequest,
    ) -> RpcResult<AuditTransactionsResponse>;
}

/// Implementation struct, following the existing `XlayerRpcExt<T>` pattern
/// (`crates/rpc/src/default.rs`).
#[derive(Debug)]
pub struct XlayerAuditRpc<T> {
    pub backend: Arc<T>,
}

#[jsonrpsee::core::async_trait]
impl<T> XlayerAuditApiServer for XlayerAuditRpc<T>
where
    T: Call + LoadState + Clone + Send + Sync + 'static,
    // Deposit txs (`Recovered<OpTxEnvelope>`) only convert into a `TxEnv` when `T`'s configured
    // EVM factory is `OpEvmFactory` (that's where `FromRecoveredTx<OpTxEnvelope>` is defined).
    // Pin it down so `evm.transact(tx)` below type-checks for any `T`, not just one concrete node.
    <T as RpcNodeCore>::Evm:
        ConfigureEvm<BlockExecutorFactory: BlockExecutorFactory<EvmFactory = OpEvmFactory>>,
{
    async fn audit_transactions(
        &self,
        req: AuditTransactionsRequest,
    ) -> RpcResult<AuditTransactionsResponse> {
        // `content_version = 0` matches the previous hard-coded call. The audit RPC has no
        // hot-reload and no block pinning: one request carries its own rules and is adjudicated in
        // one call, so the snapshot stamp is derived from the request's OWN compiled set.
        let rules = load_rules(1, 0, req.rules);
        validate_compiled_rules(&rules)?;

        let known_allowed: HashSet<String> = req.known_allowed.iter().cloned().collect();

        // Build every tx up front so a per-item construction failure produces a `Malformed`
        // result without aborting the rest of the batch.
        let (ordered_txs, ordered_tx_reqs, built) = build_deposits(&req.txs);

        // Fetch the header FIRST and pin state to that exact block hash (rather than
        // independently resolving "latest" twice). If a new block landed between two
        // independent "latest" lookups, state could come from block N while the EVM env is
        // built for block N+1 — a mismatched pairing. Deriving the state's `BlockId` from the
        // header's own hash guarantees both reference the identical block.
        let latest_header = self
            .backend
            .provider()
            .latest_header()
            .map_err(|e| {
                jsonrpsee::types::ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>)
            })?
            .ok_or_else(|| {
                jsonrpsee::types::ErrorObjectOwned::owned(-32000, "no latest header", None::<()>)
            })?;
        let evm_env = self.backend.evm_config().evm_env(&latest_header).map_err(|e| {
            jsonrpsee::types::ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>)
        })?;

        // `ordered_txs`/`ordered_tx_reqs`/`txs`/`built`/`rules`/`known_allowed` all move into the
        // closure below: assembling each request's `AuditResult` has to happen there too, since
        // the executed subset's logs (`ordered_txs`-indexed) never leave the blocking task.
        let backend = self.backend.as_ref().clone();
        let block_id = BlockId::hash(latest_header.hash());
        let txs = req.txs;
        let results: Vec<AuditResult> = backend
            .spawn_with_state_at_block(block_id, move |this, mut state_db| {
                // When the request declares at least one call-frame selector the shared capture
                // inspector is attached in `CallstackOnly` mode. With no selector declared, NO
                // inspector is constructed at all and this path takes its plain EVM.
                let inspector =
                    callstack_only_inspector(&rules, AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX);
                let mut executed = execute_audit_batch(
                    this.evm_config().evm_factory(),
                    &mut state_db,
                    evm_env,
                    &rules,
                    &known_allowed,
                    inspector,
                    &ordered_txs,
                    &ordered_tx_reqs,
                )
                .map_err(T::Error::from_evm_err)?;

                Ok(assemble_results(&txs, &built, &mut executed))
            })
            .await
            .map_err(Into::into)?;

        Ok(AuditTransactionsResponse { results })
    }
}

/// The one EVM-construction site for the two EVM shapes this path executes.
///
/// The handler and the test seam both go through here, so the inspector branch and the plain
/// branch exist once. With no inspector the plain EVM is built and no capture runs.
// Eight parameters is the batch's own input list; grouping any of them would obscure, not
// simplify, what the batch is given.
#[allow(clippy::too_many_arguments)]
fn execute_audit_batch<DB>(
    factory: &OpEvmFactory,
    db: DB,
    evm_env: EvmEnv<OpSpecId>,
    rules: &RuleSet,
    known_allowed: &HashSet<String>,
    inspector: Option<RcsInspector>,
    txs: &[Recovered<OpTransactionSigned>],
    reqs: &[DepositTxRequest],
) -> Result<Vec<Option<AuditResult>>, <OpEvmFactory as EvmFactory>::Error<DB::Error>>
where
    DB: Database + DatabaseCommit,
{
    match inspector {
        Some(inspector) => {
            let mut evm = factory.create_evm_with_inspector(db, evm_env, inspector);
            run_batch(&mut evm, rules, known_allowed, txs, reqs)
        }
        None => {
            let mut evm = factory.create_evm(db, evm_env);
            run_batch(&mut evm, rules, known_allowed, txs, reqs)
        }
    }
}

/// The base per-transaction loop, shared by the inspector and plain EVM shapes.
fn run_batch<E>(
    evm: &mut E,
    rules: &RuleSet,
    known_allowed: &HashSet<String>,
    txs: &[Recovered<OpTransactionSigned>],
    reqs: &[DepositTxRequest],
) -> Result<Vec<Option<AuditResult>>, E::Error>
where
    E: Evm,
    E::DB: DatabaseCommit,
    E::Inspector: RcsCaptureControl,
    for<'tx> &'tx Recovered<OpTransactionSigned>: IntoTxEnv<E::Tx>,
{
    let mut executed: Vec<Option<AuditResult>> = (0..txs.len()).map(|_| None).collect();
    for (exec_idx, tx) in txs.iter().enumerate() {
        let tx_req = &reqs[exec_idx];
        let (ResultAndState { result, state }, capture) = audit_capture_window(&mut *evm, tx)?;
        let observations = observations_from(&capture);
        #[cfg(any(test, feature = "test-utils"))]
        observation_recorder::record(observations);

        // Base contract, unchanged: a known_allowed entry is committed and allowed without any
        // rule (re-)classification. Its capture outcome — complete, over budget, incomplete or
        // foreign-stamped — is deliberately not consulted.
        if known_allowed.contains(&tx_req.source_hash) {
            evm.db_mut().commit(state);
            executed[exec_idx] = Some(known_allowed_result(tx.tx_hash(), &tx_req.source_hash));
            continue;
        }

        let verdict_result = match unadjudicable_observations(rules, observations) {
            Some(reason) => unadjudicable_result(&tx_req.source_hash, reason),
            None => super::verdict::verdict_for(
                tx_req,
                ExecutedTx {
                    from: tx.signer(),
                    to: tx.to(),
                    tx_hash: tx.tx_hash(),
                    value: tx.value(),
                },
                result.logs(),
                rules,
                observations,
            ),
        };
        let (commit, keep_going) = commit_and_continue(verdict_result.verdict);
        if commit {
            evm.db_mut().commit(state);
        }
        executed[exec_idx] = Some(verdict_result);
        if !keep_going {
            break;
        }
    }
    Ok(executed)
}

/// The non-verdict for a transaction whose executed code could not be identified.
///
/// Bounded deliberately: the request's `source_hash` and the fixed low-cardinality reason token.
/// No rule id, no observed address, no code hash.
fn unadjudicable_result(source_hash: &str, reason: ObservationUnavailable) -> AuditResult {
    tracing::warn!(
        target: "xlayer_audit_rpc",
        %source_hash,
        reason = reason.as_str(),
        "observations unavailable for a selector-bearing request; returning the non-verdict"
    );
    AuditResult::malformed(source_hash.to_string())
}

/// Builds the caller's ordered, built batch: every request item whose deposit transaction could
/// be decoded, its request, and the original-index → exec-index map (`None` for a failed build).
///
/// Both the handler and the test seam build through here, so a per-item construction failure
/// produces a `Malformed` result identically on both paths without aborting the rest of the batch.
fn build_deposits(
    reqs: &[DepositTxRequest],
) -> (Vec<Recovered<OpTransactionSigned>>, Vec<DepositTxRequest>, Vec<Option<usize>>) {
    let mut ordered_txs = Vec::with_capacity(reqs.len());
    let mut ordered_tx_reqs: Vec<DepositTxRequest> = Vec::with_capacity(reqs.len());
    let mut built: Vec<Option<usize>> = Vec::with_capacity(reqs.len());
    for tx_req in reqs {
        match build_deposit_tx(tx_req) {
            Ok(tx) => {
                built.push(Some(ordered_txs.len()));
                ordered_tx_reqs.push(tx_req.clone());
                ordered_txs.push(tx);
            }
            Err(_err) => {
                built.push(None);
            }
        }
    }
    (ordered_txs, ordered_tx_reqs, built)
}

/// Opens one capture window around the real `transact` of a single audit transaction and returns
/// the execution outcome together with the capture outcome.
///
/// Extracted from the batch loop so the window is defined once: the guard is scoped so the EVM is
/// usable again immediately afterwards, and its `Drop` aborts capture on the `?` path. Every
/// caller therefore leaves the inspector inactive by construction rather than by discipline.
fn audit_capture_window<E, Tx>(
    evm: &mut E,
    tx: Tx,
) -> Result<(ResultAndState<E::HaltReason>, CaptureOutcome), E::Error>
where
    E: Evm,
    E::Inspector: RcsCaptureControl,
    Tx: IntoTxEnv<E::Tx>,
{
    let mut scope = AuditCaptureScope(evm);
    let mut guard = CaptureGuard::new(&mut scope);
    let outcome = guard.scope_mut().evm_mut().transact(tx)?;
    let capture = guard.finish(outcome.result.logs());
    Ok((outcome, capture))
}

/// Records, in batch order, the observation set the production batch body actually fed to
/// screening for each transaction.
///
/// Compiled only for this crate's own tests and for the non-default `test-utils` feature, so no
/// production build contains it. It exists because "the two paths observe the same frames" is
/// only meaningful if the compared frames are the ones production really used: two decisions can
/// agree while resting on different observations, and a path that silently dropped its capture
/// would still produce a decision.
#[cfg(any(test, feature = "test-utils"))]
pub mod observation_recorder {
    use rcs_filter::callstack::{CallObservation, Observations};
    use std::cell::RefCell;

    thread_local! {
        static RECORDED: RefCell<Vec<Option<Vec<CallObservation>>>> =
            const { RefCell::new(Vec::new()) };
    }

    /// `None` records an unavailable set, which is deliberately NOT the same as an empty one.
    pub(super) fn record(observations: Observations<'_>) {
        let entry = match observations {
            Observations::Available { items, .. } => Some(items.to_vec()),
            Observations::Unavailable { .. } => None,
        };
        RECORDED.with(|recorded| recorded.borrow_mut().push(entry));
    }

    /// Returns everything recorded since the previous take, and clears the recording.
    pub fn take() -> Vec<Option<Vec<CallObservation>>> {
        RECORDED.with(|recorded| std::mem::take(&mut *recorded.borrow_mut()))
    }
}

/// Cross-crate test access to this path's own complete production batch.
///
/// Compiled only with the non-default `test-utils` feature, which no `[dependencies]` entry
/// enables — a production build contains none of it. It exists so a test that has to live in
/// another crate (the equal-observation comparison across the two execution paths) can drive
/// **this** path's real code instead of re-implementing it, which would make the comparison prove
/// nothing.
#[cfg(feature = "test-utils")]
pub mod test_seams {
    use super::*;
    use rcs_filter::callstack::SnapshotStamp;
    use rcs_filter::capture::{CallstackCaptureConfig, RcsInspectorConfig};

    /// Runs `req` through the complete production batch against `db`: request validation and
    /// rule compilation, deposit construction, the production inspector choice with
    /// [`AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX`], the per-transaction loop and result assembly.
    /// Only the handler's state anchoring (latest header, blocking task) is not part of it.
    /// Validation errors are the exact JSON-RPC errors the handler returns.
    pub fn execute_audit_batch<DB: Database + DatabaseCommit>(
        req: &AuditTransactionsRequest,
        db: DB,
        evm_env: EvmEnv<OpSpecId>,
    ) -> Result<Vec<AuditResult>, jsonrpsee::types::ErrorObjectOwned> {
        run_complete_batch(req, db, evm_env, None)
    }

    /// The same batch with capture configured under `capture_stamp` instead of the request's own
    /// snapshot identity — the only way to reach the stamp-mismatch branch, since a request
    /// always compiles its own rules. The selector set and budget are the production ones.
    pub fn execute_audit_batch_with_capture_stamp<DB: Database + DatabaseCommit>(
        req: &AuditTransactionsRequest,
        db: DB,
        evm_env: EvmEnv<OpSpecId>,
        capture_stamp: SnapshotStamp,
    ) -> Result<Vec<AuditResult>, jsonrpsee::types::ErrorObjectOwned> {
        run_complete_batch(req, db, evm_env, Some(capture_stamp))
    }

    fn run_complete_batch<DB: Database + DatabaseCommit>(
        req: &AuditTransactionsRequest,
        db: DB,
        evm_env: EvmEnv<OpSpecId>,
        capture_stamp: Option<SnapshotStamp>,
    ) -> Result<Vec<AuditResult>, jsonrpsee::types::ErrorObjectOwned> {
        let rules = load_rules(1, 0, req.rules.clone());
        validate_compiled_rules(&rules)?;
        let known_allowed: HashSet<String> = req.known_allowed.iter().cloned().collect();
        let (txs, reqs, built) = build_deposits(&req.txs);
        let inspector = match capture_stamp {
            None => callstack_only_inspector(&rules, AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX),
            Some(stamp) => rules.has_callstack_rules().then(|| {
                RcsInspector::new(RcsInspectorConfig::callstack_only_for_test(
                    AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX,
                    CallstackCaptureConfig::new(stamp, rules.callstack_selectors().to_vec()),
                ))
            }),
        };
        // Explicitly `super::`: the public seam of the same name would otherwise shadow the
        // private production function.
        let mut executed = super::execute_audit_batch(
            &OpEvmFactory::default(),
            db,
            evm_env,
            &rules,
            &known_allowed,
            inspector,
            &txs,
            &reqs,
        )
        .map_err(|error| {
            jsonrpsee::types::ErrorObjectOwned::owned(-32000, error.to_string(), None::<()>)
        })?;
        Ok(assemble_results(&req.txs, &built, &mut executed))
    }
}

#[cfg(test)]
mod algorithm_tests {
    use super::super::verdict::Verdict;
    use super::commit_and_continue;

    #[test]
    fn allow_commits_and_continues() {
        assert_eq!(commit_and_continue(Verdict::Allow), (true, true));
    }

    #[test]
    fn deny_discards_and_continues() {
        assert_eq!(commit_and_continue(Verdict::Deny), (false, true));
    }

    #[test]
    fn audit_discards_and_stops() {
        assert_eq!(commit_and_continue(Verdict::Audit), (false, false));
    }

    #[test]
    fn malformed_discards_and_continues() {
        assert_eq!(commit_and_continue(Verdict::Malformed), (false, true));
    }

    use super::super::deposit::DepositTxRequest;
    use super::super::verdict::AuditResult;
    use super::assemble_results;

    fn req(source_hash: &str) -> DepositTxRequest {
        DepositTxRequest {
            source_hash: source_hash.to_string(),
            from: "0x0404040404040404040404040404040404040404".to_string(),
            to: None,
            mint: "0".to_string(),
            value: "0".to_string(),
            gas_limit: 100_000,
            is_system_transaction: false,
            data: "0x".to_string(),
        }
    }

    #[test]
    fn malformed_entry_stays_malformed_regardless_of_executed_state() {
        let txs = vec![req("0xa")];
        let built = vec![None]; // build_deposit_tx failed for this one
        let mut executed: Vec<Option<AuditResult>> = vec![];

        let results = assemble_results(&txs, &built, &mut executed);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verdict, Verdict::Malformed);
        assert_eq!(results[0].source_hash, "0xa");
    }

    #[test]
    fn executed_entry_is_used_as_is() {
        let txs = vec![req("0xa")];
        let built = vec![Some(0)];
        let mut executed = vec![Some(AuditResult {
            tx_hash: "0xhash".to_string(),
            source_hash: "0xa".to_string(),
            verdict: Verdict::Allow,
            actions: None,
        })];

        let results = assemble_results(&txs, &built, &mut executed);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verdict, Verdict::Allow);
        assert_eq!(results[0].tx_hash, "0xhash");
    }

    #[test]
    fn never_executed_entry_becomes_unknown() {
        // Simulates: this tx was built successfully (built[0] = Some(0)) but the execution loop
        // stopped before reaching it (an earlier tx in the batch hit Audit) — executed[0] stays
        // None. This is the exact bug-fix scenario: previously this tx would have been executed
        // and (wrongly) classified anyway.
        let txs = vec![req("0xb")];
        let built = vec![Some(0)];
        let mut executed: Vec<Option<AuditResult>> = vec![None];

        let results = assemble_results(&txs, &built, &mut executed);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verdict, Verdict::Unknown);
        assert_eq!(results[0].source_hash, "0xb");
    }

    #[test]
    fn mixed_batch_preserves_original_order() {
        let txs = vec![req("0xa"), req("0xb"), req("0xc")];
        let built = vec![Some(0), None, Some(1)];
        let mut executed = vec![
            Some(AuditResult {
                tx_hash: "0xhashA".to_string(),
                source_hash: "0xa".to_string(),
                verdict: Verdict::Allow,
                actions: None,
            }),
            None, // 0xc was built (built[2] = Some(1)) but never executed
        ];

        let results = assemble_results(&txs, &built, &mut executed);

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].source_hash, "0xa");
        assert_eq!(results[0].verdict, Verdict::Allow);
        assert_eq!(results[1].source_hash, "0xb");
        assert_eq!(results[1].verdict, Verdict::Malformed);
        assert_eq!(results[2].source_hash, "0xc");
        assert_eq!(results[2].verdict, Verdict::Unknown);
    }

    use super::known_allowed_result;
    use alloy_primitives::B256;

    #[test]
    fn known_allowed_result_is_allow_with_no_actions() {
        let result = known_allowed_result(B256::ZERO, "0xsrc");
        assert_eq!(result.verdict, Verdict::Allow);
        assert_eq!(result.source_hash, "0xsrc");
        assert_eq!(result.tx_hash, format!("{:#x}", B256::ZERO));
        assert!(result.actions.is_none());
    }

    #[test]
    fn known_allowed_membership_check_matches_by_source_hash() {
        let known_allowed: std::collections::HashSet<String> =
            ["0xsrc1".to_string(), "0xsrc2".to_string()].into_iter().collect();
        assert!(known_allowed.contains("0xsrc1"));
        assert!(!known_allowed.contains("0xsrc3"));
    }
}
