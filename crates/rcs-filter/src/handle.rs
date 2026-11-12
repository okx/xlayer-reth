//! [`FilterHandle`] — the synchronous entry the block builder calls per transaction, plus
//! the owner of shared filter state and background workers.
//!
//! `screen_tx` performs **zero network IO**: dedup short-circuit → in-memory match/merge →
//! buffer-pool bookkeeping. All RCS traffic happens on the [`crate::worker`] tasks spawned
//! by [`FilterHandle::spawn`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use alloy_primitives::{Address, Log, B256, U256};
use tracing::error;

use crate::callstack::Observations;
use crate::client::RcsClient;
use crate::clock::Clock;
use crate::config::FilterConfig;
use crate::matching::{self, MatchOutcome};
use crate::metrics::RcsFilterMetrics;
use crate::pool::{BufferEntry, BufferPool, BufferStatus, Resolution};
use crate::quota_hash;
use crate::rules::RuleSet;

const TERMINAL_EVENT_CAPACITY: usize = 1024;

/// Terminal decision delivered to the builder-owned txpool integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalEvent {
    pub tx_hash: B256,
    pub generation: u64,
    pub reason: TerminalReason,
}

/// Why a buffered transaction became permanently rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalReason {
    Denied,
    Outdated,
    FailCloseTimeout,
}

/// The screening decision returned to the builder hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Package normally (no rule required action, or an already-approved-and-consistent tx).
    Allow,
    /// Exclude via `mark_invalid` (a `deny` rule matched).
    Deny,
    /// Permanently discard an audit transaction from the transaction pool after a terminal
    /// rejection (`denied`, `outdated`, fail-close, or consistency mismatch).
    Drop,
    /// Skip this round (do not commit, do not `mark_invalid`); tx stays in the pool for a
    /// later round while its audit adjudication proceeds.
    AuditPending,
    /// Approved and the pre-package consistency check passed → package normally.
    AuditApproved,
}

/// Cheap status-only decision made before EVM execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreScreen {
    /// Execute the transaction and run the authoritative post-execution screen.
    Execute,
    /// Keep the transaction in txpool but skip it and its nonce descendants this iteration.
    Defer,
    /// Permanently rejected: remove it from txpool without executing it.
    Drop,
}

/// Why the builder locally denied a transaction before rule evaluation, driven by its native
/// observation pipeline. A fixed, low-cardinality enum used as a metric label — never a dynamic
/// error string. This crate stays revm-free; the builder maps its capture outcomes onto this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalDenyReason {
    /// The per-transaction internal native-transfer observation budget was exceeded.
    NativeTransferLimit,
    /// An observation invariant failed: the captured real-log subsequence did not match the
    /// execution result, or an enabled filter received a passthrough capture outcome.
    ObservationInvariant,
    /// The per-transaction call-frame observation budget was exceeded during capture.
    CallstackObservationLimit,
}

/// Input to [`FilterHandle::screen_tx`], borrowed from the builder loop.
pub struct ScreenInput<'a> {
    pub tx_hash: B256,
    pub origin: Address,
    /// `tx.to` (`None` for contract creation).
    pub tx_to: Option<Address>,
    pub nonce: u64,
    /// Native ETH value of the transaction.
    pub value: U256,
    /// Simulated block height for the submit payload (`xlayer_block_height`).
    pub block_height: u64,
    /// Execution logs of the transaction (borrowed; screening does not consume them).
    pub logs: &'a [Log],
    /// Call-frame observations for this execution. A tri-state: a path that captures nothing
    /// passes [`Observations::NOT_CAPTURED`] so a rule with callstack selectors fails closed
    /// instead of reading an empty list as "observed nothing".
    pub observations: Observations<'a>,
}

