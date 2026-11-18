//! `xlayer_auditTransactions` JSON-RPC method: builds deposit txs from the request and executes
//! them **one at a time, in order**, against chain-tip state. `Allow` commits full state. `Deny`
//! discards the simulated state and continues. `Audit` discards the simulated state and stops,
//! leaving the tail `Unknown`. Native blacklist interception occurs inside EVM execution before
//! matcher classification; RCS synthetic-ban replay is responsible for making denied force
//! deposits reach that path.
//! A tx whose `source_hash` is in the request's `known_allowed` set is never re-classified on the
//! quota/event dimension (see `AuditTransactionsRequest::known_allowed`'s doc comment for why). It
//! has one of three outcomes: a synthetic ban prefix is executed and committed as `Allow`; an
//! approved original is re-adjudicated on the call-frame dimension only, so it can become `Deny`
//! or the non-verdict but never `Audit`; and with no call-frame rule in the request an approved
//! original is committed as `Allow`. See [`KnownAllowedKind`] for how the class is decided.
//!
//! Every transaction has its own call-frame observation budget
//! ([`AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX`], 1 000), applied per transaction and never pooled
//! across the batch. Over budget, an ordinary deposit or an approved original is the non-verdict
//! and its state is not committed. A synthetic ban prefix — labelled `synthetic_ban` by the
//! caller, or recognised by the content-binding check when the request sends no label map — keeps
//! its unconditional `Allow` + commit, because that commit rebuilds the base state the later
//! entries are adjudicated on; it is reported `not_identified`, and an over-budget prefix also
//! emits the bounded `xlayer_audit_rpc::synthetic_ban_budget` event.
//!
//! This handler does no side-effecting screening (never touches `FilterHandle::screen_tx` or its
//! `BufferPool`): it runs a throwaway EVM overlay seeded from the current chain-tip state and
//! discards it after producing verdicts.
//!
//! State anchoring and task placement follow the same `Call` helper (`spawn_with_state_at_block`)
//! that `eth_call`/`eth_estimateGas` use, so this handler's EVM execution runs on reth's blocking
//! task pool rather than the async runtime thread.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use alloy_consensus::transaction::Recovered;
use alloy_consensus::Transaction;
use alloy_eips::BlockId;
use alloy_evm::{Database, Evm, EvmFactory, IntoTxEnv};
use alloy_op_evm::OpEvmFactory;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use op_revm::OpSpecId;
pub use rcs_filter::audit::KnownAllowedKind;
use rcs_filter::audit::{
    callstack_only_inspector, callstack_only_projection, deposit_known_allowed_kind,
    observations_from, resolve_known_allowed_kind, unadjudicable_observations,
    unlabelled_known_allowed, KnownAllowedResolution,
};
use rcs_filter::callstack::{ObservationUnavailable, Observations};
use rcs_filter::capture::{
    CallstackCaptureConfig, CaptureGuard, CaptureOutcome, RcsCaptureControl, RcsCaptureScope,
    RcsInspector,
};
use rcs_filter::config::{
    is_supported_protocol, MIN_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};
use rcs_filter::rules::{load_rules, RawRule, RuleSet, UnknownFields};
use reth_evm::{block::BlockExecutorFactory, ConfigureEvm, EvmEnv};
use reth_optimism_primitives::OpTransactionSigned;
use reth_rpc_eth_api::{
    helpers::{Call, LoadState},
    FromEvmError, RpcNodeCore,
};
use reth_storage_api::BlockReaderIdExt;
use revm::{context_interface::result::ResultAndState, DatabaseCommit};

use super::deposit::{build_deposit_tx, DepositTxRequest};
use super::metrics::{KnownAllowedResolutionMetrics, SyntheticBanIdentificationMetrics};
use super::verdict::AuditResult;
use super::verdict::CallstackIdentification;
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

/// Builds the `Allow` result for a committed tx whose `source_hash` is in the caller-supplied
/// `known_allowed` set. No quota/event classification ran for it, so it never carries actions (see
/// `AuditTransactionsRequest::known_allowed`'s doc comment for why).
fn known_allowed_result(tx_hash: alloy_primitives::B256, source_hash: &str) -> AuditResult {
    AuditResult {
        tx_hash: format!("{tx_hash:#x}"),
        source_hash: source_hash.to_string(),
        verdict: Verdict::Allow,
        actions: None,
        callstack_identification: None,
    }
}

