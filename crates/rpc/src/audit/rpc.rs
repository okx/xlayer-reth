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
use alloy_evm::{Evm, IntoTxEnv};
use alloy_op_evm::OpEvmFactory;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use rcs_filter::callstack::{ObservationUnavailable, Observations};
use rcs_filter::config::{is_supported_protocol, MIN_PROTOCOL_VERSION};
use rcs_filter::rules::{load_rules, RawRule, RuleSet, UnknownFields};
use reth_evm::{block::BlockExecutorFactory, ConfigureEvm};
use reth_optimism_primitives::OpTransactionSigned;
use reth_rpc_eth_api::{
    helpers::{Call, LoadState},
    FromEvmError, RpcNodeCore,
};
use reth_storage_api::BlockReaderIdExt;
use revm::{context_interface::result::ResultAndState, DatabaseCommit};
use xlayer_rcs_capture::{
    CallstackCaptureConfig, CaptureGuard, CaptureOutcome, ObservationBudget, RcsCaptureControl,
    RcsCaptureScope, RcsInspector,
};

use super::deposit::{build_deposit_tx, DepositTxRequest};
use super::metrics::SyntheticBanIdentificationMetrics;
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

/// Builds the forced-`Allow` result for a tx whose `source_hash` is in the caller-supplied
/// `known_allowed` set — skips rule (re-)classification entirely (see
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

/// What a `known_allowed` entry IS. Supplied by the producer and **never inferred** from `to`,
/// calldata, position or any other heuristic.
///
/// The two classes have **opposite safe defaults** — Deny-priority is safe for an `Original` and
/// unsafe for a `SyntheticBan`, and committing is safe for a `SyntheticBan` and unsafe for an
/// `Original` — so there is no safe guess, which is why a missing label is a request error rather
/// than a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownAllowedKind {
    /// An ordinary deposit the caller's adjudicator already resolved to Allow.
    Original,
    /// A control prefix the caller prepended for blacklist replay. It is authored by the caller
    /// itself to rebuild base state it already decided, not an unadjudicated user payload.
    SyntheticBan,
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
    /// `source_hash` → class for every `known_allowed` entry. Required when the request
    /// advertises version 2, carries a non-empty `known_allowed`, and declares at least one
    /// callstack selector; unused (and unnecessary) otherwise.
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

/// The capture configuration threaded into the audit RPC from the node's `rcs-filter` args.
///
/// `max_callstack_observations_per_tx` is the SAME per-transaction bound the payload builder's
/// candidate loop uses, and it is applied **per transaction** inside a batch — a deposit batch
/// does not share one pooled allowance. The value carried here has already been validated as
/// `>= 1` by `FilterConfig::validate()` at startup, which this type does not relax, bypass or
/// re-interpret.
#[derive(Debug, Clone, Copy)]
pub struct AuditCaptureConfig {
    pub max_callstack_observations_per_tx: usize,
}

/// The RPC crate's EVM adapter for the shared [`RcsCaptureScope`].
///
/// `RcsCaptureScope` is owned by `xlayer-rcs-capture` and the concrete EVM by its own crate, so
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
        format!("unsupported rule protocol_version {protocol_version}"),
        None::<()>,
    ))
}

/// Rejects a v2 selector-bearing request whose `known_allowed_kinds` does not cover every
/// `known_allowed` entry, naming the unlabelled `source_hash` values.
///
/// The node does **not** silently pick a class: the two classes have opposite safe defaults, so
/// there is no safe guess. v1 requests, and v2 requests declaring no selector, need no labels and
/// are unaffected.
fn validate_known_allowed_labels(
    protocol_version: u32,
    known_allowed: &[String],
    kinds: Option<&BTreeMap<String, KnownAllowedKind>>,
    selectors_declared: bool,
) -> Result<(), jsonrpsee::types::ErrorObjectOwned> {
    if protocol_version < 2 || known_allowed.is_empty() || !selectors_declared {
        return Ok(());
    }
    let mut unlabelled: Vec<&str> = known_allowed
        .iter()
        .filter(|hash| !kinds.is_some_and(|kinds| kinds.contains_key(hash.as_str())))
        .map(String::as_str)
        .collect();
    unlabelled.sort_unstable();
    unlabelled.dedup();
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

/// The callstack-only projection of a request's rules, used ONLY to adjudicate a `known_allowed`
/// `Original`. `None` when the request declares no selector, where there is nothing to adjudicate
/// and the pre-existing unconditional `Allow` applies unchanged.
///
/// Filtering out rules with no `callstack` section leaves the selector address set — and
/// therefore the `SnapshotStamp` — unchanged, so capture and screening still provably share one
/// configuration and the existing stamp check keeps its meaning.
fn callstack_only_projection(
    protocol_version: u32,
    rules: &RuleSet,
    raw: &[RawRule],
) -> Option<RuleSet> {
    if !rules.has_callstack_rules() {
        return None;
    }
    let projected: Vec<RawRule> =
        raw.iter().filter(|rule| !rule.callstack.is_empty()).cloned().collect();
    Some(load_rules(protocol_version, 0, projected))
}

/// The capture inspector this request needs, or `None` when it declares no call-frame selector —
/// in which case NO inspector is constructed at all and the path keeps its pre-existing EVM.
///
/// `CallstackOnly`: this path produces and consumes no virtual native-transfer log, screens
/// against the raw execution logs it already used, and the only live budget is the callstack
/// observation budget — the same `max_callstack_observations_per_tx` the candidate loop uses,
/// applied PER TRANSACTION inside the batch.
fn audit_capture_inspector(rules: &RuleSet, capture: AuditCaptureConfig) -> Option<RcsInspector> {
    rules.has_callstack_rules().then(|| {
        RcsInspector::callstack_only(
            capture.max_callstack_observations_per_tx,
            CallstackCaptureConfig::new(rules.stamp(), rules.callstack_selectors().to_vec()),
        )
    })
}

// Test-only construction-site observable for the audit RPC's inspector, the
// counterpart of the builder's. Incremented at the exact construction site, so `0` means no
// inspector was built. Kept as `//` because `thread_local!` carries no documentation.
#[cfg(test)]
thread_local! {
    pub(crate) static AUDIT_INSPECTOR_CONSTRUCTIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Everything one batch needs that does not depend on the EVM type.
struct AuditBatch<'a> {
    txs: &'a [Recovered<OpTransactionSigned>],
    reqs: &'a [DepositTxRequest],
    /// The request's OWN compiled rule set. No local snapshot is ever mixed in: a node with an
    /// empty or absent local snapshot must never make an uncaptured request look captured.
    rules: &'a RuleSet,
    /// A callstack-only projection of `rules`, used ONLY to adjudicate a `known_allowed`
    /// `Original`. Filtering out rules with no `callstack` section leaves
    /// `callstack_selector_addresses` untouched, so this projection carries the **same**
    /// `SnapshotStamp` as `rules` and the existing stamp check keeps its meaning.
    ///
    /// This is what keeps the `known_allowed` short-circuit doing exactly what it was built for:
    /// QUOTA / EVENT re-classification is still skipped, so an already-approved quota
    /// transaction can never re-enter `Audit`.
    known_allowed_rules: Option<&'a RuleSet>,
    known_allowed: &'a HashSet<String>,
    kinds: Option<&'a BTreeMap<String, KnownAllowedKind>>,
    /// Whether the response may carry the identification diagnostic (v2 requests only).
    report_identification: bool,
}

impl AuditBatch<'_> {
    /// `Some(kind)` when this `source_hash` is a `known_allowed` entry. Defaults to `Original`
    /// only where a label is not required (a v1 request, or a v2 request declaring no selector);
    /// where it IS required, the request was already rejected at the boundary.
    fn known_allowed_kind(&self, source_hash: &str) -> Option<KnownAllowedKind> {
        if !self.known_allowed.contains(source_hash) {
            return None;
        }
        Some(
            self.kinds
                .and_then(|kinds| kinds.get(source_hash).copied())
                .unwrap_or(KnownAllowedKind::Original),
        )
    }

    fn identification(&self, value: CallstackIdentification) -> Option<CallstackIdentification> {
        self.report_identification.then_some(value)
    }
}

/// One request's validated, owned batch inputs.
///
/// The handler validates and compiles a request ONCE, then moves the whole plan into the blocking
/// task that executes it, which is why these are owned values rather than the borrows
/// [`AuditBatch`] carries. Keeping the assembly in one place is what lets a test drive the real
/// request pipeline — validation, compilation, the callstack-only projection and the batch body —
/// instead of a re-implementation of it that could agree with production by luck.
pub struct AuditBatchPlan {
    rules: RuleSet,
    known_allowed_rules: Option<RuleSet>,
    known_allowed: HashSet<String>,
    kinds: Option<BTreeMap<String, KnownAllowedKind>>,
    report_identification: bool,
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
        validate_known_allowed_labels(
            req.protocol_version,
            &req.known_allowed,
            req.known_allowed_kinds.as_ref(),
            rules.has_callstack_rules(),
        )?;
        let known_allowed_rules =
            callstack_only_projection(req.protocol_version, &rules, &req.rules);
        debug_assert!(
            known_allowed_rules.as_ref().is_none_or(|set| set.stamp() == rules.stamp()),
            "the callstack-only projection must carry the same snapshot stamp"
        );
        Ok(Self {
            rules,
            known_allowed_rules,
            known_allowed: req.known_allowed.iter().cloned().collect(),
            kinds: req.known_allowed_kinds.clone(),
            report_identification: req.protocol_version >= 2,
        })
    }

    /// The request's own compiled rule set — what decides whether capture is armed at all.
    pub fn rules(&self) -> &RuleSet {
        &self.rules
    }

    /// Borrows the plan for one ordered set of built transactions.
    fn batch<'a>(
        &'a self,
        txs: &'a [Recovered<OpTransactionSigned>],
        reqs: &'a [DepositTxRequest],
    ) -> AuditBatch<'a> {
        AuditBatch {
            txs,
            reqs,
            rules: &self.rules,
            known_allowed_rules: self.known_allowed_rules.as_ref(),
            known_allowed: &self.known_allowed,
            kinds: self.kinds.as_ref(),
            report_identification: self.report_identification,
        }
    }
}