/// Shared, cheaply-clonable filter state used by the handle and background workers.
#[derive(Clone)]
pub(crate) struct Shared {
    pub config: FilterConfig,
    /// Atomically hot-swapped rule snapshot (`Arc<RwLock<Arc<..>>>`, with no
    /// new dependency — hot path takes a read lock and clones the inner `Arc`).
    pub rules: Arc<RwLock<Arc<RuleSet>>>,
    pub pool: Arc<Mutex<BufferPool>>,
    pub clock: Arc<dyn Clock>,
    /// Set once the first valid rule set is loaded. Before that, screening uses empty rules.
    pub ready: Arc<AtomicBool>,
    pub terminal_events: tokio::sync::broadcast::Sender<TerminalEvent>,
    pub metrics: RcsFilterMetrics,
}

impl Shared {
    /// Current rule snapshot. Recovers from lock poisoning: a panic elsewhere while the lock
    /// was held must not permanently brick screening or the background workers (FR robustness).
    pub(crate) fn current_rules(&self) -> Arc<RuleSet> {
        self.rules.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Locks the buffer pool, recovering from poisoning.
    pub(crate) fn pool_lock(&self) -> std::sync::MutexGuard<'_, BufferPool> {
        self.pool.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Write guard for the hot-swappable rule slot, recovering from poisoning.
    pub(crate) fn rules_write(&self) -> std::sync::RwLockWriteGuard<'_, Arc<RuleSet>> {
        self.rules.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Atomically installs a supported rule snapshot and publishes its operational state.
    pub(crate) fn install_rules(&self, rules: RuleSet) {
        let content_version = rules.content_version;
        *self.rules_write() = Arc::new(rules);
        self.metrics.rules_content_version.set(content_version as f64);
        self.metrics.rules_ready.set(1.0);
        self.ready.store(true, Ordering::Release);
    }

    pub(crate) fn emit_terminal(&self, event: TerminalEvent) {
        self.metrics.terminal_discard_events_total.increment(1);
        // No active receiver is acceptable during startup/tests. The builder reconciles the
        // Dropped snapshot when it attaches or if a bounded receiver lags.
        let _ = self.terminal_events.send(event);
    }

    pub(crate) fn emit_timeout_resolutions(&self, resolved: &[(B256, u64, Resolution)]) {
        for (hash, generation, resolution) in resolved {
            tracing::debug!(target: "rcs_filter", tx_hash = %format!("{hash:#x}"), ?resolution, "cumulative timeout resolution");
            if *resolution == Resolution::Discard {
                self.emit_terminal(TerminalEvent {
                    tx_hash: *hash,
                    generation: *generation,
                    reason: TerminalReason::FailCloseTimeout,
                });
            }
        }
    }

    pub(crate) fn update_buffer_metric(&self) {
        let pool = self.pool_lock();
        let counts = pool.status_counts();
        self.metrics.buffered_transactions.set(pool.len() as f64);
        self.metrics.buffer_not_submitted.set(counts[0] as f64);
        self.metrics.buffer_submitted.set(counts[1] as f64);
        self.metrics.buffer_pending.set(counts[2] as f64);
        self.metrics.buffer_approved.set(counts[3] as f64);
        self.metrics.buffer_timed_out_allow.set(counts[4] as f64);
        self.metrics.buffer_dropped.set(counts[5] as f64);
    }
}

/// Handle exposing the synchronous screening entry and owning background workers.
pub struct FilterHandle {
    shared: Shared,
    /// Supervisor task handles for the background workers. Retained (not dropped) so the tasks
    /// are observable and are aborted when the handle is dropped (no leaked tasks). Empty for
    /// [`FilterHandle::for_test`].
    workers: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for FilterHandle {
    fn drop(&mut self) {
        for w in &self.workers {
            w.abort();
        }
    }
}

impl std::fmt::Debug for FilterHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterHandle")
            .field("enabled", &self.shared.config.enabled)
            .field("ready", &self.is_ready())
            .field("buffered", &self.buffered_len())
            .finish()
    }
}

