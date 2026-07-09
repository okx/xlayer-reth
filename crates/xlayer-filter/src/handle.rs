//! [`FilterHandle`] — the synchronous entry the block builder calls per transaction, plus
//! the owner of shared filter state and background workers (TD §4.2/§4.7).
//!
//! `screen_tx` performs **zero network IO**: dedup short-circuit → in-memory match/merge →
//! buffer-pool bookkeeping. All RCS traffic happens on the [`crate::worker`] tasks spawned
//! by [`FilterHandle::spawn`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use alloy_primitives::{Address, Log, B256, U256};

use crate::client::RcsClient;
use crate::clock::Clock;
use crate::config::FilterConfig;
use crate::matching::{self, MatchOutcome};
use crate::pool::{BufferEntry, BufferPool, BufferStatus};
use crate::quota_hash;
use crate::rules::RuleSet;

/// The screening decision returned to the builder hot path (TD §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Package normally (no rule required action, or an already-approved-and-consistent tx).
    Allow,
    /// Exclude via `mark_invalid` (a `deny` rule matched).
    Deny,
    /// Skip this round (do not commit, do not `mark_invalid`); tx stays in the pool for a
    /// later round while its audit adjudication proceeds.
    AuditPending,
    /// Approved and the pre-package consistency check passed → package normally.
    AuditApproved,
}

/// Input to [`FilterHandle::screen_tx`], borrowed from the builder loop (TD §4.2).
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
    /// Atomically hot-swapped rule snapshot (TD §4.9; `Arc<RwLock<Arc<..>>>` per R-8, no
    /// new dependency — hot path takes a read lock and clones the inner `Arc`).
    pub rules: Arc<RwLock<Arc<RuleSet>>>,
    pub pool: Arc<Mutex<BufferPool>>,
    pub clock: Arc<dyn Clock>,
    /// Set once the first valid rule set is loaded (FR-2 startup gate).
    pub ready: Arc<AtomicBool>,
}

impl Shared {
    fn current_rules(&self) -> Arc<RuleSet> {
        self.rules.read().expect("rules lock not poisoned").clone()
    }
}