/// The rule protocol version an omitted request field means. Keeps every existing caller
/// byte-compatible: a request with no version behaves exactly as it does today.
const fn default_protocol_version() -> u32 {
    MIN_PROTOCOL_VERSION
}

#[derive(Debug, serde::Deserialize)]
#[serde(try_from = "AuditTransactionsRequestWire")]
pub struct AuditTransactionsRequest {
    /// Rule protocol version of `rules`. Additive and optional: omitted means `1`.
    pub protocol_version: u32,
    pub rules: Vec<RawRule>,
    pub txs: Vec<DepositTxRequest>,
    /// `source_hash` values RCS has already resolved to Allow: both original deposits that
    /// RCS's adjudicator approved in an earlier round and synthetic ban deposits prepended for
    /// replay. The `quota`/event dimension is never re-classified for them — a `quota`-type rule
    /// match is unconditional on the tx's own logs and has no memory of a prior decision, so
    /// re-classifying it would just produce `audit` again forever. Only the **callstack**
    /// dimension may change an `Original`'s verdict, and only ever to `deny` or a non-verdict.
    pub known_allowed: Vec<String>,
    /// `source_hash` → class for every `known_allowed` entry. Entirely **optional**: a request
    /// that supplies no map at all is accepted, and each entry's class is then decided by the
    /// content-binding check described on [`KnownAllowedKind`] — not by source authentication. A
    /// request that DOES supply the map must label every one of its `known_allowed` entries,
    /// because a caller labelling some and not others is label-aware and ambiguous; no entry of
    /// such a request is inferred.
    pub known_allowed_kinds: Option<BTreeMap<String, KnownAllowedKind>>,
}

/// Wire shape of [`AuditTransactionsRequest`]: its declared fields plus every key it does not
/// declare, so a speculative or misspelled request field is rejected by name instead of being
/// silently discarded — the same strictness the rule wire schema has.
#[derive(Debug, serde::Deserialize)]
struct AuditTransactionsRequestWire {
    #[serde(default = "default_protocol_version")]
    protocol_version: u32,
    rules: Vec<RawRule>,
    txs: Vec<DepositTxRequest>,
    #[serde(default)]
    known_allowed: Vec<String>,
    #[serde(default)]
    known_allowed_kinds: Option<BTreeMap<String, KnownAllowedKind>>,
    #[serde(flatten)]
    unknown: UnknownFields,
}

impl TryFrom<AuditTransactionsRequestWire> for AuditTransactionsRequest {
    type Error = String;

    fn try_from(wire: AuditTransactionsRequestWire) -> Result<Self, String> {
        rcs_filter::rules::reject_unknown_fields("audit request", &wire.unknown)?;
        Ok(Self {
            protocol_version: wire.protocol_version,
            rules: wire.rules,
            txs: wire.txs,
            known_allowed: wire.known_allowed,
            known_allowed_kinds: wire.known_allowed_kinds,
        })
    }
}

/// Per-transaction call-frame observation budget of `xlayer_auditTransactions`.
///
/// Applied to each transaction separately, never pooled across a batch, and independent of the
/// payload builder's `max-callstack-observations-per-tx`. Over budget, an ordinary deposit or an
/// approved original is the non-verdict and is not committed; a synthetic ban prefix keeps its
/// `Allow` + commit and is reported `not_identified` (see the module documentation).
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
    fn scope_configure_callstack_capture(&mut self, config: CallstackCaptureConfig) {
        self.0.components_mut().1.configure_callstack_capture(config);
    }

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

/// Rejects a rule protocol version this build does not support, naming it.
///
/// Returns the JSON-RPC error rather than a bool so the handler and its tests exercise the exact
/// same code — including the error code and message a caller sees.
fn validate_protocol_version(
    protocol_version: u32,
) -> Result<(), jsonrpsee::types::ErrorObjectOwned> {
    if is_supported_protocol(protocol_version) {
        return Ok(());
    }
    Err(jsonrpsee::types::ErrorObjectOwned::owned(
        -32602,
        format!(
            "unsupported rule protocol_version {protocol_version}; supported: {:?}",
            SUPPORTED_PROTOCOL_VERSIONS
        ),
        None::<()>,
    ))
}