/// Extracts the bounded diagnostic counts `(observed_count, limit)` from a matcher [`MatchError`]
/// for structured overflow logging. It NEVER returns rule ids or binding/transfer content — only
/// the numeric budget counts. A count-overflow (usize multiplication overflow) reports
/// `observed_count = usize::MAX` against the configured limit.
fn binding_overflow_counts(
    error: &matching::MatchError,
    configured_limit: usize,
) -> (usize, usize) {
    match error {
        matching::MatchError::BindingBudgetExceeded { attempted, limit, .. } => {
            (*attempted, *limit)
        }
        matching::MatchError::BindingCountOverflow { .. } => (usize::MAX, configured_limit),
        matching::MatchError::ObservationsUnavailable { .. }
        | matching::MatchError::ObservationSnapshotMismatch { .. } => (0, configured_limit),
    }
}

impl FilterHandle {
    /// Builds a handle and spawns the background workers (rule load/hot-reload, batch
    /// submit, adjudication poll, timeout tick). Must be called from within a tokio runtime.
    pub fn spawn(
        config: FilterConfig,
        client: Arc<dyn RcsClient>,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        let (terminal_events, _) = tokio::sync::broadcast::channel(TERMINAL_EVENT_CAPACITY);
        let metrics = RcsFilterMetrics::default();
        metrics.rules_ready.set(0.0);
        metrics.rules_content_version.set(0.0);
        let shared = Shared {
            config,
            rules: Arc::new(RwLock::new(Arc::new(RuleSet::default()))),
            pool: Arc::new(Mutex::new(BufferPool::default())),
            clock,
            ready: Arc::new(AtomicBool::new(false)),
            terminal_events,
            metrics,
        };
        let workers = crate::worker::spawn(shared.clone(), client);
        Arc::new(Self { shared, workers })
    }

    /// True once the first valid rule set has loaded. Until then screening uses the empty
    /// snapshot while background loading retries, so block production remains live.
    pub fn is_ready(&self) -> bool {
        self.shared.ready.load(Ordering::Acquire)
    }

    /// Number of transactions currently buffered awaiting adjudication.
    pub fn buffered_len(&self) -> usize {
        self.shared.pool_lock().len()
    }

    /// The configured maximum internal native-transfer observation candidates per transaction.
    /// This is the single source of truth for the builder's inspector budget; the builder reads it
    /// here rather than duplicating it into its own context, so the two can never drift.
    pub fn max_native_transfers_per_tx(&self) -> usize {
        self.shared.config.max_native_transfers_per_tx
    }

    /// Records a builder-side local deny that bypassed rule evaluation (native-transfer budget
    /// overflow or an observation invariant failure). Increments the total-deny metric once and the
    /// matching reason metric once. Creates no buffer-pool entry and performs no txpool removal —
    /// the shared builder `Screen::Deny` branch owns eviction.
    pub fn record_local_deny(&self, reason: LocalDenyReason) {
        self.shared.metrics.deny_total.increment(1);
        match reason {
            LocalDenyReason::NativeTransferLimit => {
                self.shared.metrics.local_deny_native_transfer_limit_total.increment(1)
            }
            LocalDenyReason::ObservationInvariant => {
                self.shared.metrics.local_deny_observation_invariant_total.increment(1)
            }
            LocalDenyReason::CallstackObservationLimit => {
                self.shared.metrics.local_deny_callstack_observation_limit_total.increment(1)
            }
        }
    }

    /// The configured maximum call-frame observations per transaction. Single source of truth for
    /// the builder's capture budget, read here rather than duplicated so the two cannot drift.
    pub fn max_callstack_observations_per_tx(&self) -> usize {
        self.shared.config.max_callstack_observations_per_tx
    }

    /// The configured per-transaction complete-binding budget. Single source of truth for every
    /// caller that drives the pure classifier itself (the deposit/sequencer identification path)
    /// rather than going through `screen_tx`, so the two cannot drift.
    pub fn max_event_bindings_per_tx(&self) -> usize {
        self.shared.config.max_event_bindings_per_tx
    }