/// Handle exposing the synchronous screening entry and owning background workers.
pub struct FilterHandle {
    shared: Shared,
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
        let shared = Shared {
            config,
            rules: Arc::new(RwLock::new(Arc::new(RuleSet::default()))),
            pool: Arc::new(Mutex::new(BufferPool::default())),
            clock,
            ready: Arc::new(AtomicBool::new(false)),
        };
        crate::worker::spawn(shared.clone(), client);
        Arc::new(Self { shared })
    }

    /// True once the first valid rule set has loaded (FR-2: the node should not mine until
    /// this is true when the filter is enabled).
    pub fn is_ready(&self) -> bool {
        self.shared.ready.load(Ordering::Acquire)
    }

    /// Number of transactions currently buffered awaiting adjudication.
    pub fn buffered_len(&self) -> usize {
        self.shared.pool.lock().expect("pool lock not poisoned").len()
    }

    /// Screens one transaction (synchronous, no network IO). See [`Screen`].
    pub fn screen_tx(&self, input: &ScreenInput) -> Screen {
        // Stage zero: dedup short-circuit — a non-terminal buffered entry is reused without
        // re-decoding or re-submitting (FR-4 stage zero).
        let existing = {
            let pool = self.shared.pool.lock().expect("pool lock");
            pool.get(&input.tx_hash).map(|e| (e.status, e.quota_consistency_hash))
        };
        if let Some((status, stored_hash)) = existing {
            return self.screen_existing(input, status, stored_hash);
        }

        // Fresh evaluation.
        let rules = self.shared.current_rules();
        match matching::evaluate(&rules, input) {
            MatchOutcome::Allow => Screen::Allow,
            MatchOutcome::Deny => Screen::Deny,
            MatchOutcome::Audit { actions, timeout_action } => {
                let now = self.shared.clock.now_unix();
                let quota_consistency_hash = quota_hash::encode_and_hash(&actions);
                let entry = BufferEntry {
                    tx_hash: input.tx_hash,
                    origin: input.origin,
                    contract_address: input.tx_to.unwrap_or(Address::ZERO),
                    nonce: input.nonce,
                    block_height: input.block_height,
                    status: BufferStatus::NotSubmitted,
                    actions,
                    quota_consistency_hash,
                    timeout_action,
                    first_not_submitted_at: now,
                    last_transition_at: now,
                };
                self.shared.pool.lock().expect("pool lock").insert(entry);
                Screen::AuditPending
            }
        }
    }

    /// Handles a transaction that already has a buffer-pool entry (dedup short-circuit).
    /// Terminal tombstones map deterministically without re-screening; the in-flight and
    /// `Approved` cases are handled per FR-1/FR-7.
    fn screen_existing(
        &self,
        input: &ScreenInput,
        status: BufferStatus,
        stored_hash: B256,
    ) -> Screen {
        match status {
            // Terminal: release into the block (fail-open timeout, or a prior approved +
            // consistency pass). Deterministic on every re-entry; no clock reset (G1).
            BufferStatus::ReleasePending => Screen::AuditApproved,

            // Terminal: dropped (denied/outdated/fail-close/consistency-mismatch). Never
            // packaged, never re-submitted, never re-admitted this build cycle (G2/G3).
            BufferStatus::Dropped => Screen::AuditPending,

            // Approved → FR-7 pre-package consistency check: re-simulate the quota from the
            // current logs and compare to the submit-time hash. Transition to a terminal
            // tombstone either way (no RCS cancel call on mismatch, TD §4.8).
            BufferStatus::Approved => {
                let rules = self.shared.current_rules();
                let consistent = match matching::evaluate(&rules, input) {
                    MatchOutcome::Audit { actions, .. } => {
                        quota_hash::encode_and_hash(&actions) == stored_hash
                    }
                    _ => false,
                };
                let now = self.shared.clock.now_unix();
                let mut pool = self.shared.pool.lock().expect("pool lock");
                if consistent {
                    pool.set_terminal(&input.tx_hash, BufferStatus::ReleasePending, now);
                    Screen::AuditApproved
                } else {
                    // G2: consistency mismatch → hard drop (deterministic, no re-admit).
                    pool.set_terminal(&input.tx_hash, BufferStatus::Dropped, now);
                    Screen::AuditPending
                }
            }

            // NotSubmitted / Submitted / Pending / Outdated → still in flight; skip this round.
            _ => Screen::AuditPending,
        }
    }

    /// Installs a rule set and marks the handle ready. Intended for tests and for a
    /// synchronous first-load path; production hot-reload goes through the worker.
    pub fn install_rules(&self, rules: RuleSet) {
        *self.shared.rules.write().expect("rules lock") = Arc::new(rules);
        self.shared.ready.store(true, Ordering::Release);
    }

    /// Buffer status of a transaction, if buffered (test/observability helper).
    pub fn buffer_status(&self, tx_hash: &B256) -> Option<BufferStatus> {
        self.shared.pool.lock().expect("pool lock").get(tx_hash).map(|e| e.status)
    }

    /// Test-only accessor to drive the buffer pool directly (simulating worker transitions)
    /// without spawning the async workers.
    #[cfg(test)]
    pub(crate) fn with_pool<R>(&self, f: impl FnOnce(&mut crate::pool::BufferPool) -> R) -> R {
        let mut pool = self.shared.pool.lock().expect("pool lock");
        f(&mut pool)
    }

    /// Constructs a handle without spawning workers, pre-loaded with `rules` (test helper —
    /// lets `screen_tx` be exercised without a tokio runtime).
    pub fn for_test(config: FilterConfig, rules: RuleSet, clock: Arc<dyn Clock>) -> Self {
        let shared = Shared {
            config,
            rules: Arc::new(RwLock::new(Arc::new(rules))),
            pool: Arc::new(Mutex::new(BufferPool::default())),
            clock,
            ready: Arc::new(AtomicBool::new(true)),
        };
        Self { shared }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::load_rules;
    use crate::test_support::{golden, log_builder, TestClock};
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
    fn unmatched_tx_is_allowed() {
        let h = handle_with_scenario_a();
        assert_eq!(h.screen_tx(&scenario_a_input(&[])), Screen::Allow);
    }

    // ---- G1-G4: builder-visible terminal tombstone semantics ------------------------------

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

    /// FR-10 scenario c (denied → drop). Denied tombstones the tx as `Dropped`; it is never
    /// packaged, never re-submitted, and re-screening keeps returning `AuditPending` (G3).
    #[test]
    fn scenario_c_denied_drops_and_never_resubmits() {
        let (h, _clock) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::THREE_TOKENS)];
        let input = audit_input(golden::tx_c(), 2, 1_000_010, &logs);

        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
        assert_eq!(h.buffer_status(&golden::tx_c()), Some(BufferStatus::NotSubmitted));

        let now = 1_751_000_000;
        h.with_pool(|p| {
            p.apply_submit_response(&[format!("{:#x}", golden::tx_c())], now);
            p.apply_query_status(&golden::tx_c(), "denied", now);
        });
        assert_eq!(h.buffer_status(&golden::tx_c()), Some(BufferStatus::Dropped));

        // Re-screen: still dropped; not re-admitted, not re-submitted, not re-queried.
        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
        h.with_pool(|p| {
            assert!(p.not_submitted().is_empty());
            assert!(p.in_flight_hashes().is_empty());
        });
    }

    /// FR-10 scenario d (pending → approved → outdated → drop). The first `outdated`
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
            p.apply_submit_response(&[format!("{:#x}", golden::tx_d())], now);
            p.apply_query_status(&golden::tx_d(), "pending", now);
            p.apply_query_status(&golden::tx_d(), "approved", now);
            p.apply_query_status(&golden::tx_d(), "outdated", now);
        });
        assert_eq!(h.buffer_status(&golden::tx_d()), Some(BufferStatus::Dropped));
        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
    }

    /// FR-6 §6.5 fail-open: an audit tx whose `audit_timeout_action=allow` exceeds the 90s
    /// outer timeout is released into the block via `ReleasePending → AuditApproved` (G1),
    /// deterministically on every subsequent round (no clock reset, no re-buffer).
    #[test]
    fn fail_open_timeout_releases_for_packaging() {
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
        assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::ReleasePending));
        assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
        // Deterministic on re-entry.
        assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
    }

    /// FR-7 §6.6 consistency pass: approved + matching re-simulation → release.
    #[test]
    fn approved_consistency_pass_releases() {
        let (h, _clock) = handle_and_clock(1_751_000_000);
        let logs = vec![transfer_log(golden::ONE_TOKEN)];
        let input = audit_input(golden::tx_a(), 1, 1_000_000, &logs);

        assert_eq!(h.screen_tx(&input), Screen::AuditPending);
        let now = 1_751_000_000;
        h.with_pool(|p| {
            p.apply_submit_response(&[format!("{:#x}", golden::tx_a())], now);
            p.apply_query_status(&golden::tx_a(), "approved", now);
        });
        // Same logs → hash matches → release.
        assert_eq!(h.screen_tx(&input), Screen::AuditApproved);
        assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::ReleasePending));
    }

    /// FR-7 §6.6 consistency mismatch (G2): approved but the pre-package re-simulation yields
    /// a different quota → hard drop, no RCS cancel, never re-admitted.
    #[test]
    fn approved_consistency_mismatch_hard_drops() {
        let (h, _clock) = handle_and_clock(1_751_000_000);
        let submit_logs = vec![transfer_log(golden::ONE_TOKEN)];
        let input_submit = audit_input(golden::tx_a(), 1, 1_000_000, &submit_logs);

        assert_eq!(h.screen_tx(&input_submit), Screen::AuditPending);
        let now = 1_751_000_000;
        h.with_pool(|p| {
            p.apply_submit_response(&[format!("{:#x}", golden::tx_a())], now);
            p.apply_query_status(&golden::tx_a(), "approved", now);
        });

        // Re-simulate with a different amount → quota hash mismatch → drop.
        let repackage_logs = vec![transfer_log(golden::TWO_TOKENS)];
        let input_repackage = audit_input(golden::tx_a(), 1, 1_000_000, &repackage_logs);
        assert_eq!(h.screen_tx(&input_repackage), Screen::AuditPending);
        assert_eq!(h.buffer_status(&golden::tx_a()), Some(BufferStatus::Dropped));
        // Never re-admitted / never released.
        assert_eq!(h.screen_tx(&input_repackage), Screen::AuditPending);
    }
}