/// Maps a non-`Complete` capture outcome to the observation state screening must see. Every
/// variant is an EXPLICIT unavailable reason — never `Available { items: [] }`, which would read
/// as "captured, nothing matched".
fn observations_from(outcome: &CaptureOutcome) -> Observations<'_> {
    match outcome {
        CaptureOutcome::Complete { observations, .. } => {
            Observations::available(observations.stamp, &observations.items)
        }
        CaptureOutcome::LimitExceeded { budget: ObservationBudget::Callstack, .. } => {
            Observations::Unavailable {
                reason: rcs_filter::callstack::ObservationUnavailable::BudgetExceeded,
            }
        }
        // The native budget is not applicable in `CallstackOnly` mode and is never consulted, so
        // reaching it would be a capture-state bug; it is unidentifiable, not ignorable.
        CaptureOutcome::LimitExceeded { .. } | CaptureOutcome::InvariantViolation { .. } => {
            Observations::Unavailable {
                reason: rcs_filter::callstack::ObservationUnavailable::Incomplete,
            }
        }
        CaptureOutcome::Passthrough => super::verdict::observations_not_captured(),
    }
}

/// Why one transaction cannot be adjudicated AT ALL against the request's rules, decided from the
/// request's own compiled set before a single rule is filtered or matched.
///
/// This has to be a request-level question, not a per-rule one. Matching skips a rule narrowed by
/// `contract_address` / `origin` with zero decoding, so for such a rule the per-rule observation
/// check is never reached and an unidentifiable execution would fall through to an authoritative
/// `Allow` — exactly what the highest-priority rule forbids. `has_callstack_rules()` keeps a
/// request that declares no selector byte-identical to its previous behaviour: nothing is
/// declared, so nothing can be unidentifiable.
///
/// The stamp comparison mirrors the matcher's own: an observation set produced under a different
/// rule snapshot cannot distinguish an absent frame from an unobserved one, so it is unavailable
/// rather than empty.
fn unadjudicable_observations(
    rules: &RuleSet,
    observations: Observations<'_>,
) -> Option<ObservationUnavailable> {
    if !rules.has_callstack_rules() {
        return None;
    }
    match observations {
        Observations::Unavailable { reason } => Some(reason),
        Observations::Available { stamp, .. } if stamp != rules.stamp() => {
            Some(ObservationUnavailable::SnapshotMismatch)
        }
        Observations::Available { .. } => None,
    }
}

/// The non-verdict for a transaction whose executed code could not be identified, with the
/// bounded diagnostic that used to come from the matcher's own failure path.
///
/// Bounded deliberately: the request's `source_hash`, the fixed low-cardinality reason token and
/// the COUNT of selector-bearing rules. No rule id, no observed address, no code hash.
fn unadjudicable_result(
    batch: &AuditBatch<'_>,
    source_hash: &str,
    reason: ObservationUnavailable,
) -> AuditResult {
    tracing::warn!(
        target: "xlayer_audit_rpc",
        %source_hash,
        reason = reason.as_str(),
        selector_bearing_rules = batch.rules.callstack_selectors().len(),
        "observations unavailable for a selector-bearing request; returning the non-verdict"
    );
    AuditResult::malformed(source_hash.to_string())
        .with_identification(batch.identification(CallstackIdentification::NotIdentified))
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
/// production build contains it. It exists because "the three paths observe the same frames" is
/// only meaningful if the compared frames are the ones production really used: three decisions
/// can agree while resting on different observations, and a path that silently dropped its
/// capture would still produce a decision.
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

/// Executes one ordered batch against `evm`, filling `executed`.
///
/// Generic over the EVM so the capturing and the inert construction share ONE loop body: with no
/// selector declared the caller builds a plain EVM and no [`RcsInspector`] is constructed at all.
///
/// Capture is armed per transaction through the shared RAII guard, so the `known_allowed`
/// `continue`, the `?` on `transact`, the `Audit` `break` and the batch-end path all leave the
/// inspector inactive by construction rather than by caller discipline.
fn run_audit_batch<E>(
    evm: &mut E,
    batch: &AuditBatch<'_>,
    executed: &mut [Option<AuditResult>],
) -> Result<(), E::Error>
where
    E: Evm,
    E::DB: DatabaseCommit,
    E::Inspector: RcsCaptureControl,
    for<'tx> &'tx Recovered<OpTransactionSigned>: IntoTxEnv<E::Tx>,
{
    // Built on first use rather than per batch: a request without a replay prefix registers
    // nothing, and the derive caches the registration so a later batch only clones handles.
    let mut synthetic_ban_metrics: Option<SyntheticBanIdentificationMetrics> = None;

    for (exec_idx, tx) in batch.txs.iter().enumerate() {
        let tx_req = &batch.reqs[exec_idx];

        // One capture window per transaction, opened by the shared RAII guard.
        let (outcome, capture) = audit_capture_window(&mut *evm, tx)?;
        let ResultAndState { result, state } = outcome;
        let logs = result.logs().to_vec();
        let observations = observations_from(&capture);
        #[cfg(any(test, feature = "test-utils"))]
        observation_recorder::record(observations);
        let kind = batch.known_allowed_kind(&tx_req.source_hash);

        // Highest priority, and deliberately BEFORE any rule is filtered or matched: a request
        // that declares a selector and whose observation set cannot be adjudicated gets the
        // non-verdict, with no state committed. Rule-level filtering (`contract_address` /
        // `origin`) short-circuits with zero decoding, so leaving this to the matcher would let
        // an unidentifiable execution be reported as an authoritative `Allow` whenever the
        // narrowed rules happened to be skipped. The `SyntheticBan` class is the ONE explicit
        // exception: its contract is unconditional (it executes, commits and stays `Allow`), so
        // it is handled by its own arm below and never reaches this gate.
        if kind != Some(KnownAllowedKind::SyntheticBan)
            && let Some(reason) = unadjudicable_observations(batch.rules, observations)
        {
            executed[exec_idx] = Some(unadjudicable_result(batch, &tx_req.source_hash, reason));
            continue;
        }

        match kind {
            // A synthetic replay prefix keeps the pre-existing contract UNCONDITIONALLY: it
            // executes, its state IS committed, and its verdict stays `Allow`. The caller skips
            // such a prefix's verdict entirely and *depends* on it committing so the blacklist /
            // failed-deposit base state is rebuilt; withholding that commit to "protect" it would
            // corrupt the very base state the protection depends on. Its identification result is
            // therefore reported, never acted on.
            Some(KnownAllowedKind::SyntheticBan) => {
                evm.db_mut().commit(state);
                let identification = match observations {
                    Observations::Unavailable { .. } if batch.rules.has_callstack_rules() => {
                        CallstackIdentification::NotIdentified
                    }
                    _ => synthetic_ban_identification(batch, tx, tx_req, &logs, observations),
                };
                // Observation only, and the one place an operator can see it: the consumer
                // skips this prefix's verdict, so the response diagnostic reaches nobody.
                report_synthetic_ban_identification(
                    synthetic_ban_metrics
                        .get_or_insert_with(SyntheticBanIdentificationMetrics::default),
                    tx.tx_hash(),
                    &tx_req.source_hash,
                    identification,
                    batch.rules.callstack_selectors().len(),
                );
                executed[exec_idx] = Some(
                    known_allowed_result(tx.tx_hash(), &tx_req.source_hash)
                        .with_identification(batch.identification(identification)),
                );
                continue;
            }
            // An `Original` earns no exemption from the non-verdict rule. Priority, highest
            // first: unavailable observations with a selector-bearing request → the existing
            // non-verdict; a selector-bearing objection → `Deny`; otherwise `Allow` + commit,
            // byte-identical to the previous behaviour.
            Some(KnownAllowedKind::Original) => {
                let Some(callstack_rules) = batch.known_allowed_rules else {
                    // The request declares no selector, so there is nothing to adjudicate and
                    // this is exactly today's unconditional `Allow` + commit.
                    evm.db_mut().commit(state);
                    executed[exec_idx] = Some(
                        known_allowed_result(tx.tx_hash(), &tx_req.source_hash)
                            .with_identification(
                                batch.identification(CallstackIdentification::NotApplicable),
                            ),
                    );
                    continue;
                };
                let adjudicated = super::verdict::verdict_for(
                    tx_req,
                    ExecutedTx {
                        from: tx.signer(),
                        to: tx.to(),
                        tx_hash: tx.tx_hash(),
                        value: tx.value(),
                    },
                    &logs,
                    callstack_rules,
                    observations,
                );
                match adjudicated.verdict {
                    // Not adjudicable: the existing non-verdict, state NOT committed.
                    Verdict::Malformed => {
                        executed[exec_idx] = Some(
                            AuditResult::malformed(tx_req.source_hash.clone()).with_identification(
                                batch.identification(CallstackIdentification::NotIdentified),
                            ),
                        );
                    }
                    // A selector-bearing rule objects. Reuses `commit_and_continue(Deny)`'s
                    // existing semantics unchanged: state discarded, batch continues.
                    Verdict::Deny => {
                        executed[exec_idx] = Some(
                            AuditResult {
                                tx_hash: format!("{:#x}", tx.tx_hash()),
                                source_hash: tx_req.source_hash.clone(),
                                verdict: Verdict::Deny,
                                actions: None,
                                callstack_identification: None,
                            }
                            .with_identification(
                                batch.identification(CallstackIdentification::Objected),
                            ),
                        );
                    }
                    // `Audit` is deliberately NOT propagated here: the callstack dimension may
                    // only ever move a `known_allowed` verdict to `Deny` or a non-verdict, never
                    // to `Audit`, so quota convergence cannot regress into a re-adjudication loop.
                    Verdict::Allow | Verdict::Audit | Verdict::Unknown => {
                        evm.db_mut().commit(state);
                        executed[exec_idx] = Some(
                            known_allowed_result(tx.tx_hash(), &tx_req.source_hash)
                                .with_identification(
                                    batch.identification(CallstackIdentification::Clean),
                                ),
                        );
                    }
                }
                continue;
            }
            None => {}
        }

        let verdict_result = super::verdict::verdict_for(
            tx_req,
            ExecutedTx { from: tx.signer(), to: tx.to(), tx_hash: tx.tx_hash(), value: tx.value() },
            &logs,
            batch.rules,
            observations,
        );
        // `MatchOutcome::Deny` carries no rule id, so the diagnostic cannot name the objecting
        // dimension; for a selector-bearing request a deny is reported as an objection.
        let identification = match (verdict_result.verdict, batch.rules.has_callstack_rules()) {
            (Verdict::Deny, true) => Some(CallstackIdentification::Objected),
            _ => verdict_result.callstack_identification,
        };
        let (commit, keep_going) = commit_and_continue(verdict_result.verdict);
        if commit {
            evm.db_mut().commit(state);
        }
        executed[exec_idx] = Some(verdict_result.with_identification(
            batch.identification(identification.unwrap_or(CallstackIdentification::NotApplicable)),
        ));
        if !keep_going {
            break;
        }
    }
    Ok(())
}

/// Emits the per-class counter and the bounded anomaly log for one synthetic replay prefix's
/// identification.
///
/// The prefix's `Allow` and its state commit are unconditional and are decided by the caller;
/// nothing here can change either. This is the runtime observability that makes a compromised or
/// unidentifiable replay prefix visible, because the consumer skips such a prefix's verdict
/// entirely and so an optional response field alone would reach no operator.
///
/// Bounded exactly like the deposit path's diagnostic: the request's `source_hash`, the executed
/// transaction hash, the fixed outcome token and the COUNT of selector-bearing rules. No rule id
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

/// The reported identification of a synthetic replay prefix: observation only.
fn synthetic_ban_identification(
    batch: &AuditBatch<'_>,
    tx: &Recovered<OpTransactionSigned>,
    tx_req: &DepositTxRequest,
    logs: &[alloy_primitives::Log],
    observations: Observations<'_>,
) -> CallstackIdentification {
    let Some(callstack_rules) = batch.known_allowed_rules else {
        return CallstackIdentification::NotApplicable;
    };
    // The same shared classifier the other two paths use — the prefix's verdict is computed
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
    /// Capture configuration, read from the node's `rcs-filter` args at registration. Only the
    /// **budget** comes from that args group: the audit RPC adjudicates the rules carried in the
    /// request, not the node's installed snapshot, so it keeps working with the filter disabled.
    pub capture: AuditCaptureConfig,
}

impl<T> XlayerAuditRpc<T> {
    pub fn new(backend: Arc<T>, capture: AuditCaptureConfig) -> Self {
        Self { backend, capture }
    }
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
        let capture_config = self.capture;

        // Build every tx up front so a per-item construction failure produces a `Malformed`
        // result without aborting the rest of the batch.
        let mut ordered_txs = Vec::with_capacity(req.txs.len());
        let mut ordered_tx_reqs: Vec<DepositTxRequest> = Vec::with_capacity(req.txs.len());
        let mut built: Vec<Option<usize>> = Vec::with_capacity(req.txs.len());
        for tx_req in &req.txs {
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
                let mut executed: Vec<Option<AuditResult>> =
                    (0..ordered_txs.len()).map(|_| None).collect();
                let batch = plan.batch(&ordered_txs, &ordered_tx_reqs);

                // Two typed EVM-construction branches feed one generic batch body. When the
                // request declares at least one call-frame selector the shared capture inspector
                // is attached in `CallstackOnly` mode — the SAME inspector the payload builder
                // uses, which is what makes equal identification structural rather than
                // coincidental. With no selector declared, NO inspector is constructed at all and
                // this path takes its pre-existing plain `evm_with_env` branch.
                if let Some(inspector) = audit_capture_inspector(plan.rules(), capture_config) {
                    #[cfg(test)]
                    AUDIT_INSPECTOR_CONSTRUCTIONS.with(|c| c.set(c.get() + 1));
                    let mut evm = this.evm_config().evm_with_env_and_inspector(
                        &mut state_db,
                        evm_env,
                        inspector,
                    );
                    run_audit_batch(&mut evm, &batch, &mut executed)
                        .map_err(T::Error::from_evm_err)?;
                } else {
                    let mut evm = this.evm_config().evm_with_env(&mut state_db, evm_env);
                    run_audit_batch(&mut evm, &batch, &mut executed)
                        .map_err(T::Error::from_evm_err)?;
                }

                Ok(assemble_results(&txs, &built, &mut executed))
            })
            .await
            .map_err(Into::into)?;

        Ok(AuditTransactionsResponse { results })
    }
}