    /// The current rule snapshot.
    ///
    /// A caller that observes call frames must acquire the snapshot **once**, configure capture
    /// from it, and then screen against the very same `Arc`. Acquiring it again after simulation
    /// could evaluate against rules whose selector set never configured the capture, which would
    /// make an unobserved frame look like an absent one.
    pub fn rule_snapshot(&self) -> Arc<RuleSet> {
        self.shared.current_rules()
    }

    /// The rule snapshot a candidate already present in the buffer pool must be re-simulated and
    /// re-screened under, or `None` when it has no entry.
    ///
    /// A buffered entry keeps the snapshot it was first evaluated against, and
    /// [`Self::screen_tx_with_snapshot`] re-evaluates an `Approved` entry against that saved
    /// snapshot rather than the one its caller pinned. A caller that also *captures* call frames
    /// must therefore configure capture from this snapshot, not from the one pinned for the block:
    /// otherwise the observation set would carry the block snapshot's stamp while the consistency
    /// recompute evaluates the saved one, and a reload that changed nothing but `content_version`
    /// would turn a legitimate re-simulation into a stamp mismatch.
    pub fn saved_rule_snapshot(&self, tx_hash: &B256) -> Option<Arc<RuleSet>> {
        self.shared.pool_lock().get(tx_hash).map(|entry| Arc::clone(&entry.rule_snapshot))
    }

    /// Screens one transaction (synchronous, no network IO) against the current rule snapshot.
    /// See [`Screen`].
    ///
    /// Callers that captured call-frame observations must use
    /// [`Self::screen_tx_with_snapshot`] with the snapshot that configured the capture.
    pub fn screen_tx(&self, input: &ScreenInput) -> Screen {
        self.screen_tx_with_snapshot(input, &self.shared.current_rules())
    }