/// Rejects a request that supplies a `known_allowed_kinds` map which does not cover every
/// `known_allowed` entry, naming the unlabelled `source_hash` values.
///
/// Labels are optional, and that is load-bearing: a caller that sends no map at all is the
/// ordinary case, and rejecting it would make every round that carries a non-empty
/// `known_allowed` fail permanently. Such a request is accepted and each entry's class is decided
/// from its built deposit (see [`KnownAllowedKind`]).
///
/// A **partial** map is a different thing and is still refused: a caller that labels some entries
/// and not others is label-aware, so the omission is ambiguous rather than conventional, and the
/// node does not silently pick a class for the remainder.
fn validate_known_allowed_labels(
    known_allowed: &[String],
    kinds: Option<&BTreeMap<String, KnownAllowedKind>>,
) -> Result<(), jsonrpsee::types::ErrorObjectOwned> {
    let unlabelled = unlabelled_known_allowed(known_allowed, kinds);
    if unlabelled.is_empty() {
        return Ok(());
    }
    Err(jsonrpsee::types::ErrorObjectOwned::owned(
        -32602,
        format!(
            "known_allowed_kinds is missing a label for {} entr{}",
            unlabelled.len(),
            if unlabelled.len() == 1 { "y" } else { "ies" }
        ),
        Some(unlabelled),
    ))
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

/// The class of an unlabelled `known_allowed` entry, decided from the deposit the batch executes.
///
/// Only the built transaction's own `source_hash` and `input` are read — the bytes
/// `build_deposit_tx` decoded once from the request — so every spelling of one request decodes
/// to one class. A transaction that is not a deposit cannot be a synthetic ban.
fn inferred_known_allowed_kind(tx: &Recovered<OpTransactionSigned>) -> KnownAllowedKind {
    match tx.inner() {
        op_alloy_consensus::OpTxEnvelope::Deposit(deposit) => {
            deposit_known_allowed_kind(deposit.source_hash, &deposit.input)
        }
        _ => KnownAllowedKind::Original,
    }
}

/// One request's validated, owned batch inputs.
///
/// The handler validates and compiles a request ONCE, then moves the whole plan into the blocking
/// task that executes it, which is why these are owned values. One layer only: the executor and
/// the per-class helpers are methods on the plan, and the built transactions are passed alongside
/// it rather than copied into a second borrowed view.
struct AuditBatchPlan {
    rules: RuleSet,
    known_allowed_rules: Option<RuleSet>,
    known_allowed: HashSet<String>,
    kinds: Option<BTreeMap<String, KnownAllowedKind>>,
}

impl AuditBatchPlan {
    /// Validates one request and compiles everything its batch needs, returning the exact
    /// JSON-RPC error a caller would see on the first failing check.
    fn prepare(req: &AuditTransactionsRequest) -> Result<Self, jsonrpsee::types::ErrorObjectOwned> {
        // The protocol version belongs to the REQUEST. An omitted field means 1, so every
        // existing caller is byte-compatible; a version outside the supported set is named in
        // the error rather than silently downgraded.
        validate_protocol_version(req.protocol_version)?;
        // `content_version = 0` matches the previous hard-coded call. The audit RPC has no
        // hot-reload and no block pinning: one request carries its own rules and is adjudicated
        // in one call, so the snapshot stamp is derived from the request's OWN compiled set and
        // the request deliberately does not accept a `content_version` (honouring one would
        // imply a persistence this path does not have).
        let rules = load_rules(req.protocol_version, 0, req.rules.clone());
        validate_compiled_rules(&rules)?;
        validate_known_allowed_labels(&req.known_allowed, req.known_allowed_kinds.as_ref())?;
        let known_allowed_rules = callstack_only_projection(&rules, &req.rules);
        debug_assert!(
            known_allowed_rules.as_ref().is_none_or(|set| set.stamp() == rules.stamp()),
            "the callstack-only projection must carry the same snapshot stamp"
        );
        Ok(Self {
            rules,
            known_allowed_rules,
            known_allowed: req.known_allowed.iter().cloned().collect(),
            kinds: req.known_allowed_kinds.clone(),
        })
    }

    /// `Some(kind)` when this `source_hash` is a `known_allowed` entry.
    ///
    /// A request with a label map is read from it: a partial map was already refused at the
    /// boundary, so every entry has a label and nothing is inferred (an entry missing from a
    /// validated map cannot occur; it would be adjudicated as an ordinary deposit). Without a map
    /// the class is inferred from the built deposit `tx` (see [`KnownAllowedKind`]).
    fn known_allowed_kind(
        &self,
        source_hash: &str,
        tx: &Recovered<OpTransactionSigned>,
    ) -> Option<(KnownAllowedKind, KnownAllowedResolution)> {
        resolve_known_allowed_kind(&self.known_allowed, self.kinds.as_ref(), source_hash, || {
            inferred_known_allowed_kind(tx)
        })
    }

    /// The response diagnostic, reported only when the request declares a call-frame alias.
    fn identification(&self, value: CallstackIdentification) -> Option<CallstackIdentification> {
        self.rules.has_callstack_rules().then_some(value)
    }

    /// Executes one ordered batch against `evm` and returns each built transaction's result in
    /// order (`None` for an entry after an `Audit` stop).
    ///
    /// This is the base skeleton: transact, resolve the `known_allowed` class, let the class
    /// decide its verdict, commit the state when it says so, and stop when it says so. Capture
    /// and diagnostics live in the class helpers instead of in this loop.
    fn run<E>(
        &self,
        evm: &mut E,
        txs: &[Recovered<OpTransactionSigned>],
        reqs: &[DepositTxRequest],
    ) -> Result<Vec<Option<AuditResult>>, E::Error>
    where
        E: Evm,
        E::DB: DatabaseCommit,
        E::Inspector: RcsCaptureControl,
        for<'tx> &'tx Recovered<OpTransactionSigned>: IntoTxEnv<E::Tx>,
    {
        // Built on first use rather than per batch: a request with no `known_allowed` entry
        // registers nothing.
        let mut resolution_metrics: Option<KnownAllowedResolutionMetrics> = None;

        let mut executed: Vec<Option<AuditResult>> = (0..txs.len()).map(|_| None).collect();
        for (exec_idx, tx) in txs.iter().enumerate() {
            let tx_req = &reqs[exec_idx];

            // One capture window per transaction, opened by the shared RAII guard.
            let (ResultAndState { result, state }, capture) = audit_capture_window(&mut *evm, tx)?;
            let observations = observations_from(&capture);
            #[cfg(any(test, feature = "test-utils"))]
            observation_recorder::record(observations);

            let resolved = self.known_allowed_kind(&tx_req.source_hash, tx);
            if let Some((kind, resolution)) = resolved {
                let metrics =
                    resolution_metrics.get_or_insert_with(KnownAllowedResolutionMetrics::default);
                match (resolution, kind) {
                    (KnownAllowedResolution::Labelled, _) => metrics.labelled_total.increment(1),
                    (KnownAllowedResolution::Inferred, KnownAllowedKind::SyntheticBan) => {
                        metrics.inferred_synthetic_ban_total.increment(1)
                    }
                    (KnownAllowedResolution::Inferred, KnownAllowedKind::Original) => {
                        metrics.inferred_original_total.increment(1)
                    }
                }
            }

            let (audit_result, commit, keep_going) = match resolved.map(|(kind, _)| kind) {
                Some(KnownAllowedKind::SyntheticBan) => {
                    self.synthetic_ban_entry(tx, tx_req, result.logs(), observations)
                }
                Some(KnownAllowedKind::Original) => {
                    self.original_entry(tx, tx_req, result.logs(), observations)
                }
                None => self.ordinary_entry(tx, tx_req, result.logs(), observations),
            };
            if commit {
                evm.db_mut().commit(state);
            }
            executed[exec_idx] = Some(audit_result);
            if !keep_going {
                break;
            }
        }
        Ok(executed)
    }

    /// The synthetic replay prefix's entry: `Allow` + commit unconditionally, with its
    /// identification reported.
    ///
    /// The caller skips such a prefix's verdict entirely and *depends* on it committing so the
    /// blacklist / failed-deposit base state is rebuilt; withholding that commit to "protect" it
    /// would corrupt the very base state the protection depends on. Its identification result is
    /// therefore reported, never acted on.
    fn synthetic_ban_entry(
        &self,
        tx: &Recovered<OpTransactionSigned>,
        tx_req: &DepositTxRequest,
        logs: &[alloy_primitives::Log],
        observations: Observations<'_>,
    ) -> (AuditResult, bool, bool) {
        // The SAME unadjudicable judgement the ordinary gate applies, deliberately reused rather
        // than restated: an observation set is adjudicable only if it is available AND was
        // produced under this request's own snapshot. Testing only the available dimension here
        // let an `Available` set carrying a FOREIGN stamp reach the classifier, where an
        // `origin` / `contract_address`-narrowed rule is skipped with zero decoding and the
        // prefix was reported `Clean` — "identified, nothing matched" about frames this snapshot
        // cannot adjudicate at all. A request declaring no selector still falls through to the
        // classifier and keeps its `NotApplicable` report, because the judgement returns `None`
        // there.
        let identification = diagnostic_identification(self, tx, tx_req, logs, observations);
        // Observation only, and the one place an operator can see it: the consumer skips this
        // prefix's verdict, so the response diagnostic reaches nobody.
        report_synthetic_ban_identification(
            &SyntheticBanIdentificationMetrics::default(),
            tx.tx_hash(),
            &tx_req.source_hash,
            identification,
            self.rules.selector_bearing_rule_count(),
        );
        if unadjudicable_observations(&self.rules, observations)
            == Some(ObservationUnavailable::BudgetExceeded)
        {
            report_synthetic_ban_budget_exceeded(
                tx.tx_hash(),
                &tx_req.source_hash,
                self.rules.selector_bearing_rule_count(),
            );
        }
        (
            known_allowed_result(tx.tx_hash(), &tx_req.source_hash)
                .with_identification(self.identification(identification)),
            true,
            true,
        )
    }

    /// An `Original`, labelled or inferred, earns no exemption from the non-verdict rule.
    ///
    /// Priority, highest first: unavailable observations with an alias-bearing request → the
    /// existing non-verdict; an alias-bearing objection → `Deny`; otherwise `Allow` + commit,
    /// byte-identical to the previous behaviour.
    fn original_entry(
        &self,
        tx: &Recovered<OpTransactionSigned>,
        tx_req: &DepositTxRequest,
        logs: &[alloy_primitives::Log],
        observations: Observations<'_>,
    ) -> (AuditResult, bool, bool) {
        if let Some(reason) = unadjudicable_observations(&self.rules, observations) {
            return (unadjudicable_result(self, &tx_req.source_hash, reason), false, true);
        }
        let Some(callstack_rules) = self.known_allowed_rules.as_ref() else {
            // The request declares no selector, so there is nothing to adjudicate and this is
            // exactly today's unconditional `Allow` + commit.
            return (
                known_allowed_result(tx.tx_hash(), &tx_req.source_hash).with_identification(
                    self.identification(CallstackIdentification::NotApplicable),
                ),
                true,
                true,
            );
        };
        let adjudicated = super::verdict::verdict_for(
            tx_req,
            ExecutedTx { from: tx.signer(), to: tx.to(), tx_hash: tx.tx_hash(), value: tx.value() },
            logs,
            callstack_rules,
            observations,
        );
        match adjudicated.verdict {
            // Not adjudicable: the existing non-verdict, state NOT committed.
            Verdict::Malformed => (
                AuditResult::malformed(tx_req.source_hash.clone()).with_identification(
                    self.identification(CallstackIdentification::NotIdentified),
                ),
                false,
                true,
            ),
            // A selector-bearing rule objects. Reuses `commit_and_continue(Deny)`'s existing
            // semantics unchanged: state discarded, batch continues.
            Verdict::Deny => (
                AuditResult {
                    tx_hash: format!("{:#x}", tx.tx_hash()),
                    source_hash: tx_req.source_hash.clone(),
                    verdict: Verdict::Deny,
                    actions: None,
                    callstack_identification: None,
                }
                .with_identification(self.identification(CallstackIdentification::Objected)),
                false,
                true,
            ),
            // `Audit` is deliberately NOT propagated here: the callstack dimension may only ever
            // move a `known_allowed` verdict to `Deny` or a non-verdict, never to `Audit`, so
            // quota convergence cannot regress into a re-adjudication loop.
            Verdict::Allow | Verdict::Audit | Verdict::Unknown => (
                known_allowed_result(tx.tx_hash(), &tx_req.source_hash)
                    .with_identification(self.identification(CallstackIdentification::Clean)),
                true,
                true,
            ),
        }
    }

    /// An ordinary deposit (not in `known_allowed`): the unadjudicable gate, then the full
    /// classifier, then the commit table.
    fn ordinary_entry(
        &self,
        tx: &Recovered<OpTransactionSigned>,
        tx_req: &DepositTxRequest,
        logs: &[alloy_primitives::Log],
        observations: Observations<'_>,
    ) -> (AuditResult, bool, bool) {
        if let Some(reason) = unadjudicable_observations(&self.rules, observations) {
            return (unadjudicable_result(self, &tx_req.source_hash, reason), false, true);
        }
        let verdict_result = super::verdict::verdict_for(
            tx_req,
            ExecutedTx { from: tx.signer(), to: tx.to(), tx_hash: tx.tx_hash(), value: tx.value() },
            logs,
            &self.rules,
            observations,
        );
        // `verdict_for` already reports `Objected` only for a deny the matcher attributes to a
        // selector-bearing rule; an event-only deny keeps the observation-set diagnostic.
        let identification = verdict_result
            .callstack_identification
            .unwrap_or(CallstackIdentification::NotApplicable);
        let (commit, keep_going) = commit_and_continue(verdict_result.verdict);
        (
            verdict_result.with_identification(self.identification(identification)),
            commit,
            keep_going,
        )
    }
}

/// The non-verdict for a transaction whose executed code could not be identified, with the
/// bounded diagnostic that used to come from the matcher's own failure path.
///
/// Bounded deliberately: the request's `source_hash`, the fixed low-cardinality reason token and
/// the COUNT of rules carrying a call-frame selector — the rule surface, not the deduplicated
/// selector address set. No rule id, no observed address, no code hash.
fn unadjudicable_result(
    plan: &AuditBatchPlan,
    source_hash: &str,
    reason: ObservationUnavailable,
) -> AuditResult {
    tracing::warn!(
        target: "xlayer_audit_rpc",
        %source_hash,
        reason = reason.as_str(),
        selector_bearing_rules = plan.rules.selector_bearing_rule_count(),
        "observations unavailable for a selector-bearing request; returning the non-verdict"
    );
    AuditResult::malformed(source_hash.to_string())
        .with_identification(plan.identification(CallstackIdentification::NotIdentified))
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

/// The one EVM-construction site for the two EVM shapes this path executes.
///
/// The handler and the test seam both go through here, so the inspector branch and the plain
/// branch exist once. With no inspector the plain EVM is built and no capture runs.
fn execute_audit_batch<DB>(
    factory: &OpEvmFactory,
    db: DB,
    evm_env: EvmEnv<OpSpecId>,
    plan: &AuditBatchPlan,
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
            plan.run(&mut evm, txs, reqs)
        }
        None => {
            let mut evm = factory.create_evm(db, evm_env);
            plan.run(&mut evm, txs, reqs)
        }
    }
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

/// Emits the per-class counter and the bounded anomaly log for one synthetic replay prefix's
/// identification.
///
/// The prefix's `Allow` and its state commit are unconditional and are decided by the caller;
/// nothing here can change either. This is the runtime observability that makes a compromised or
/// unidentifiable replay prefix visible, because the consumer skips such a prefix's verdict
/// entirely and so an optional response field alone would reach no operator.
///
/// Bounded exactly like the request-level non-verdict diagnostic: the request's `source_hash`,
/// the executed transaction hash, the fixed outcome token and the COUNT of rules carrying a
/// call-frame selector (never the deduplicated selector address count). No rule id
/// (`MatchOutcome::Deny` carries none and `MatchError`'s `Display` does, which is why neither is
/// formatted here), no observed address, no code hash, no log content.
fn report_synthetic_ban_identification(
    metrics: &SyntheticBanIdentificationMetrics,
    tx_hash: alloy_primitives::B256,
    source_hash: &str,
    identification: CallstackIdentification,
    selector_bearing_rules: usize,
) {
    metrics.counter(identification).increment(1);
    match identification {
        // Nothing anomalous happened, so the counter alone carries it.
        CallstackIdentification::Clean | CallstackIdentification::NotApplicable => {}
        CallstackIdentification::Objected => {
            tracing::error!(
                target: "xlayer_audit_rpc::synthetic_ban_identification",
                %tx_hash,
                %source_hash,
                outcome = "objected",
                selector_bearing_rules,
                "replay prefix executed code a rule objects to; reported, its allow and commit are unchanged"
            );
        }
        CallstackIdentification::NotIdentified => {
            tracing::error!(
                target: "xlayer_audit_rpc::synthetic_ban_identification",
                %tx_hash,
                %source_hash,
                outcome = "not_identified",
                selector_bearing_rules,
                "replay prefix executed code could not be identified; reported, its allow and commit are unchanged"
            );
        }
    }
}

/// The separately filterable record that a synthetic replay prefix exceeded the call-frame
/// observation budget and was STILL committed under the synthetic-ban exception.
///
/// It complements, and never replaces, the `not_identified` counter and log above: those cover
/// every unidentifiable prefix whatever the reason, this event only the over-budget one, so an
/// operator can select it without reading every `not_identified` as an over-budget event. Its
/// fields are fixed and low-cardinality, bounded exactly like the identification log. The
/// prefix's `Allow` and its commit are decided by the caller and are unchanged by this.
fn report_synthetic_ban_budget_exceeded(
    tx_hash: alloy_primitives::B256,
    source_hash: &str,
    selector_bearing_rules: usize,
) {
    tracing::warn!(
        target: "xlayer_audit_rpc::synthetic_ban_budget",
        %tx_hash,
        %source_hash,
        outcome = "not_identified",
        reason = ObservationUnavailable::BudgetExceeded.as_str(),
        committed = true,
        selector_bearing_rules,
        "replay prefix exceeded the call-frame observation budget; committed under the synthetic-ban exception"
    );
}

/// The reported call-frame identification of a `known_allowed` entry whose verdict its own class
/// contract fixes: observation only, never acted on.
///
/// The unadjudicable test is the SAME judgement the request-level gate applies, deliberately
/// reused rather than restated: a set is adjudicable only if it is available AND was produced
/// under this request's own snapshot. Testing only the available dimension would let an
/// `Available` set carrying a FOREIGN stamp reach the classifier, where a narrowed rule is
/// skipped with zero decoding and the entry would be reported `Clean` — "identified, nothing
/// matched" about frames this snapshot cannot adjudicate at all. A request declaring no alias
/// still falls through to the classifier and keeps its `NotApplicable` report, because the
/// judgement returns `None` there.
fn diagnostic_identification(
    plan: &AuditBatchPlan,
    tx: &Recovered<OpTransactionSigned>,
    tx_req: &DepositTxRequest,
    logs: &[alloy_primitives::Log],
    observations: Observations<'_>,
) -> CallstackIdentification {
    if unadjudicable_observations(&plan.rules, observations).is_some() {
        return CallstackIdentification::NotIdentified;
    }
    let Some(callstack_rules) = plan.known_allowed_rules.as_ref() else {
        return CallstackIdentification::NotApplicable;
    };
    // The same shared classifier the other path uses — the prefix's verdict is computed
    // identically, it is simply not acted on.
    let adjudicated = super::verdict::verdict_for(
        tx_req,
        ExecutedTx { from: tx.signer(), to: tx.to(), tx_hash: tx.tx_hash(), value: tx.value() },
        logs,
        callstack_rules,
        observations,
    );
    match adjudicated.verdict {
        Verdict::Deny => CallstackIdentification::Objected,
        Verdict::Malformed => CallstackIdentification::NotIdentified,
        _ => CallstackIdentification::Clean,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditTransactionsResponse {
    pub results: Vec<AuditResult>,
}

#[rpc(server, namespace = "xlayer")]
pub trait XlayerAuditApi {
    /// Classifies each deposit tx in `req.txs` against `req.rules`, without side effects.
    ///
    /// A tx whose `source_hash` is in `req.known_allowed` is never re-classified on the
    /// quota/event dimension. A synthetic ban prefix is `allow` and committed; an approved
    /// original is re-adjudicated on the call-frame rules only, so it can become `deny` or
    /// `malformed` but never `audit` (see [`KnownAllowedKind`] and
    /// `AuditTransactionsRequest::known_allowed`). A verdict describes the request as submitted
    /// and is never evidence of where the request or any entry came from.
    ///
    /// Each transaction has its own call-frame observation budget of 1 000. Over it, an ordinary
    /// deposit or an approved original is `malformed` and not committed; a synthetic ban prefix —
    /// labelled `synthetic_ban`, or recognised by the content-binding check when no label map is
    /// sent — stays `allow`, is committed (it rebuilds the base state) and is reported
    /// `not_identified`.
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
        // Every request-level check, the rule compilation and the callstack-only projection, in
        // the one place a test can drive them too.
        let plan = AuditBatchPlan::prepare(&req)?;

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

        // `ordered_txs`/`ordered_tx_reqs`/`txs`/`built`/`plan` all move into the closure below:
        // assembling each request's `AuditResult` has to happen there too, since the executed
        // subset's logs (`ordered_txs`-indexed) never leave the blocking task.
        let backend = self.backend.as_ref().clone();
        let block_id = BlockId::hash(latest_header.hash());
        let txs = req.txs;
        let results: Vec<AuditResult> = backend
            .spawn_with_state_at_block(block_id, move |this, mut state_db| {
                // When the request declares at least one call-frame selector the shared capture
                // inspector is attached in `CallstackOnly` mode — the SAME inspector the payload
                // builder uses, which is what makes equal identification structural rather than
                // coincidental. With no selector declared, NO inspector is constructed at all
                // and this path takes its pre-existing plain EVM.
                let inspector =
                    callstack_only_inspector(&plan.rules, AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX);
                let mut executed = execute_audit_batch(
                    this.evm_config().evm_factory(),
                    &mut state_db,
                    evm_env,
                    &plan,
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

/// Cross-crate test access to this path's own complete production batch.
///
/// Compiled only with the non-default `test-utils` feature, which no `[dependencies]` entry
/// enables — a production build contains none of it. It exists so a test that has to live in
/// another crate (the equal-identification comparison across the two execution paths) can drive
/// **this** path's real code instead of re-implementing it, which would make the comparison prove
/// nothing.
#[cfg(feature = "test-utils")]
pub mod test_seams {
    use super::*;
    use rcs_filter::callstack::SnapshotStamp;

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

    /// The class of an unlabelled `known_allowed` entry, decided from the built deposit the
    /// batch executes. A request can never carry a non-deposit, so this is the only boundary that
    /// can exercise the non-deposit arm of the private classifier.
    pub fn inferred_known_allowed_kind(tx: &Recovered<OpTransactionSigned>) -> KnownAllowedKind {
        super::inferred_known_allowed_kind(tx)
    }

    fn run_complete_batch<DB: Database + DatabaseCommit>(
        req: &AuditTransactionsRequest,
        db: DB,
        evm_env: EvmEnv<OpSpecId>,
        capture_stamp: Option<SnapshotStamp>,
    ) -> Result<Vec<AuditResult>, jsonrpsee::types::ErrorObjectOwned> {
        let plan = super::AuditBatchPlan::prepare(req)?;
        let (txs, reqs, built) = super::build_deposits(&req.txs);
        let inspector = match capture_stamp {
            None => callstack_only_inspector(&plan.rules, AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX),
            Some(stamp) => plan.rules.has_callstack_rules().then(|| {
                RcsInspector::callstack_only(
                    AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX,
                    CallstackCaptureConfig::new(stamp, plan.rules.callstack_selectors().to_vec()),
                )
            }),
        };
        // Explicitly `super::`: the public seam of the same name would otherwise shadow the
        // private production function.
        let mut executed = super::execute_audit_batch(
            &OpEvmFactory::default(),
            db,
            evm_env,
            &plan,
            inspector,
            &txs,
            &reqs,
        )
        .map_err(|error| {
            jsonrpsee::types::ErrorObjectOwned::owned(-32000, error.to_string(), None::<()>)
        })?;
        Ok(super::assemble_results(&req.txs, &built, &mut executed))
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
            callstack_identification: None,
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
                callstack_identification: None,
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