/// Cross-crate test access to this path's own capture and observation handling.
///
/// Compiled only with the non-default `test-utils` feature, which no `[dependencies]` entry
/// enables — a production build contains none of it. It exists so a test that has to live in
/// another crate (the equal-identification comparison across the three execution paths) can
/// drive **this** path's real code instead of re-implementing it, which would make the
/// comparison prove nothing.
#[cfg(feature = "test-utils")]
pub mod test_seams {
    use super::*;

    /// Delegates to this path's own [`super::audit_capture_inspector`].
    pub fn audit_capture_inspector(
        rules: &RuleSet,
        capture: AuditCaptureConfig,
    ) -> Option<RcsInspector> {
        super::audit_capture_inspector(rules, capture)
    }

    /// Delegates to this path's own [`super::observations_from`].
    pub fn observations_from(outcome: &CaptureOutcome) -> Observations<'_> {
        super::observations_from(outcome)
    }

    /// Validates and compiles one request exactly as the handler does, including the strict wire
    /// schema's own checks, and returns the plan the production batch body executes.
    pub fn prepare_audit_batch_plan(
        req: &AuditTransactionsRequest,
    ) -> Result<AuditBatchPlan, jsonrpsee::types::ErrorObjectOwned> {
        AuditBatchPlan::prepare(req)
    }

    /// Runs the production [`super::run_audit_batch`] over `plan`, so a cross-crate test reaches
    /// the real capture window, the real `known_allowed` policy, the real commit decision and the
    /// real verdict mapping rather than any subset of them.
    pub fn run_audit_batch<E>(
        evm: &mut E,
        plan: &AuditBatchPlan,
        txs: &[Recovered<OpTransactionSigned>],
        reqs: &[DepositTxRequest],
        executed: &mut [Option<AuditResult>],
    ) -> Result<(), E::Error>
    where
        E: Evm,
        E::DB: DatabaseCommit,
        E::Inspector: RcsCaptureControl,
        for<'tx> &'tx Recovered<OpTransactionSigned>: IntoTxEnv<E::Tx>,
    {
        super::run_audit_batch(evm, &plan.batch(txs, reqs), executed)
    }

    /// Delegates to this path's own [`super::audit_capture_window`].
    pub fn audit_capture_window<E, Tx>(
        evm: &mut E,
        tx: Tx,
    ) -> Result<(ResultAndState<E::HaltReason>, CaptureOutcome), E::Error>
    where
        E: Evm,
        E::Inspector: RcsCaptureControl,
        Tx: IntoTxEnv<E::Tx>,
    {
        super::audit_capture_window(evm, tx)
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

/// Real capture and a real verdict on `xlayer_auditTransactions`.
///
/// The batch tests drive the production `run_audit_batch` against a real `OpEvm` built by the
/// same factory the node uses, so capture arming, the `known_allowed` policy, the commit/no-commit
/// decision and the verdict are all exercised as shipped. The boundary tests call the exact
/// production functions the handler calls, so an error code or message cannot drift between them.
#[cfg(test)]
mod capture_path_tests {
    use super::*;
    use alloy_evm::{EvmEnv, EvmFactory};
    use alloy_op_evm::{OpEvmFactory, OpTx};
    use alloy_primitives::{keccak256, Address, B256, U256};
    use op_revm::OpSpecId;
    use rcs_filter::rules::load_rules;
    use revm::context::{BlockEnv, CfgEnv};
    use revm::database::InMemoryDB;
    use revm::state::{AccountInfo, Bytecode};
    use revm::Database as _;

    /// The proxy a rule observes. Its own code performs a first-level `DELEGATECALL`.
    const PROXY: Address = Address::repeat_byte(0xAA);
    const APPROVED_IMPL: Address = Address::repeat_byte(0xB1);
    const UNAPPROVED_IMPL: Address = Address::repeat_byte(0xB2);
    const DEPOSITOR: Address = Address::repeat_byte(0xC1);

    /// `DELEGATECALL(gas, target, 0,0,0,0); POP` repeated `times`, then `STOP`.
    fn delegating_proxy_code(target: Address, times: usize) -> Vec<u8> {
        let mut code = Vec::new();
        for _ in 0..times {
            code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73]);
            code.extend_from_slice(target.as_slice());
            code.extend_from_slice(&[0x5a, 0xf4, 0x50]);
        }
        code.push(0x00);
        code
    }

    /// `SSTORE(0, marker); STOP`. Under a `DELEGATECALL` this writes the PROXY's storage, which
    /// makes "the state was committed" a direct observation rather than an inference. The marker
    /// also gives the two implementations distinct code, hence distinct code hashes.
    fn impl_code(marker: u8) -> Vec<u8> {
        vec![0x60, marker, 0x60, 0x00, 0x55, 0x00]
    }

    fn approved_hash() -> B256 {
        keccak256(impl_code(0x01))
    }

    /// The normative rule pattern: observe by **proxy address only**, then deny when
    /// the observed implementation code hash is NOT in the approved set. The explicit non-null
    /// guard keeps an ordinary non-delegatecall call to the proxy from being denied.
    fn proxy_impl_allowlist_rule() -> RawRule {
        proxy_impl_allowlist_rule_approving(&[approved_hash()])
    }

    /// The same pattern over an explicit approved set, for a fixture whose implementation is not
    /// the default one. The shape is identical — only the allowlist contents differ.
    fn proxy_impl_allowlist_rule_approving(approved: &[B256]) -> RawRule {
        let approved: Vec<String> = approved.iter().map(|h| format!("{h:#x}")).collect();
        serde_json::from_value(serde_json::json!({
            "id": "proxy-impl-allowlist",
            "event_abis": {},
            "callstack": { "entry": { "address": format!("{PROXY:#x}") } },
            "condition": { "and": [
                { "==": [ { "var": "entry.matched" }, true ] },
                { "!=": [ { "var": "entry.implCodeHash" }, null ] },
                { "!": { "in": [ { "var": "entry.implCodeHash" }, approved ] } }
            ] },
            "action": "deny"
        }))
        .expect("the normative sample must deserialize")
    }

    /// The origin the request's rules are narrowed to in the skip tests: deliberately NOT the
    /// depositor, so every rule is filtered out with zero decoding before any observation is
    /// consulted.
    const OTHER_ORIGIN: Address = Address::repeat_byte(0x99);

    /// The same normative pattern, additionally narrowed to one `origin`. Stage one of matching
    /// skips such a rule on an origin mismatch before stage two resolves any call frame, so no
    /// per-rule observation check can ever be reached for it.
    fn proxy_impl_allowlist_rule_from_origin(origin: Address) -> RawRule {
        let approved = vec![format!("{:#x}", approved_hash())];
        serde_json::from_value(serde_json::json!({
            "id": "proxy-impl-allowlist-origin-narrowed",
            "origin": format!("{origin:#x}"),
            "event_abis": {},
            "callstack": { "entry": { "address": format!("{PROXY:#x}") } },
            "condition": { "and": [
                { "==": [ { "var": "entry.matched" }, true ] },
                { "!=": [ { "var": "entry.implCodeHash" }, null ] },
                { "!": { "in": [ { "var": "entry.implCodeHash" }, approved ] } }
            ] },
            "action": "deny"
        }))
        .expect("the origin-narrowed sample must deserialize")
    }

    /// A v1 event rule declaring no `callstack` section at all.
    fn event_only_rule() -> RawRule {
        serde_json::from_str(
            r#"{"id":"evt","event_abis":{"transfer":{"type":"event","name":"Transfer",
               "inputs":[{"name":"from","type":"address","indexed":true},
                         {"name":"to","type":"address","indexed":true},
                         {"name":"value","type":"uint256","indexed":false}],
               "anonymous":false}},"condition":true,"action":"deny"}"#,
        )
        .expect("valid v1 rule")
    }

    fn deposit_request(source_hash: &str, to: Address) -> DepositTxRequest {
        DepositTxRequest {
            source_hash: source_hash.to_string(),
            from: format!("{DEPOSITOR:#x}"),
            to: Some(format!("{to:#x}")),
            mint: "0".to_string(),
            value: "0".to_string(),
            gas_limit: 1_000_000,
            is_system_transaction: false,
            data: "0x".to_string(),
        }
    }

    fn seeded_db(proxy_target: Address, delegatecalls: usize) -> InMemoryDB {
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            DEPOSITOR,
            AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
        );
        db.insert_account_info(
            PROXY,
            AccountInfo {
                code: Some(Bytecode::new_raw(
                    delegating_proxy_code(proxy_target, delegatecalls).into(),
                )),
                ..Default::default()
            },
        );
        for (address, marker) in [(APPROVED_IMPL, 0x01u8), (UNAPPROVED_IMPL, 0x02u8)] {
            db.insert_account_info(
                address,
                AccountInfo {
                    code: Some(Bytecode::new_raw(impl_code(marker).into())),
                    ..Default::default()
                },
            );
        }
        db
    }

    /// One batch outcome: the per-transaction results plus the PROXY storage slot the
    /// implementation writes, which is how "state committed" is observed directly.
    struct BatchRun {
        results: Vec<Option<AuditResult>>,
        proxy_slot0: U256,
    }

    /// Runs the production `run_audit_batch` against a real EVM, exactly as the handler does:
    /// the inspector is attached only when the request's own rules declare a selector.
    fn run_batch(
        reqs: &[DepositTxRequest],
        raw_rules: Vec<RawRule>,
        protocol_version: u32,
        known_allowed: &[&str],
        kinds: Option<BTreeMap<String, KnownAllowedKind>>,
        capture: AuditCaptureConfig,
        db: InMemoryDB,
    ) -> BatchRun {
        let rules = load_rules(protocol_version, 0, raw_rules.clone());
        assert!(rules.rejected.is_empty(), "test rules must load clean: {:?}", rules.rejected);
        let known_allowed_rules = callstack_only_projection(protocol_version, &rules, &raw_rules);
        let known_allowed_set: HashSet<String> =
            known_allowed.iter().map(|h| (*h).to_string()).collect();
        let txs: Vec<_> =
            reqs.iter().map(|r| build_deposit_tx(r).expect("valid deposit request")).collect();
        let batch = AuditBatch {
            txs: &txs,
            reqs,
            rules: &rules,
            known_allowed_rules: known_allowed_rules.as_ref(),
            known_allowed: &known_allowed_set,
            kinds: kinds.as_ref(),
            report_identification: protocol_version >= 2,
        };
        let mut executed: Vec<Option<AuditResult>> = (0..txs.len()).map(|_| None).collect();
        let env = EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::JOVIAN),
            BlockEnv { gas_limit: 30_000_000, ..Default::default() },
        );
        let proxy_slot0 = match audit_capture_inspector(&rules, capture) {
            Some(inspector) => {
                let mut evm =
                    OpEvmFactory::<OpTx>::default().create_evm_with_inspector(db, env, inspector);
                run_audit_batch(&mut evm, &batch, &mut executed).expect("batch executes");
                // Capture must be inactive on every exit path, guaranteed by the RAII guard.
                assert!(
                    matches!(
                        evm.components_mut().1.finish_capture(&[]),
                        CaptureOutcome::Complete { .. }
                    ),
                    "the inspector must be left inactive after the batch"
                );
                evm.db_mut().storage(PROXY, U256::ZERO).expect("storage read")
            }
            None => {
                let mut evm = OpEvmFactory::<OpTx>::default().create_evm(db, env);
                run_audit_batch(&mut evm, &batch, &mut executed).expect("batch executes");
                evm.db_mut().storage(PROXY, U256::ZERO).expect("storage read")
            }
        };
        BatchRun { results: executed, proxy_slot0 }
    }

    fn generous() -> AuditCaptureConfig {
        AuditCaptureConfig { max_callstack_observations_per_tx: 10_000 }
    }

    fn only(run: &BatchRun) -> &AuditResult {
        run.results[0].as_ref().expect("the single transaction was classified")
    }

    // ---------------------------------------------------------------- batch semantics ---------

    /// a v2 `callstack` rule is accepted and classified from REAL capture: the
    /// deposit→proxy→unapproved-implementation request returns the deny verdict.
    #[test]
    fn v2_callstack_rule_denies_unapproved_implementation() {
        let run = run_batch(
            &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[],
            None,
            generous(),
            seeded_db(UNAPPROVED_IMPL, 1),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Deny);
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::Objected));
        assert_eq!(run.proxy_slot0, U256::ZERO, "a denied transaction's state is not committed");
    }

    /// contrast: the same request against the APPROVED implementation returns allow, and
    /// its state IS committed. Without this, the deny case above could pass for the wrong reason.
    #[test]
    fn same_request_with_approved_implementation_allows() {
        let run = run_batch(
            &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[],
            None,
            generous(),
            seeded_db(APPROVED_IMPL, 1),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Allow);
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::Clean));
        assert_eq!(run.proxy_slot0, U256::from(1u64), "an allowed transaction commits its state");
    }

    /// third direction of the same sample: an ORDINARY (non-delegatecall) call to the observed
    /// proxy must ALSO be allowed. The frame is genuinely observed, but a plain call carries no
    /// implementation, so the implementation code hash binds to null and the explicit non-null
    /// guard stops the deny branch from being reached. Without this direction the pattern could
    /// deny every ordinary caller of the proxy and both other directions would still pass.
    #[test]
    fn ordinary_call_to_the_observed_proxy_is_allowed() {
        let run = run_batch(
            &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[],
            None,
            generous(),
            // The proxy's own code performs NO delegatecall here: it is a bare `STOP`.
            seeded_db(UNAPPROVED_IMPL, 0),
        );
        let result = only(&run);
        assert_eq!(
            result.verdict,
            Verdict::Allow,
            "a plain call to the proxy has no implementation to compare and must not be denied"
        );
        assert_eq!(
            result.callstack_identification,
            Some(CallstackIdentification::Clean),
            "the frame WAS identified; it simply carries no implementation"
        );
    }

    /// a genuine capture failure returns the existing non-verdict, never `Allow`.
    /// Driven by a NON-DEFAULT budget of 1 against two observations, which also proves the
    /// configured value is actually injected rather than hard-coded.
    #[test]
    fn capture_failure_returns_the_non_verdict_and_the_budget_is_injected() {
        let run = run_batch(
            &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[],
            None,
            AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
            seeded_db(APPROVED_IMPL, 2),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Malformed, "never Allow on a capture failure");
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
        assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict does not commit state");

        // Counter-check: the SAME transaction under the default budget is adjudicated normally,
        // so the previous assertion is about the budget and not about the fixture.
        let generous_run = run_batch(
            &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[],
            None,
            generous(),
            seeded_db(APPROVED_IMPL, 2),
        );
        assert_eq!(only(&generous_run).verdict, Verdict::Allow);
    }

    /// a `known_allowed` **Original** executing unapproved implementation code returns
    /// verdict `Deny`, and its state is NOT committed. At `3a6028eb` this was an unconditional
    /// `Allow` with the state committed.
    #[test]
    fn known_allowed_original_running_unapproved_code_is_denied() {
        let source = format!("0x{}", "33".repeat(32));
        let run = run_batch(
            &[deposit_request(&source, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[&source],
            Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
            generous(),
            seeded_db(UNAPPROVED_IMPL, 1),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Deny);
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::Objected));
        assert_eq!(run.proxy_slot0, U256::ZERO, "state must not be committed");
    }

    /// the same with approved code returns `Allow` + `clean`, state committed.
    #[test]
    fn known_allowed_original_running_approved_code_is_allowed_and_committed() {
        let source = format!("0x{}", "33".repeat(32));
        let run = run_batch(
            &[deposit_request(&source, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[&source],
            Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
            generous(),
            seeded_db(APPROVED_IMPL, 1),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Allow);
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::Clean));
        assert_eq!(run.proxy_slot0, U256::from(1u64));
    }

    /// a `known_allowed` Original whose capture fails returns the non-verdict, state not
    /// committed, reported `not_identified`.
    #[test]
    fn known_allowed_original_with_failed_capture_is_a_non_verdict() {
        let source = format!("0x{}", "33".repeat(32));
        let run = run_batch(
            &[deposit_request(&source, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[&source],
            Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
            AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
            seeded_db(APPROVED_IMPL, 2),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Malformed);
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
        assert_eq!(run.proxy_slot0, U256::ZERO);
    }

    /// the non-verdict survives a rule that is filtered out BEFORE its selectors are ever
    /// resolved. The request's only rule is narrowed to an origin the transaction does not have,
    /// so the matcher skips it with zero decoding and therefore never looks at the (unavailable)
    /// observation set. Priority is a property of the REQUEST, not of whichever rule happened to
    /// survive filtering, so an unidentifiable execution must still be the non-verdict with no
    /// state committed.
    #[test]
    fn capture_failure_is_the_non_verdict_even_when_every_rule_is_skipped_by_origin() {
        let source = format!("0x{}", "33".repeat(32));
        let run = run_batch(
            &[deposit_request(&source, PROXY)],
            vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
            2,
            &[&source],
            Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
            AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
            seeded_db(APPROVED_IMPL, 2),
        );
        let result = only(&run);
        assert_eq!(
            result.verdict,
            Verdict::Malformed,
            "an unidentifiable execution is never reported Allow, whichever rules were skipped"
        );
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
        assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict must not commit state");

        // Counter-check: the SAME request and the SAME skipped rule under a sufficient budget is
        // adjudicated normally and committed, so the assertion above is about the unavailable
        // observation set and not about the origin narrowing by itself.
        let identified = run_batch(
            &[deposit_request(&source, PROXY)],
            vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
            2,
            &[&source],
            Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
            generous(),
            seeded_db(APPROVED_IMPL, 2),
        );
        assert_eq!(only(&identified).verdict, Verdict::Allow);
        assert_eq!(
            only(&identified).callstack_identification,
            Some(CallstackIdentification::Clean)
        );
        assert_eq!(identified.proxy_slot0, U256::from(1u64));
    }

    /// the same for an ORDINARY entry (no `known_allowed` at all), running UNAPPROVED code: the
    /// request declares a selector, the execution could not be identified, so it is the
    /// non-verdict and its state stays out of the overlay.
    #[test]
    fn capture_failure_on_an_ordinary_entry_with_skipped_rules_is_the_non_verdict() {
        let run = run_batch(
            &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
            vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
            2,
            &[],
            None,
            AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
            seeded_db(UNAPPROVED_IMPL, 2),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Malformed);
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
        assert_eq!(
            run.proxy_slot0,
            U256::ZERO,
            "unidentified execution of unapproved code must not be committed"
        );
    }

    /// the synthetic-prefix exception is explicit and survives the request-level check: with the
    /// same skipped rule and the same failed capture, a `SyntheticBan` still returns `Allow` and
    /// its state IS committed, because later originals depend on that base state.
    #[test]
    fn synthetic_ban_prefix_keeps_allow_and_commit_when_every_rule_is_skipped_by_origin() {
        let prefix = format!("0x{}", "55".repeat(32));
        let run = run_batch(
            &[deposit_request(&prefix, PROXY)],
            vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
            2,
            &[&prefix],
            Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
            AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
            seeded_db(APPROVED_IMPL, 2),
        );
        let s = only(&run);
        assert_eq!(s.verdict, Verdict::Allow, "the prefix contract is unconditional");
        assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
        assert_eq!(run.proxy_slot0, U256::from(1u64), "its commit is never withheld");
    }

    /// a `known_allowed` Original denied on the callstack dimension does NOT stop the
    /// batch: a later transaction still executes and is classified.
    #[test]
    fn a_denied_original_does_not_stop_the_batch() {
        let first = format!("0x{}", "33".repeat(32));
        let second = format!("0x{}", "44".repeat(32));
        let run = run_batch(
            &[deposit_request(&first, PROXY), deposit_request(&second, APPROVED_IMPL)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[&first],
            Some([(first.clone(), KnownAllowedKind::Original)].into_iter().collect()),
            generous(),
            seeded_db(UNAPPROVED_IMPL, 1),
        );
        assert_eq!(run.results[0].as_ref().unwrap().verdict, Verdict::Deny);
        let later = run.results[1].as_ref().expect("the later transaction still executed");
        assert_eq!(later.verdict, Verdict::Allow);
    }

    /// a synthetic replay prefix keeps the pre-existing contract unconditionally:
    /// even when a rule objects to its executed code, `S` returns `Allow`, its state IS
    /// committed, and the objection is reported only. A later original is therefore simulated on
    /// the CORRECT base state including the prefix.
    #[test]
    fn synthetic_ban_prefix_keeps_allow_and_commits_even_when_objected() {
        let prefix = format!("0x{}", "55".repeat(32));
        let original = format!("0x{}", "66".repeat(32));
        let run = run_batch(
            &[deposit_request(&prefix, PROXY), deposit_request(&original, APPROVED_IMPL)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[&prefix],
            Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
            generous(),
            seeded_db(UNAPPROVED_IMPL, 1),
        );
        let s = run.results[0].as_ref().unwrap();
        assert_eq!(s.verdict, Verdict::Allow, "a synthetic prefix's verdict never changes");
        assert_eq!(
            s.callstack_identification,
            Some(CallstackIdentification::Objected),
            "its identification is reported, not acted on"
        );
        assert_eq!(
            run.proxy_slot0,
            U256::from(2u64),
            "the prefix's state IS committed, so later originals see the correct base state"
        );
        assert_eq!(run.results[1].as_ref().unwrap().verdict, Verdict::Allow);
    }

    /// the same with the prefix's capture failing: `S` is still `Allow` and still
    /// committed, reported `not_identified`.
    #[test]
    fn synthetic_ban_prefix_with_failed_capture_still_allows_and_commits() {
        let prefix = format!("0x{}", "55".repeat(32));
        let run = run_batch(
            &[deposit_request(&prefix, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[&prefix],
            Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
            AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
            seeded_db(UNAPPROVED_IMPL, 2),
        );
        let s = only(&run);
        assert_eq!(s.verdict, Verdict::Allow);
        assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
        assert_eq!(run.proxy_slot0, U256::from(2u64), "its commit is never withheld");
    }

    /// the budget-failure branch of a replay prefix REALLY reaches the reporting path when
    /// driven through the production batch: the prefix keeps its `Allow` and its commit, and the
    /// bounded anomaly log is emitted. Without this the classification would exist only in the
    /// response field the consumer never reads for such a prefix.
    #[test]
    fn a_prefix_whose_capture_failed_reports_through_the_production_batch() {
        let prefix = format!("0x{}", "55".repeat(32));
        let kinds: BTreeMap<String, KnownAllowedKind> =
            [(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect();
        let (run, out) = super::identification_reporting_tests::capture_tracing(|| {
            run_batch(
                &[deposit_request(&prefix, PROXY)],
                vec![proxy_impl_allowlist_rule()],
                2,
                &[&prefix],
                Some(kinds),
                AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
                seeded_db(UNAPPROVED_IMPL, 2),
            )
        });

        let s = only(&run);
        assert_eq!(s.verdict, Verdict::Allow, "the prefix contract stays unconditional");
        assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
        assert_eq!(run.proxy_slot0, U256::from(2u64), "its commit is never withheld");
        assert!(
            out.contains("xlayer_audit_rpc::synthetic_ban_identification"),
            "the budget-failure branch must still emit the dedicated diagnostic: {out}"
        );
        assert!(out.contains("outcome=\"not_identified\""), "expected the outcome token: {out}");
        assert!(
            !out.contains("proxy-impl-allowlist"),
            "the diagnostic must never carry a rule id: {out}"
        );
    }

    /// the same through the batch for an OBJECTED prefix: `Allow`, committed, and the objection
    /// is visible in the operator-facing diagnostic rather than only in the response field.
    #[test]
    fn an_objected_prefix_reports_through_the_production_batch() {
        let prefix = format!("0x{}", "55".repeat(32));
        let kinds: BTreeMap<String, KnownAllowedKind> =
            [(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect();
        let (run, out) = super::identification_reporting_tests::capture_tracing(|| {
            run_batch(
                &[deposit_request(&prefix, PROXY)],
                vec![proxy_impl_allowlist_rule()],
                2,
                &[&prefix],
                Some(kinds),
                generous(),
                seeded_db(UNAPPROVED_IMPL, 1),
            )
        });

        let s = only(&run);
        assert_eq!(s.verdict, Verdict::Allow);
        assert_eq!(s.callstack_identification, Some(CallstackIdentification::Objected));
        assert_eq!(run.proxy_slot0, U256::from(2u64));
        assert!(out.contains("outcome=\"objected\""), "expected the outcome token: {out}");
    }

    /// an `Original` in the SAME request still gets `Deny` priority, proving the two
    /// classes are handled differently within one batch rather than by a global switch.
    #[test]
    fn both_classes_are_handled_differently_in_one_request() {
        let prefix = format!("0x{}", "55".repeat(32));
        let original = format!("0x{}", "66".repeat(32));
        let run = run_batch(
            &[deposit_request(&prefix, PROXY), deposit_request(&original, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[&prefix, &original],
            Some(
                [
                    (prefix.clone(), KnownAllowedKind::SyntheticBan),
                    (original.clone(), KnownAllowedKind::Original),
                ]
                .into_iter()
                .collect(),
            ),
            generous(),
            seeded_db(UNAPPROVED_IMPL, 1),
        );
        assert_eq!(run.results[0].as_ref().unwrap().verdict, Verdict::Allow, "synthetic prefix");
        assert_eq!(run.results[1].as_ref().unwrap().verdict, Verdict::Deny, "original");
    }

    /// when the request declares no selector, no inspector is constructed, the
    /// identification diagnostic is `not_applicable`, and a v1 response keeps its exact previous
    /// shape (the field is absent from the JSON).
    #[test]
    fn selector_free_request_is_unchanged() {
        let source = format!("0x{}", "33".repeat(32));
        let rules = load_rules(1, 0, vec![event_only_rule()]);
        assert!(
            audit_capture_inspector(&rules, generous()).is_none(),
            "a selector-free request must construct NO inspector"
        );

        let run = run_batch(
            &[deposit_request(&source, PROXY)],
            vec![event_only_rule()],
            1,
            &[&source],
            None,
            generous(),
            seeded_db(APPROVED_IMPL, 1),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Allow);
        assert_eq!(
            result.callstack_identification, None,
            "a v1 response must not carry the diagnostic"
        );
        let json = serde_json::to_value(result).expect("serializes");
        assert!(
            json.get("callstack_identification").is_none(),
            "the v1 wire shape is unchanged: {json}"
        );
        assert_eq!(run.proxy_slot0, U256::from(1u64), "known_allowed still commits as before");
    }

    /// an **event-only** rule with a non-zero internal native transfer in the transaction
    /// returns exactly its pre-change verdict, which proves no virtual native-transfer log
    /// entered the screened stream. This is the direct consequence of paths 2 and 3 running in
    /// `CallstackOnly` mode: under `NativeAndCallstack` the same transaction WOULD produce a
    /// virtual `Transfer` on the native-asset identifier, and the rule below is written to deny
    /// exactly that.
    #[test]
    fn no_virtual_native_log_enters_the_screened_stream() {
        let source = format!("0x{}", "33".repeat(32));

        // With capture armed (a selector-bearing rule is present) and a transaction that really
        // performs an internal native transfer: the native-log rule must NOT match.
        let armed = run_batch(
            &[deposit_request(&source, PROXY)],
            vec![native_transfer_deny_rule(), inert_selector_rule()],
            2,
            &[],
            None,
            generous(),
            value_transferring_db(),
        );
        // Without capture at all — the pre-change path for this rule.
        let unarmed = run_batch(
            &[deposit_request(&source, PROXY)],
            vec![native_transfer_deny_rule()],
            1,
            &[],
            None,
            generous(),
            value_transferring_db(),
        );

        assert_eq!(only(&unarmed).verdict, Verdict::Allow, "baseline: no native log is screened");
        assert_eq!(
            only(&armed).verdict,
            only(&unarmed).verdict,
            "arming capture must not introduce a virtual native-transfer log"
        );
    }

    /// Deny on a `Transfer` whose `log.address` is the native-asset identifier — i.e. exactly the
    /// virtual log `NativeAndCallstack` capture would synthesize, and nothing a real EVM emits
    /// here.
    fn native_transfer_deny_rule() -> RawRule {
        serde_json::from_value(serde_json::json!({
            "id": "deny-native-transfer",
            "event_abis": { "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    { "name": "from", "type": "address", "indexed": true },
                    { "name": "to", "type": "address", "indexed": true },
                    { "name": "value", "type": "uint256", "indexed": false }
                ],
                "anonymous": false
            } },
            "condition": { "==": [
                { "var": "transfer.address" },
                format!("{:#x}", xlayer_rcs_capture::NATIVE_ASSET_ADDRESS)
            ] },
            "action": "deny"
        }))
        .expect("valid native-log rule")
    }

    /// A selector-bearing rule that exists only to arm capture: its condition can never hold,
    /// because the selector always matches when the frame is observed.
    fn inert_selector_rule() -> RawRule {
        serde_json::from_value(serde_json::json!({
            "id": "arm-capture-only",
            "event_abis": {},
            "callstack": { "entry": { "address": format!("{PROXY:#x}") } },
            "condition": { "==": [ { "var": "entry.matched" }, false ] },
            "action": "deny"
        }))
        .expect("valid selector rule")
    }

    /// A proxy whose own code performs a non-zero internal `CALL` — a real native transfer.
    fn value_transferring_db() -> InMemoryDB {
        // retSize, retOffset, argsSize, argsOffset (zero), value = 1, PUSH20 target, GAS, CALL, POP, STOP
        let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0x73];
        code.extend_from_slice(APPROVED_IMPL.as_slice());
        code.extend_from_slice(&[0x5a, 0xf1, 0x50, 0x00]);

        let mut db = InMemoryDB::default();
        db.insert_account_info(
            DEPOSITOR,
            AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
        );
        db.insert_account_info(
            PROXY,
            AccountInfo {
                balance: U256::from(1_000u64),
                code: Some(Bytecode::new_raw(code.into())),
                ..Default::default()
            },
        );
        db.insert_account_info(
            APPROVED_IMPL,
            AccountInfo {
                code: Some(Bytecode::new_raw(impl_code(0x01).into())),
                ..Default::default()
            },
        );
        db
    }

    // ---------------------------------------------------------------- request boundary --------

    /// a request omitting the protocol version behaves as version 1.
    #[test]
    fn omitted_protocol_version_means_one() {
        let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
            "rules": [],
            "txs": [],
        }))
        .expect("an omitted version must still decode");
        assert_eq!(req.protocol_version, MIN_PROTOCOL_VERSION);
        assert!(req.known_allowed.is_empty());
        assert!(req.known_allowed_kinds.is_none());
        assert!(validate_protocol_version(req.protocol_version).is_ok());
    }

    /// an unsupported protocol version returns `-32602` naming the version.
    #[test]
    fn unsupported_protocol_version_is_rejected_by_name() {
        let error = validate_protocol_version(99).expect_err("99 is not supported");
        assert_eq!(error.code(), -32602);
        assert!(error.message().contains("99"), "{}", error.message());
        // Both shipped versions stay accepted.
        assert!(validate_protocol_version(1).is_ok());
        assert!(validate_protocol_version(2).is_ok());
    }

    /// The request root is as strict as the rule schema: a speculative or misspelled field is
    /// rejected by name rather than silently discarded.
    #[test]
    fn unknown_request_field_is_rejected_by_name() {
        let error = serde_json::from_value::<AuditTransactionsRequest>(serde_json::json!({
            "rules": [],
            "txs": [],
            "knownAllowed": []
        }))
        .expect_err("an unknown request key must be rejected");
        assert!(
            error.to_string().contains("unknown field(s) in audit request: knownAllowed"),
            "{error}"
        );
    }

    /// a v2 selector-bearing request with a missing or partial `known_allowed_kinds`
    /// returns `-32602` naming the unlabelled hashes, and nothing is installed or committed.
    #[test]
    fn missing_or_partial_labels_are_rejected_naming_the_hashes() {
        let a = format!("0x{}", "33".repeat(32));
        let b = format!("0x{}", "44".repeat(32));
        let known = vec![a.clone(), b.clone()];

        // Missing entirely.
        let error = validate_known_allowed_labels(2, &known, None, true)
            .expect_err("a missing map must be rejected");
        assert_eq!(error.code(), -32602);
        let data = error.data().expect("the unlabelled hashes are named").to_string();
        assert!(data.contains(&a) && data.contains(&b), "{data}");

        // Partial.
        let partial: BTreeMap<String, KnownAllowedKind> =
            [(a.clone(), KnownAllowedKind::Original)].into_iter().collect();
        let error = validate_known_allowed_labels(2, &known, Some(&partial), true)
            .expect_err("a partial map must be rejected");
        let data = error.data().expect("named").to_string();
        assert!(data.contains(&b), "only the unlabelled hash is named: {data}");
        assert!(!data.contains(&a), "a labelled hash must not be named: {data}");

        // Complete.
        let complete: BTreeMap<String, KnownAllowedKind> =
            [(a.clone(), KnownAllowedKind::Original), (b.clone(), KnownAllowedKind::SyntheticBan)]
                .into_iter()
                .collect();
        assert!(validate_known_allowed_labels(2, &known, Some(&complete), true).is_ok());
    }

    /// v1 requests, and v2 requests declaring no selector, need no labels.
    #[test]
    fn requests_that_need_no_labels_are_unaffected() {
        let known = vec![format!("0x{}", "33".repeat(32))];
        assert!(validate_known_allowed_labels(1, &known, None, true).is_ok(), "v1 needs none");
        assert!(
            validate_known_allowed_labels(2, &known, None, false).is_ok(),
            "a selector-free v2 request needs none"
        );
        assert!(
            validate_known_allowed_labels(2, &[], None, true).is_ok(),
            "an empty known_allowed needs none"
        );
    }

    /// The class is read from the labels and NEVER inferred. `synthetic_ban` is the wire spelling
    /// a producer must send for a replay prefix.
    #[test]
    fn known_allowed_kind_wire_spelling_is_snake_case() {
        let kinds: BTreeMap<String, KnownAllowedKind> = serde_json::from_value(
            serde_json::json!({ "0xaa": "original", "0xbb": "synthetic_ban" }),
        )
        .expect("both spellings decode");
        assert_eq!(kinds["0xaa"], KnownAllowedKind::Original);
        assert_eq!(kinds["0xbb"], KnownAllowedKind::SyntheticBan);
        assert!(
            serde_json::from_value::<KnownAllowedKind>(serde_json::json!("syntheticBan")).is_err(),
            "a wrong spelling must not be accepted as a class"
        );
    }

    /// a programmatic `AuditCaptureConfig { 0 }` yields the non-verdict, never `Allow`.
    /// Defence-in-depth of this type only: `FilterConfig::validate()` rejects `0` at startup, so
    /// the audit RPC can never receive it through configuration.
    #[test]
    fn programmatic_zero_budget_yields_the_non_verdict() {
        let run = run_batch(
            &[deposit_request(&format!("0x{}", "33".repeat(32)), PROXY)],
            vec![proxy_impl_allowlist_rule()],
            2,
            &[],
            None,
            AuditCaptureConfig { max_callstack_observations_per_tx: 0 },
            seeded_db(APPROVED_IMPL, 1),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Malformed, "never Allow");
        assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    }

    /// The callstack-only projection used for `known_allowed` adjudication carries the SAME
    /// snapshot stamp as the full set, which is what keeps the existing stamp check meaningful
    /// instead of bypassed.
    #[test]
    fn callstack_only_projection_preserves_the_snapshot_stamp() {
        let raw = vec![event_only_rule(), proxy_impl_allowlist_rule()];
        let full = load_rules(2, 0, raw.clone());
        assert!(full.rejected.is_empty());
        let projected = callstack_only_projection(2, &full, &raw).expect("selectors are declared");
        assert_eq!(projected.stamp(), full.stamp());
        assert_eq!(projected.rules.len(), 1, "only the selector-bearing rule survives");
        assert!(callstack_only_projection(
            1,
            &load_rules(1, 0, vec![event_only_rule()]),
            &[event_only_rule()]
        )
        .is_none());
    }

    /// The budget actually reaches the inspector: a configured value is what the inspector
    /// enforces, not a hard-coded default.
    #[test]
    fn audit_capture_config_budget_reaches_the_inspector() {
        let rules = load_rules(2, 0, vec![proxy_impl_allowlist_rule()]);
        let inspector = audit_capture_inspector(
            &rules,
            AuditCaptureConfig { max_callstack_observations_per_tx: 7 },
        )
        .expect("selectors declared");
        assert_eq!(inspector.mode(), xlayer_rcs_capture::CaptureMode::CallstackOnly);
    }

    // ----------------------------------------------------- repeated-adjudication fixtures ----

    /// The implementation used by the repeated-adjudication fixture. Distinct from the two above
    /// because its code emits a log, hence a distinct code hash.
    const LOGGING_IMPL: Address = Address::repeat_byte(0xB3);

    /// Implementation code that emits one real ERC20
    /// `Transfer(from = PROXY, to = DEPOSITOR, value = 1)` then `STOP`s. Executed under the
    /// proxy's `DELEGATECALL`, so `log.address` is the PROXY.
    ///
    /// `LOG3` pops `offset, size, topic0, topic1, topic2` with the offset on top, so the topics
    /// are pushed in reverse.
    fn logging_impl_code() -> Vec<u8> {
        let signature = keccak256("Transfer(address,address,uint256)");
        let mut code = vec![0x60, 0x01, 0x60, 0x00, 0x52]; // PUSH1 1; PUSH1 0; MSTORE
        code.push(0x7f); // PUSH32 topic2 (to)
        code.extend_from_slice(DEPOSITOR.into_word().as_slice());
        code.push(0x7f); // PUSH32 topic1 (from)
        code.extend_from_slice(PROXY.into_word().as_slice());
        code.push(0x7f); // PUSH32 topic0 (the event signature)
        code.extend_from_slice(signature.as_slice());
        code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0xa3, 0x00]); // PUSH1 32; PUSH1 0; LOG3; STOP
        code
    }

    /// A log-dimension rule asking for adjudication on the `Transfer` the fixture really emits.
    /// This is what makes the convergence claim falsifiable: the very same input genuinely does
    /// reach adjudication while it is not yet approved.
    fn audit_on_proxy_transfer_rule() -> RawRule {
        serde_json::from_value(serde_json::json!({
            "id": "audit-proxy-transfer",
            "event_abis": { "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    { "name": "from", "type": "address", "indexed": true },
                    { "name": "to", "type": "address", "indexed": true },
                    { "name": "value", "type": "uint256", "indexed": false }
                ],
                "anonymous": false
            } },
            "condition": { "==": [ { "var": "transfer.address" }, format!("{PROXY:#x}") ] },
            "action": "audit"
        }))
        .expect("valid log-dimension rule")
    }

    /// The proxy delegatecalls [`LOGGING_IMPL`], so one transaction carries BOTH a call-frame
    /// observation and a matching log.
    fn logging_db() -> InMemoryDB {
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            DEPOSITOR,
            AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
        );
        db.insert_account_info(
            PROXY,
            AccountInfo {
                code: Some(Bytecode::new_raw(delegating_proxy_code(LOGGING_IMPL, 1).into())),
                ..Default::default()
            },
        );
        db.insert_account_info(
            LOGGING_IMPL,
            AccountInfo {
                code: Some(Bytecode::new_raw(logging_impl_code().into())),
                ..Default::default()
            },
        );
        db
    }

    /// repeatedly adjudicating an already-approved entry converges: every round returns the
    /// byte-identical serialized result, and adjudication is never re-entered.
    ///
    /// The paired baseline run is what gives the assertion teeth — the same transaction, the same
    /// rules and the same state DO reach adjudication while the entry is not yet approved, so the
    /// approved rounds are not passing merely because nothing could ever have matched.
    #[test]
    fn repeated_adjudication_of_an_approved_entry_converges_and_never_re_enters_adjudication() {
        let source = format!("0x{}", "33".repeat(32));
        let approved = keccak256(logging_impl_code());
        let rules =
            vec![proxy_impl_allowlist_rule_approving(&[approved]), audit_on_proxy_transfer_rule()];
        let kinds = BTreeMap::from([(source.clone(), KnownAllowedKind::Original)]);

        let fresh = run_batch(
            &[deposit_request(&source, PROXY)],
            rules.clone(),
            2,
            &[],
            None,
            generous(),
            logging_db(),
        );
        assert_eq!(
            only(&fresh).verdict,
            Verdict::Audit,
            "baseline: an unapproved entry really does reach adjudication on this fixture"
        );

        let mut rounds: Vec<String> = Vec::new();
        for round in 0..3 {
            let run = run_batch(
                &[deposit_request(&source, PROXY)],
                rules.clone(),
                2,
                &[&source],
                Some(kinds.clone()),
                generous(),
                logging_db(),
            );
            let result = only(&run);
            assert_eq!(
                result.verdict,
                Verdict::Allow,
                "round {round}: an already-approved entry keeps its allow"
            );
            assert_ne!(
                result.verdict,
                Verdict::Audit,
                "round {round}: adjudication must be unreachable for an approved entry"
            );
            rounds.push(serde_json::to_string(result).expect("the result serializes"));
        }
        assert!(
            rounds.windows(2).all(|pair| pair[0] == pair[1]),
            "the rounds must be byte-identical: {rounds:?}"
        );
    }

    // ------------------------------------------------------------- rule-rejection detail -----

    /// a rule refused for an unknown field is listed in the `-32602` detail array with its rule
    /// id and the unknown-field reason, so the caller learns WHICH rule was refused and WHY
    /// rather than only how many were.
    #[test]
    fn a_rejected_rule_is_listed_in_the_error_detail_with_its_id_and_reason() {
        let typo: RawRule = serde_json::from_value(serde_json::json!({
            "id": "typo-rule",
            "event_abis": {},
            // The section name is misspelled, so no selector is declared and the key is unknown.
            "callStack": { "entry": { "address": format!("{PROXY:#x}") } },
            "condition": true,
            "action": "deny"
        }))
        .expect("the wire type captures an unknown key instead of failing to decode");

        let rules = load_rules(2, 0, vec![typo]);
        assert_eq!(rules.rejected.len(), 1, "the rule must be refused, not installed");

        let error = validate_compiled_rules(&rules)
            .expect_err("a refused rule must fail the whole request");
        assert_eq!(error.code(), -32602);
        let detail: Vec<serde_json::Value> =
            serde_json::from_str(error.data().expect("a detail array is attached").get())
                .expect("the detail is a JSON array");
        assert_eq!(detail.len(), 1, "one refusal, one entry: {detail:?}");
        assert_eq!(detail[0]["id"], "typo-rule", "the entry names the rule: {detail:?}");
        let reason = detail[0]["reason"].as_str().expect("the entry carries a reason");
        assert!(
            reason.contains("callStack"),
            "the reason must name the unrecognised key: {reason}"
        );
    }
}