    /// Screens one transaction against an explicitly pinned rule snapshot.
    pub fn screen_tx_with_snapshot(&self, input: &ScreenInput, pinned: &Arc<RuleSet>) -> Screen {
        // Stage zero: dedup short-circuit — a non-terminal buffered entry is reused without
        // re-decoding or re-submitting during the first screening stage.
        let existing = {
            let pool = self.shared.pool_lock();
            pool.get(&input.tx_hash).map(|e| {
                (e.status, e.quota_consistency_hash, e.generation, Arc::clone(&e.rule_snapshot))
            })
        };
        if let Some((status, stored_hash, generation, rule_snapshot)) = existing {
            return self.screen_existing(input, status, stored_hash, generation, rule_snapshot);
        }

        // Fresh evaluation against the pinned snapshot.
        let rules = Arc::clone(pinned);
        let outcome = match matching::try_evaluate(
            &rules,
            input,
            self.shared.config.max_event_bindings_per_tx,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.record_match_failure(input.tx_hash, &error);
                return Screen::Deny;
            }
        };
        match outcome {
            MatchOutcome::Allow => {
                let concurrent = {
                    let pool = self.shared.pool_lock();
                    pool.get(&input.tx_hash).map(|entry| {
                        (
                            entry.status,
                            entry.quota_consistency_hash,
                            entry.generation,
                            Arc::clone(&entry.rule_snapshot),
                        )
                    })
                };
                if let Some((status, stored_hash, generation, rule_snapshot)) = concurrent {
                    return self.screen_existing(
                        input,
                        status,
                        stored_hash,
                        generation,
                        rule_snapshot,
                    );
                }
                self.shared.metrics.allow_total.increment(1);
                Screen::Allow
            }
            MatchOutcome::Deny => {
                self.shared.metrics.deny_total.increment(1);
                Screen::Deny
            }
            MatchOutcome::Audit { actions, timeout_action } => {
                let now = self.shared.clock.now_unix();
                let quota_consistency_hash = quota_hash::encode_and_hash(&actions);
                let entry = BufferEntry {
                    generation: 0,
                    tx_hash: input.tx_hash,
                    origin: input.origin,
                    contract_address: input.tx_to.unwrap_or(Address::ZERO),
                    nonce: input.nonce,
                    block_height: input.block_height,
                    status: BufferStatus::NotSubmitted,
                    actions,
                    rule_snapshot: rules,
                    quota_consistency_hash,
                    timeout_action,
                    first_not_submitted_at: now,
                    last_transition_at: now,
                };
                self.shared.pool_lock().insert(entry);
                self.shared.metrics.audit_pending_total.increment(1);
                self.shared.update_buffer_metric();
                Screen::AuditPending
            }
        }
    }

    /// Emits bounded diagnostics for a screening failure and counts the deny.
    ///
    /// The four fail-closed causes are operationally different incidents and are deliberately not
    /// collapsed into one log line or one counter: a matcher-budget overflow is a capacity signal,
    /// an unavailable observation set means the execution path cannot adjudicate callstack rules
    /// at all, and a snapshot-stamp mismatch means capture and screening disagreed about which
    /// rules were in force. Fields stay bounded to the hash, counts and a fixed reason token —
    /// never the matched binding, the rule content, or any observation value.
    fn record_match_failure(&self, tx_hash: B256, error: &matching::MatchError) {
        let configured = self.shared.config.max_event_bindings_per_tx;
        match error.kind() {
            matching::MatchErrorKind::BindingBudget => {
                let (attempted, limit) = binding_overflow_counts(error, configured);
                error!(
                    target: "rcs_filter",
                    tx_hash = %tx_hash,
                    observed_count = attempted,
                    limit = limit,
                    "matcher binding budget exceeded; denying transaction (Screen::Deny)"
                );
                self.shared.metrics.deny_total.increment(1);
            }
            matching::MatchErrorKind::ObservationsUnavailable => {
                let reason = match error {
                    matching::MatchError::ObservationsUnavailable { reason, .. } => reason.as_str(),
                    _ => "unknown",
                };
                error!(
                    target: "rcs_filter",
                    tx_hash = %tx_hash,
                    reason,
                    "callstack observations unavailable for a rule that requires them; denying transaction (Screen::Deny)"
                );
                self.shared.metrics.deny_total.increment(1);
                self.shared.metrics.deny_observations_unavailable_total.increment(1);
            }
            matching::MatchErrorKind::ObservationSnapshotMismatch => {
                error!(
                    target: "rcs_filter",
                    tx_hash = %tx_hash,
                    "observation set was captured under a different rule snapshot; denying transaction (Screen::Deny)"
                );
                self.shared.metrics.deny_total.increment(1);
                self.shared.metrics.deny_observation_snapshot_mismatch_total.increment(1);
            }
        }
    }

    /// Returns a status-only decision before EVM execution. Fresh and approved transactions still
    /// require execution because their logs determine the authoritative screening result.
    pub fn pre_screen(&self, tx_hash: &B256) -> PreScreen {
        match self.shared.pool_lock().get(tx_hash).map(|entry| entry.status) {
            None | Some(BufferStatus::Approved | BufferStatus::TimedOutAllow) => PreScreen::Execute,
            Some(BufferStatus::Dropped) => PreScreen::Drop,
            Some(BufferStatus::NotSubmitted | BufferStatus::Submitted | BufferStatus::Pending) => {
                PreScreen::Defer
            }
        }
    }

    /// Removes buffered entries for transactions included in a newly canonical chain segment.
    pub fn remove_canonical_transactions(&self, hashes: &[B256]) -> usize {
        let removed = self.shared.pool_lock().remove_canonical(hashes);
        self.shared.metrics.canonical_cleanup_total.increment(removed as u64);
        self.shared.update_buffer_metric();
        removed
    }

    /// Invalidates lifecycle state that might have crossed an unobserved canonical update.
    pub fn recover_after_canonical_lag(&self, skipped: u64) -> usize {
        let removed = self.shared.pool_lock().recover_after_canonical_lag();
        self.shared.metrics.canonical_channel_lag_total.increment(skipped);
        self.shared.metrics.canonical_lag_recovery_total.increment(removed as u64);
        self.shared.update_buffer_metric();
        removed
    }

    /// Snapshot of dropped hashes used by the builder to reconcile missed discard notifications.
    pub fn dropped_hashes(&self) -> Vec<B256> {
        self.shared.pool_lock().dropped_hashes()
    }

    /// Snapshot of dropped hash/generation tokens used for race-safe txpool reconciliation.
    pub fn dropped_lifecycles(&self) -> Vec<(B256, u64)> {
        self.shared.pool_lock().dropped_lifecycles()
    }

    /// Snapshot of non-terminal lifecycle tokens used to remove state for transactions that no
    /// longer exist in the builder's transaction pool.
    pub fn non_terminal_lifecycles(&self) -> Vec<(B256, u64)> {
        self.shared.pool_lock().non_terminal_lifecycles()
    }

    /// Removes an exact non-terminal lifecycle after the builder confirms the transaction is no
    /// longer in txpool. A stale generation can never remove a reinserted lifecycle.
    pub fn remove_non_terminal_if_generation(&self, tx_hash: &B256, generation: u64) -> bool {
        let removed =
            self.shared.pool_lock().remove_non_terminal_if_generation(tx_hash, generation);
        if removed {
            self.shared.metrics.txpool_absent_cleanup_total.increment(1);
            self.shared.update_buffer_metric();
        }
        removed
    }

    /// Runs `action` while the exact dropped lifecycle is pinned under the filter pool lock.
    /// This closes the check/use window with canonical cleanup and same-hash reinsertion.
    pub fn with_dropped_lifecycle<R>(
        &self,
        tx_hash: &B256,
        generation: u64,
        action: impl FnOnce() -> R,
    ) -> Option<R> {
        let pool = self.shared.pool_lock();
        if pool.matches(tx_hash, generation, BufferStatus::Dropped) {
            Some(action())
        } else {
            None
        }
    }

    /// Subscribes to background terminal decisions. Lagged consumers must reconcile via
    /// [`Self::dropped_hashes`].
    pub fn subscribe_terminal_events(&self) -> tokio::sync::broadcast::Receiver<TerminalEvent> {
        self.shared.terminal_events.subscribe()
    }

    /// Records builder-side terminal event receiver lag and reconciliation work.
    pub fn record_terminal_reconciliation(&self, skipped: u64, reconciled: usize) {
        self.shared.metrics.terminal_channel_lag_total.increment(skipped);
        self.shared.metrics.terminal_reconciliation_total.increment(reconciled as u64);
    }

    /// Records whether a terminal discard found the transaction in the transaction pool.
    pub fn record_txpool_discard(&self, removed: bool) {
        if removed {
            self.shared.metrics.txpool_discard_success_total.increment(1);
        } else {
            self.shared.metrics.txpool_discard_already_absent_total.increment(1);
        }
    }

    /// Handles a transaction that already has a buffer-pool entry (dedup short-circuit).
    /// Terminal tombstones map deterministically without re-screening; the in-flight and
    /// `Approved` cases are handled separately by the consistency check.
    fn screen_existing(
        &self,
        input: &ScreenInput,
        status: BufferStatus,
        stored_hash: B256,
        generation: u64,
        rule_snapshot: Arc<RuleSet>,
    ) -> Screen {
        match status {
            // Terminal fail-open for the RCS verdict only: the audit service never answered, but a
            // local deny (e.g. a codeHash whitelist rule) needs no RCS and must still win. Re-run
            // the matcher on THIS execution against the saved snapshot; no quota-consistency gate,
            // no clock reset, no re-submit.
            BufferStatus::TimedOutAllow => {
                let release = match matching::try_evaluate(
                    &rule_snapshot,
                    input,
                    self.shared.config.max_event_bindings_per_tx,
                ) {
                    Ok(MatchOutcome::Allow | MatchOutcome::Audit { .. }) => true,
                    Ok(MatchOutcome::Deny) => {
                        // Bounded: tx hash and a fixed reason only.
                        tracing::warn!(
                            target: "rcs_filter",
                            tx_hash = %input.tx_hash,
                            "local deny matched on timed-out-allow re-check; dropping (Screen::Drop)"
                        );
                        false
                    }
                    Err(error) => {
                        let (attempted, limit) = binding_overflow_counts(
                            &error,
                            self.shared.config.max_event_bindings_per_tx,
                        );
                        error!(
                            target: "rcs_filter",
                            tx_hash = %input.tx_hash,
                            kind = ?error.kind(),
                            observed_count = attempted,
                            limit = limit,
                            "screening failed closed during timed-out-allow re-check"
                        );
                        false
                    }
                };
                let now = self.shared.clock.now_unix();
                let status = self.shared.pool_lock().finish_timed_out_allow_recheck(
                    &input.tx_hash,
                    generation,
                    release,
                    now,
                );
                if !release {
                    self.shared.update_buffer_metric();
                }
                match status {
                    Some(BufferStatus::TimedOutAllow) if release => {
                        self.shared.metrics.audit_approved_total.increment(1);
                        Screen::AuditApproved
                    }
                    Some(BufferStatus::Dropped) => {
                        if !release {
                            self.shared.metrics.timed_out_allow_recheck_drop_total.increment(1);
                        }
                        self.shared.metrics.drop_total.increment(1);
                        Screen::Drop
                    }
                    _ => Screen::AuditPending,
                }
            }

            // Terminal: dropped (denied/outdated/fail-close/consistency-mismatch). Never
            // packaged, never re-submitted, and never re-admitted this build cycle.
            BufferStatus::Dropped => {
                if !self.shared.pool_lock().matches(
                    &input.tx_hash,
                    generation,
                    BufferStatus::Dropped,
                ) {
                    return Screen::AuditPending;
                }
                self.shared.metrics.drop_total.increment(1);
                Screen::Drop
            }

            // Approved → pre-package consistency check: re-simulate the quota from the
            // current logs and compare to the submit-time hash. Transition to a terminal
            // tombstone either way (no RCS cancel call on mismatch).
            BufferStatus::Approved => {
                // Uses the SAME configured budget as the first `screen_tx` so a transaction that
                // passes first screening can never be dropped at consistency recompute for a
                // divergent limit.
                let consistent = match matching::try_evaluate(
                    &rule_snapshot,
                    input,
                    self.shared.config.max_event_bindings_per_tx,
                ) {
                    Ok(MatchOutcome::Audit { actions, .. }) => {
                        quota_hash::encode_and_hash(&actions) == stored_hash
                    }
                    Err(error) => {
                        // Determinism invariant: post-approval re-simulation runs against the SAME
                        // rule snapshot and the SAME `max_event_bindings_per_tx` as the first
                        // screen, so a matcher-binding overflow cannot newly fire here. A
                        // re-simulation does, however, re-capture its own observations, so an
                        // unavailable or over-budget observation set CAN first appear at this
                        // point; it must not be read as a clean no-match either, and inconsistency
                        // is the fail-closed answer. Diagnostics stay bounded.
                        let (attempted, limit) = binding_overflow_counts(
                            &error,
                            self.shared.config.max_event_bindings_per_tx,
                        );
                        error!(
                            target: "rcs_filter",
                            tx_hash = %input.tx_hash,
                            kind = ?error.kind(),
                            observed_count = attempted,
                            limit = limit,
                            "screening failed closed during consistency recompute"
                        );
                        false
                    }
                    _ => false,
                };
                let now = self.shared.clock.now_unix();
                let status = self.shared.pool_lock().finish_consistency_check(
                    &input.tx_hash,
                    generation,
                    consistent,
                    now,
                );
                self.shared.update_buffer_metric();
                match status {
                    Some(BufferStatus::Approved) if consistent => {
                        self.shared.metrics.audit_approved_total.increment(1);
                        Screen::AuditApproved
                    }
                    Some(BufferStatus::Dropped) => {
                        // Reached on a genuine quota-consistency mismatch. A matcher-binding
                        // overflow could also route here via `consistent = false`, but per the
                        // determinism invariant above that cannot newly fire at recompute, so the
                        // terminal state stays `Screen::Drop` (design §5.7 — not unified to Deny).
                        self.shared.metrics.consistency_mismatch_total.increment(1);
                        self.shared.metrics.drop_total.increment(1);
                        Screen::Drop
                    }
                    // A concurrent query resolution takes precedence over the stale consistency
                    // result. Other non-terminal states remain pending.
                    _ => Screen::AuditPending,
                }
            }

            // NotSubmitted / Submitted / Pending → still in flight; skip this round.
            _ => Screen::AuditPending,
        }
    }

    /// Installs a rule set and marks the handle ready. Intended for tests and for a
    /// synchronous first-load path; production hot-reload goes through the worker.
    pub fn install_rules(&self, rules: RuleSet) {
        self.shared.install_rules(rules);
    }

    /// Buffer status of a transaction, if buffered (test/observability helper).
    pub fn buffer_status(&self, tx_hash: &B256) -> Option<BufferStatus> {
        self.shared.pool_lock().get(tx_hash).map(|e| e.status)
    }

    /// Test-only accessor to drive the buffer pool directly (simulating worker transitions)
    /// without spawning the async workers.
    ///
    /// Compiled for this crate's own tests and for dependents whose `[dev-dependencies]` enable
    /// the non-default `test-utils` feature. It is absent from every production build: a
    /// dependent's `cfg(test)` is inactive while this crate is compiled as a dependency, so the
    /// feature — not `test` — is what carries the helper across the crate boundary.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_pool<R>(&self, f: impl FnOnce(&mut crate::pool::BufferPool) -> R) -> R {
        let mut pool = self.shared.pool_lock();
        f(&mut pool)
    }

    /// Advances a buffered transaction to Approved state for testing, without spawning workers.
    ///
    /// Compiled for this crate's own tests and for dependents that enable the non-default
    /// `test-utils` feature from `[dev-dependencies]`; never exposed by a production build.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn advance_to_approved_for_test(&self, tx_hash: &B256) {
        let hash_str = format!("{tx_hash:#x}");
        let now = self.shared.clock.now_unix();
        let mut pool = self.shared.pool_lock();
        let Some(generation) = pool.get(tx_hash).map(|e| e.generation) else { return };
        let expected = std::collections::HashMap::from([(*tx_hash, generation)]);
        pool.apply_submit_response(&[hash_str], &expected, &FilterConfig::default(), now);
        pool.apply_query_status(tx_hash, generation, "approved", &FilterConfig::default(), now);
    }

    /// Constructs a handle without spawning workers, pre-loaded with `rules` (test helper —
    /// lets `screen_tx` be exercised without a tokio runtime).
    pub fn for_test(config: FilterConfig, rules: RuleSet, clock: Arc<dyn Clock>) -> Self {
        let (terminal_events, _) = tokio::sync::broadcast::channel(TERMINAL_EVENT_CAPACITY);
        let metrics = RcsFilterMetrics::default();
        metrics.rules_ready.set(1.0);
        metrics.rules_content_version.set(rules.content_version as f64);
        let shared = Shared {
            config,
            rules: Arc::new(RwLock::new(Arc::new(rules))),
            pool: Arc::new(Mutex::new(BufferPool::default())),
            clock,
            ready: Arc::new(AtomicBool::new(true)),
            terminal_events,
            metrics,
        };
        Self { shared, workers: Vec::new() }
    }
}

#[path = "../tests/handle__tests.rs"]
#[cfg(test)]
mod tests;
