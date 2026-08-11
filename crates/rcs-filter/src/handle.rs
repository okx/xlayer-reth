//! [`FilterHandle`] — the synchronous entry the block builder calls per transaction, plus
//! the owner of shared filter state and background workers.
//!
//! `screen_tx` performs **zero network IO**: dedup short-circuit → in-memory match/merge →
//! buffer-pool bookkeeping. All RCS traffic happens on the [`crate::worker`] tasks spawned
//! by [`FilterHandle::spawn`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use alloy_primitives::{Address, Log, B256, U256};
use tracing::warn;

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

    /// Screens one transaction (synchronous, no network IO). See [`Screen`].
    pub fn screen_tx(&self, input: &ScreenInput) -> Screen {
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

        // Fresh evaluation.
        let rules = self.shared.current_rules();
        let outcome = match matching::try_evaluate(&rules, input) {
            Ok(outcome) => outcome,
            Err(error) => {
                warn!(
                    target: "rcs_filter",
                    tx_hash = %input.tx_hash,
                    %error,
                    "matching budget exceeded; denying transaction"
                );
                self.shared.metrics.deny_total.increment(1);
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
            // Terminal fail-open: release into the block without resetting the retry clock.
            BufferStatus::TimedOutAllow => {
                if !self.shared.pool_lock().matches(
                    &input.tx_hash,
                    generation,
                    BufferStatus::TimedOutAllow,
                ) {
                    return Screen::AuditPending;
                }
                self.shared.metrics.audit_approved_total.increment(1);
                Screen::AuditApproved
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
                let consistent = match matching::try_evaluate(&rule_snapshot, input) {
                    Ok(MatchOutcome::Audit { actions, .. }) => {
                        quota_hash::encode_and_hash(&actions) == stored_hash
                    }
                    Err(error) => {
                        warn!(
                            target: "rcs_filter",
                            tx_hash = %input.tx_hash,
                            %error,
                            "matching budget exceeded during consistency check"
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
    #[cfg(test)]
    pub(crate) fn with_pool<R>(&self, f: impl FnOnce(&mut crate::pool::BufferPool) -> R) -> R {
        let mut pool = self.shared.pool_lock();
        f(&mut pool)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{load_rules, RawRule};
    use crate::test_support::{golden, log_builder, TestClock};
    use alloy_primitives::{Bytes, LogData};
    use std::str::FromStr;

    fn handle_with_scenario_a() -> FilterHandle {
        let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()]);
        FilterHandle::for_test(
            FilterConfig::default(),
            rules,
            Arc::new(TestClock::new(1_751_000_000)),
        )
    }

    fn scenario_a_input<'a>(logs: &'a [Log]) -> ScreenInput<'a> {
        ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: U256::ZERO,
            block_height: 1_000_000,
            logs,
        }
    }

    #[test]
    fn audit_tx_enters_pool_as_pending() {
        let h = handle_with_scenario_a();
        let logs = vec![log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        assert_eq!(h.screen_tx(&scenario_a_input(&logs)), Screen::AuditPending);
        assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::NotSubmitted));
        // Second call is deduped (still pending, not re-added).
        assert_eq!(h.screen_tx(&scenario_a_input(&logs)), Screen::AuditPending);
        assert_eq!(h.buffered_len(), 1);
    }

    #[test]
    fn deny_tx_returns_deny_without_buffering() {
        let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_B).unwrap()]);
        let h = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(TestClock::new(1)));
        let logs = vec![log_builder::erc20_transfer(
            golden::token_x(),
            golden::blacklisted_from(),
            golden::recipient(),
            golden::one_token(),
        )];
        let input = ScreenInput {
            tx_hash: golden::tx_b(),
            origin: golden::origin_b(),
            tx_to: Some(golden::claim_contract()),
            nonce: 0,
            value: U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
        };
        assert_eq!(h.screen_tx(&input), Screen::Deny);
        assert!(h.buffer_status(&golden::tx_b()).is_none());
    }

    #[test]
    fn excessive_complete_bindings_deny_without_buffering() {
        let raw: RawRule = serde_json::from_str(
            r#"{"id":"excessive","event_abis":{"a":{"type":"event","name":"A","inputs":[],"anonymous":true},"b":{"type":"event","name":"B","inputs":[],"anonymous":true}},"audit_types":["custom"],"condition":true,"action":"audit"}"#,
        )
        .unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        let h = FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(TestClock::new(1)));
        let logs = (0..65)
            .map(|_| Log {
                address: golden::token_x(),
                data: LogData::new_unchecked(vec![], Bytes::new()),
            })
            .collect::<Vec<_>>();
        let input = scenario_a_input(&logs);

        assert_eq!(h.screen_tx(&input), Screen::Deny);
        assert!(h.buffer_status(&input.tx_hash).is_none());
    }

    #[test]
    fn unmatched_tx_is_allowed() {
        let h = handle_with_scenario_a();
        assert_eq!(h.screen_tx(&scenario_a_input(&[])), Screen::Allow);
    }

    // ---- Builder-visible terminal tombstone semantics ------------------------------------

    fn handle_and_clock(start: u64) -> (FilterHandle, std::sync::Arc<TestClock>) {
        let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()]);
        let clock = std::sync::Arc::new(TestClock::new(start));
        (FilterHandle::for_test(FilterConfig::default(), rules, clock.clone()), clock)
    }

    fn audit_input<'a>(
        tx_hash: B256,
        nonce: u64,
        block_height: u64,
        logs: &'a [Log],
    ) -> ScreenInput<'a> {
        ScreenInput {
            tx_hash,
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce,
            value: U256::ZERO,
            block_height,
            logs,
        }
    }

    fn transfer_log(amount: &str) -> Log {
        log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            U256::from_str(amount).unwrap(),
        )
    }

    /// A denied decision tombstones the tx as `Dropped`; it is never
    /// packaged or re-submitted, and re-screening requests tx-pool eviction.
    #[test]
    fn scenario_c_denied_drops_and_never_resubmits() {
        let (h, _clock) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::THREE_TOKENS)];
        let input = audit_input(golden::tx_c(), 2, 1_000_010, &logs);

        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
        assert_eq!(h.buffer_status(&golden::tx_c()), Some(BufferStatus::NotSubmitted));

        let now = 1_751_000_000;
        h.with_pool(|p| {
            p.apply_submit_response_current(&[format!("{:#x}", golden::tx_c())], now);
            p.apply_query_status_current(&golden::tx_c(), "denied", now);
        });
        assert_eq!(h.buffer_status(&golden::tx_c()), Some(BufferStatus::Dropped));

        // Re-screen: still dropped; not re-admitted, not re-submitted, not re-queried.
        assert_eq!(h.screen_tx(&input), Screen::Drop);
        h.with_pool(|p| {
            assert!(p.not_submitted().is_empty());
            assert!(p.in_flight_hashes().is_empty());
        });
    }

    /// For a pending → approved → outdated flow, the first `outdated`
    /// observation drops the tx; re-screening never returns `AuditApproved` and the
    /// consistency check is never triggered (it went straight to `Dropped`).
    #[test]
    fn scenario_d_outdated_drops_without_consistency_check() {
        let (h, _clock) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::TWO_TOKENS)];
        let input = audit_input(golden::tx_d(), 3, 1_000_020, &logs);

        assert_eq!(h.screen_tx(&input), Screen::AuditPending);

        let now = 1_751_000_000;
        h.with_pool(|p| {
            p.apply_submit_response_current(&[format!("{:#x}", golden::tx_d())], now);
            p.apply_query_status_current(&golden::tx_d(), "pending", now);
            p.apply_query_status_current(&golden::tx_d(), "approved", now);
            p.apply_query_status_current(&golden::tx_d(), "outdated", now);
        });
        assert_eq!(h.buffer_status(&golden::tx_d()), Some(BufferStatus::Dropped));
        assert_eq!(h.screen_tx(&input), Screen::Drop);
    }

    /// Fail-open behavior: an audit tx whose `audit_timeout_action=allow` exceeds the 90s
    /// outer timeout is released into the block via `TimedOutAllow → AuditApproved`,
    /// deterministically on every subsequent round (no clock reset, no re-buffer).
    #[test]
    fn timed_out_allow_is_terminal_allow() {
        let (h, clock) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::ONE_TOKEN)];
        let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);

        assert_eq!(h.screen_tx(&input), Screen::AuditPending);

        clock.advance(91);
        let now = clock.now_unix();
        h.with_pool(|p| {
            let resolved = p.check_timeouts(&FilterConfig::default(), now);
            assert_eq!(resolved.len(), 1);
        });
        assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::TimedOutAllow));
        assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
        // Deterministic on re-entry.
        assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
    }

    /// Consistency pass: approved + matching re-simulation → release.
    #[test]
    fn approved_consistency_pass_is_attempt_scoped() {
        let (h, _clock) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::ONE_TOKEN)];
        let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);

        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
        let now = 1_751_000_000;
        h.with_pool(|p| {
            p.apply_submit_response_current(&[format!("{:#x}", golden::tx_a())], now);
            p.apply_query_status_current(&golden::tx_a(), "approved", now);
        });
        // Same logs → hash matches → release.
        assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
        assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));
    }

    #[test]
    fn cancelled_attempt_rechecks_changed_actions() {
        let (h, _clock) = handle_and_clock(1_751_000_000);
        let original_logs = vec![transfer_log(golden::ONE_TOKEN)];
        let original = audit_input(golden::tx_a(), 1, 1_000_000, &original_logs);
        assert_eq!(h.screen_tx(&original), Screen::AuditPending);
        h.with_pool(|p| {
            p.apply_submit_response_current(&[format!("{:#x}", golden::tx_a())], 1_751_000_000);
            p.apply_query_status_current(&golden::tx_a(), "approved", 1_751_000_000);
        });
        assert_eq!(h.screen_tx(&original), Screen::AuditApproved);
        assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Approved));

        let changed_logs = vec![transfer_log(golden::TWO_TOKENS)];
        let changed = audit_input(golden::tx_a(), 1, 1_000_000, &changed_logs);
        assert_eq!(h.screen_tx(&changed), Screen::Drop);
    }

    #[test]
    fn pre_screen_defers_all_inflight_states() {
        for target in ["not_submitted", "submitted", "pending"] {
            let (h, _) = handle_and_clock(1_751_000_000);
            let logs = vec![transfer_log(golden::ONE_TOKEN)];
            let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);
            assert_eq!(h.screen_tx(&input), Screen::AuditPending);
            h.with_pool(|pool| {
                if target != "not_submitted" {
                    pool.apply_submit_response_current(
                        &[format!("{:#x}", golden::tx_a())],
                        1_751_000_000,
                    );
                }
                if target == "pending" {
                    pool.apply_query_status_current(&golden::tx_a(), "pending", 1_751_000_000);
                }
            });
            assert_eq!(h.pre_screen(&golden::tx_a()), PreScreen::Defer);
        }
    }

    #[test]
    fn pre_screen_executes_approved_and_timeout_allow() {
        for target in ["approved", "timed_out_allow"] {
            let (h, _) = handle_and_clock(1_751_000_000);
            let logs = vec![transfer_log(golden::ONE_TOKEN)];
            let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);
            assert_eq!(h.screen_tx(&input), Screen::AuditPending);
            h.with_pool(|pool| {
                if target == "approved" {
                    pool.apply_submit_response_current(
                        &[format!("{:#x}", golden::tx_a())],
                        1_751_000_000,
                    );
                    pool.apply_query_status_current(&golden::tx_a(), "approved", 1_751_000_000);
                } else {
                    pool.check_timeouts(&FilterConfig::default(), 1_751_000_091);
                }
            });
            assert_eq!(h.pre_screen(&golden::tx_a()), PreScreen::Execute);
        }
    }

    #[test]
    fn pending_precheck_marks_descendants_invalid_for_iterator() {
        let (h, _) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::ONE_TOKEN)];
        let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);
        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
        assert_eq!(h.pre_screen(&golden::tx_a()), PreScreen::Defer);
    }

    #[test]
    fn approved_can_become_outdated_before_canonical_cleanup() {
        let (h, _) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::ONE_TOKEN)];
        let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);
        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
        h.with_pool(|pool| {
            pool.apply_submit_response_current(&[format!("{:#x}", golden::tx_a())], 1_751_000_000);
            pool.apply_query_status_current(&golden::tx_a(), "approved", 1_751_000_000);
        });
        assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
        h.with_pool(|pool| {
            pool.apply_query_status_current(&golden::tx_a(), "outdated", 1_751_000_001);
        });
        assert_eq!(h.pre_screen(&golden::tx_a()), PreScreen::Drop);
    }

    /// Consistency mismatch: approved but the pre-package re-simulation yields
    /// a different quota → hard drop, no RCS cancel, never re-admitted.
    #[test]
    fn approved_consistency_mismatch_hard_drops() {
        let (h, _clock) = handle_and_clock(1_751_000_000);
        let submit_logs = vec![transfer_log(golden::ONE_TOKEN)];
        let input_submit = audit_input(golden::tx_a(), 1, 1_000_000, &submit_logs);

        assert_eq!(h.screen_tx(&input_submit), Screen::AuditPending);
        let now = 1_751_000_000;
        h.with_pool(|p| {
            p.apply_submit_response_current(&[format!("{:#x}", golden::tx_a())], now);
            p.apply_query_status_current(&golden::tx_a(), "approved", now);
        });

        // Re-simulate with a different amount → quota hash mismatch → drop.
        let repackage_logs = vec![transfer_log(golden::TWO_TOKENS)];
        let input_repackage = audit_input(golden::tx_a(), 1, 1_000_000, &repackage_logs);
        assert_eq!(h.screen_tx(&input_repackage), Screen::Drop);
        assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
        // Never re-admitted / never released.
        assert_eq!(h.screen_tx(&input_repackage), Screen::Drop);
    }

    #[test]
    fn approved_mismatch_after_total_timeout_still_drops() {
        let (h, clock) = handle_and_clock(1_751_000_000);
        let original_logs = vec![transfer_log(golden::ONE_TOKEN)];
        let original = audit_input(golden::tx_a(), 1, 1_000_000, &original_logs);
        assert_eq!(h.screen_tx(&original), Screen::AuditPending);
        h.with_pool(|pool| {
            pool.apply_submit_response_current(&[format!("{:#x}", golden::tx_a())], 1_751_000_000);
            pool.apply_query_status_current(&golden::tx_a(), "approved", 1_751_000_001);
        });

        clock.set(1_751_000_200);
        let changed_logs = vec![transfer_log(golden::TWO_TOKENS)];
        let changed = audit_input(golden::tx_a(), 1, 1_000_000, &changed_logs);
        assert_eq!(h.screen_tx(&changed), Screen::Drop);
        assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
    }

    #[test]
    fn rules_state_updates_on_install_and_reload() {
        let (h, _) = handle_and_clock(1_751_000_000);
        assert!(h.is_ready());
        assert_eq!(h.shared.current_rules().content_version, 1);

        let replacement =
            load_rules(1, 2, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()]);
        h.install_rules(replacement);
        assert!(h.is_ready());
        assert_eq!(h.shared.current_rules().content_version, 2);
    }
}