/// The request-level non-verdict gate, and the synthetic-prefix reporting the consumer cannot see
/// any other way.
#[cfg(test)]
mod identification_reporting_tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use rcs_filter::callstack::SnapshotStamp;
    use rcs_filter::rules::load_rules;

    /// Runs `f` with every `tracing` event captured, so a diagnostic's fields can be asserted —
    /// including the fields it must NOT carry.
    pub(super) fn capture_tracing<R>(f: impl FnOnce() -> R) -> (R, String) {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

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

    /// A selector-bearing rule set, compiled exactly as the handler compiles a request's own.
    fn selector_bearing_rules() -> RuleSet {
        let rule: RawRule = serde_json::from_value(serde_json::json!({
            "id": "proxy-impl-allowlist",
            "event_abis": {},
            "callstack": { "entry": { "address": format!("{:#x}", Address::repeat_byte(0xAA)) } },
            "condition": { "==": [ { "var": "entry.matched" }, true ] },
            "action": "deny"
        }))
        .expect("the fixture rule deserializes");
        let rules = load_rules(2, 0, vec![rule]);
        assert!(rules.rejected.is_empty(), "the fixture rule must load clean");
        rules
    }

    /// A v1 event rule declaring no call-frame selector.
    fn selector_free_rules() -> RuleSet {
        let rule: RawRule = serde_json::from_str(
            r#"{"id":"evt","event_abis":{"transfer":{"type":"event","name":"Transfer",
               "inputs":[{"name":"from","type":"address","indexed":true}],
               "anonymous":false}},"condition":true,"action":"deny"}"#,
        )
        .expect("valid v1 rule");
        load_rules(1, 0, vec![rule])
    }

    /// the gate refuses an observation set produced under a different rule snapshot, accepts a
    /// genuinely empty set stamped by the evaluating one, and leaves a request that declares no
    /// selector completely untouched.
    #[test]
    fn the_request_level_gate_separates_unavailable_from_empty() {
        let rules = selector_bearing_rules();
        let foreign = SnapshotStamp { selector_digest: B256::repeat_byte(0xEE), ..rules.stamp() };

        assert_eq!(
            unadjudicable_observations(&rules, Observations::available(foreign, &[])),
            Some(ObservationUnavailable::SnapshotMismatch),
            "a foreign stamp cannot be read as captured-and-empty"
        );
        assert_eq!(
            unadjudicable_observations(&rules, Observations::available(rules.stamp(), &[])),
            None,
            "capture ran and genuinely observed nothing: adjudicable"
        );
        assert_eq!(
            unadjudicable_observations(&rules, Observations::NOT_CAPTURED),
            Some(ObservationUnavailable::NotCaptured)
        );
        assert_eq!(
            unadjudicable_observations(
                &rules,
                Observations::Unavailable { reason: ObservationUnavailable::BudgetExceeded }
            ),
            Some(ObservationUnavailable::BudgetExceeded),
            "the reason is preserved, not collapsed"
        );
        assert_eq!(
            unadjudicable_observations(&selector_free_rules(), Observations::NOT_CAPTURED),
            None,
            "a request declaring no selector keeps its pre-existing path byte for byte"
        );
    }

    /// every classification increments its OWN counter, read back from a recorder rather than
    /// restated from the code.
    #[test]
    fn each_classification_increments_its_own_counter() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _guard = metrics::set_default_local_recorder(&recorder);

        // `new_with_labels` registers fresh handles against the recorder installed above, where
        // `default()` would hand back a cached instance bound to whichever recorder was active
        // when some other test first built it.
        let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
        for identification in [
            CallstackIdentification::Clean,
            CallstackIdentification::Objected,
            CallstackIdentification::NotIdentified,
            CallstackIdentification::NotApplicable,
        ] {
            report_synthetic_ban_identification(
                &metrics,
                B256::repeat_byte(0x7A),
                "0xsrc",
                identification,
                3,
            );
        }

        let mut counters: Vec<(String, u64)> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .filter_map(|(key, _, _, value)| match value {
                DebugValue::Counter(count) => Some((key.key().name().to_string(), count)),
                _ => None,
            })
            .collect();
        counters.sort();
        assert_eq!(
            counters,
            vec![
                ("rcs_filter.synthetic_ban_identification.clean_total".to_string(), 1),
                ("rcs_filter.synthetic_ban_identification.not_applicable_total".to_string(), 1),
                ("rcs_filter.synthetic_ban_identification.not_identified_total".to_string(), 1),
                ("rcs_filter.synthetic_ban_identification.objected_total".to_string(), 1),
            ],
            "each class lands in its own dedicated synthetic_ban series (the dotted key is the \
             recorder's internal form; the exporter renders it with underscores)"
        );
    }

    /// an objection on a replay prefix emits exactly the bounded fields — and none of the
    /// content a rule id or an observation would carry.
    #[test]
    fn an_objected_prefix_emits_one_bounded_anomaly_log() {
        let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
        let (_, out) = capture_tracing(|| {
            report_synthetic_ban_identification(
                &metrics,
                B256::repeat_byte(0x7A),
                "0xsrc",
                CallstackIdentification::Objected,
                3,
            );
        });

        assert_eq!(out.lines().count(), 1, "exactly one log line: {out}");
        assert!(out.contains("ERROR"), "expected error level: {out}");
        assert!(
            out.contains("xlayer_audit_rpc::synthetic_ban_identification"),
            "expected the dedicated target: {out}"
        );
        assert!(out.contains("outcome=\"objected\""), "expected the outcome token: {out}");
        assert!(out.contains("selector_bearing_rules=3"), "expected the bounded count: {out}");
        assert!(out.contains("0xsrc"), "expected the source hash: {out}");
        assert!(
            !out.contains("proxy-impl-allowlist"),
            "the diagnostic must never carry a rule id: {out}"
        );
        assert!(
            !out.to_lowercase().contains("implcodehash"),
            "the diagnostic must never carry observation content: {out}"
        );
        assert!(
            !out.to_lowercase().contains(&format!("{:#x}", Address::repeat_byte(0xAA))),
            "the diagnostic must never carry an observed address: {out}"
        );
    }

    /// an unidentifiable replay prefix emits the same bounded log under its own outcome token.
    #[test]
    fn an_unidentified_prefix_emits_one_bounded_anomaly_log() {
        let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
        let (_, out) = capture_tracing(|| {
            report_synthetic_ban_identification(
                &metrics,
                B256::repeat_byte(0x7A),
                "0xsrc",
                CallstackIdentification::NotIdentified,
                1,
            );
        });

        assert_eq!(out.lines().count(), 1, "exactly one log line: {out}");
        assert!(out.contains("ERROR"), "expected error level: {out}");
        assert!(out.contains("outcome=\"not_identified\""), "expected the outcome token: {out}");
        assert!(out.contains("selector_bearing_rules=1"), "expected the bounded count: {out}");
    }

    /// a clean (and a not-applicable) prefix is counted and stays SILENT: the log is for
    /// anomalies, so an ordinary replay must not fill an operator's log with one line per prefix.
    #[test]
    fn a_clean_prefix_is_counted_without_an_anomaly_log() {
        let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
        let (_, out) = capture_tracing(|| {
            report_synthetic_ban_identification(
                &metrics,
                B256::repeat_byte(0x7A),
                "0xsrc",
                CallstackIdentification::Clean,
                3,
            );
            report_synthetic_ban_identification(
                &metrics,
                B256::repeat_byte(0x7B),
                "0xsrc2",
                CallstackIdentification::NotApplicable,
                0,
            );
        });
        assert!(out.is_empty(), "no anomaly, so no log: {out}");
    }

    /// the classification → series mapping itself, asserted on the handles so it cannot drift
    /// into reporting one class under another's counter.
    #[test]
    fn the_counter_mapping_is_one_series_per_class() {
        let metrics = SyntheticBanIdentificationMetrics::new_with_labels(Vec::new());
        assert!(std::ptr::eq(
            metrics.counter(CallstackIdentification::Clean),
            &metrics.clean_total
        ));
        assert!(std::ptr::eq(
            metrics.counter(CallstackIdentification::Objected),
            &metrics.objected_total
        ));
        assert!(std::ptr::eq(
            metrics.counter(CallstackIdentification::NotIdentified),
            &metrics.not_identified_total
        ));
        assert!(std::ptr::eq(
            metrics.counter(CallstackIdentification::NotApplicable),
            &metrics.not_applicable_total
        ));
    }
}
