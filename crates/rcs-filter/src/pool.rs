//! Buffer pool and audit state machine.
//!
//! `bufferPool[tx_hash]` is a Filter-local side table (distinct from the RCS-facing
//! `status`). It tracks adjudication progress across block-building rounds;
//! terminal discard outcomes are also applied to the builder transaction pool.
//!
//! Terminal outcomes do **not** remove the entry — they transition it to a builder-visible
//! **tombstone** (`TimedOutAllow` = fail-open release, `Dropped` = discard). Removing
//! the entry would let the builder re-screen the tx from scratch on the next round, resetting
//! `first_not_submitted_at` (timeout clock) and re-submitting a denied/outdated tx — an
//! infinite loop. The tombstone gives `screen_tx` a deterministic mapping
//! (`TimedOutAllow → Screen::AuditApproved` unless the local re-check on that execution denies or
//! cannot decide, in which case the lifecycle becomes `Dropped`; `Dropped → Screen::Drop` for
//! tx-pool eviction) and keeps the tx out of the submit / query / timeout scans for the rest of
//! the build cycle. To bound memory over the node's (unbounded) lifetime, tombstones are
//! evicted by [`BufferPool::prune_terminal`] once they have been terminal for longer than
//! `terminal_entry_retention_seconds`. Non-terminal state is removed when the
//! exact transaction generation is no longer present in txpool.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use alloy_primitives::{Address, B256};

use crate::client::ActionItem;
use crate::config::FilterConfig;
use crate::rules::{RuleSet, TimeoutAction};

/// In-memory audit state. `TimedOutAllow` and `Dropped` are **terminal
/// tombstones** kept in the pool (not removed) so the outcome is visible to the builder's
/// `screen_tx` on the next round without re-screening from scratch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferStatus {
    NotSubmitted,
    Submitted,
    Pending,
    Approved,
    /// Terminal fail-open timeout (the audit service never decided). A successful consistency
    /// check does not enter this state: approval remains attempt-scoped until canonical inclusion.
    /// Releasing it still requires the local matcher to decide allow/audit on the current
    /// execution; a local deny (or an undecidable match) turns the lifecycle into `Dropped`.
    TimedOutAllow,
    /// Terminal: discard the tx (denied / outdated / consistency-mismatch / fail-close
    /// timeout). Never packaged, never re-submitted, never re-admitted this build cycle.
    Dropped,
}

impl BufferStatus {
    /// Whether this is a terminal tombstone (excluded from submit/query/timeout scans and
    /// from timeout re-resolution).
    pub fn is_terminal(self) -> bool {
        matches!(self, BufferStatus::TimedOutAllow | BufferStatus::Dropped)
    }
}

/// Terminal outcome for a buffered audit transaction (used to log the resolution; the entry
/// is tombstoned in place rather than removed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Approved (+ consistency check passes at package time) or fail-open timeout → release.
    ReleaseForPackaging,
    /// Denied / outdated / consistency-mismatch / fail-close timeout → drop the tx.
    Discard,
}

/// Why applying a query response produced a terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryResolution {
    /// The cumulative retry deadline won before the query result was applied.
    RetryTimeout(Resolution),
    /// RCS returned `denied` before the cumulative retry deadline.
    Denied,
    /// RCS returned `outdated` before the cumulative retry deadline.
    Outdated,
}

/// One buffered audit transaction.
#[derive(Debug, Clone)]
pub struct BufferEntry {
    /// Monotonic insertion identity. This prevents a consistency result computed for an older
    /// lifecycle from being applied after canonical cleanup and reinsertion of the same hash.
    pub generation: u64,
    pub tx_hash: B256,
    pub origin: Address,
    /// `tx.to` — submitted verbatim as `contract_address` for observation.
    pub contract_address: Address,
    pub nonce: u64,
    pub block_height: u64,
    pub status: BufferStatus,
    /// The exact `actions` payload submitted (snapshot), reused for the submit request.
    pub actions: BTreeMap<String, Vec<ActionItem>>,
    /// The exact rule snapshot used to produce `actions`. Approved transactions are re-evaluated
    /// against this snapshot so a hot reload cannot change an in-flight decision.
    pub rule_snapshot: Arc<RuleSet>,
    /// keccak256 of the canonical `actions.quota` computed at submit time.
    pub quota_consistency_hash: B256,
    /// Merged fallback action across matched audit rules.
    pub timeout_action: TimeoutAction,
    /// Unix seconds when the entry first entered `NotSubmitted` (cumulative timeout base).
    pub first_not_submitted_at: u64,
    /// Unix seconds of the last state transition (used for 8s/20s stall detection).
    pub last_transition_at: u64,
}

/// The buffer pool: a `tx_hash → entry` side table.
#[derive(Debug, Default)]
pub struct BufferPool {
    entries: HashMap<B256, BufferEntry>,
    next_generation: u64,
}

impl BufferPool {
    /// Counts entries by state for metrics without exposing the backing map.
    pub fn status_counts(&self) -> [usize; 6] {
        let mut counts = [0usize; 6];
        for entry in self.entries.values() {
            let index = match entry.status {
                BufferStatus::NotSubmitted => 0,
                BufferStatus::Submitted => 1,
                BufferStatus::Pending => 2,
                BufferStatus::Approved => 3,
                BufferStatus::TimedOutAllow => 4,
                BufferStatus::Dropped => 5,
            };
            counts[index] += 1;
        }
        counts
    }

    /// Returns the entry for `tx_hash`, if any.
    pub fn get(&self, tx_hash: &B256) -> Option<&BufferEntry> {
        self.entries.get(tx_hash)
    }

    /// Returns true when `tx_hash` still refers to the exact insertion lifecycle and status.
    pub fn matches(&self, tx_hash: &B256, generation: u64, status: BufferStatus) -> bool {
        self.entries
            .get(tx_hash)
            .is_some_and(|entry| entry.generation == generation && entry.status == status)
    }

    /// Whether an entry exists (including a terminal tombstone). Presence drives the dedup
    /// short-circuit; `screen_tx` inspects the status to decide the reuse.
    pub fn contains(&self, tx_hash: &B256) -> bool {
        self.entries.contains_key(tx_hash)
    }

    /// Number of buffered entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Inserts a freshly-matched audit tx in `NotSubmitted` (no-op if already present).
    pub fn insert(&mut self, mut entry: BufferEntry) {
        if self.entries.contains_key(&entry.tx_hash) {
            return;
        }
        self.next_generation = self.next_generation.wrapping_add(1);
        entry.generation = self.next_generation;
        self.entries.insert(entry.tx_hash, entry);
    }

    /// Removes and returns an entry.
    pub fn remove(&mut self, tx_hash: &B256) -> Option<BufferEntry> {
        self.entries.remove(tx_hash)
    }

    /// Applies a pre-package consistency result if the entry is still `Approved`.
    ///
    /// Query and timeout workers can resolve an approved entry while the builder recomputes its
    /// consistency hash without holding the pool lock. Re-checking the status here prevents that
    /// stale computation from overwriting a newer terminal decision such as `Dropped`.
    pub fn finish_consistency_check(
        &mut self,
        tx_hash: &B256,
        expected_generation: u64,
        consistent: bool,
        now: u64,
    ) -> Option<BufferStatus> {
        let entry = self.entries.get_mut(tx_hash)?;
        if entry.generation != expected_generation {
            return None;
        }
        if entry.status == BufferStatus::Approved && !consistent {
            entry.status = BufferStatus::Dropped;
            entry.last_transition_at = now;
        }
        Some(entry.status)
    }

    /// Applies a `TimedOutAllow` local-deny re-check if the entry is still the same lifecycle.
    ///
    /// The builder recomputes the re-check without holding the pool lock; re-checking generation
    /// here prevents a stale result from touching a reinserted lifecycle. `release == false` (a
    /// local deny or an undecidable match) tombstones the entry as `Dropped`; the cumulative
    /// timeout clock is untouched, and the entry is never removed, re-submitted or re-buffered.
    pub fn finish_timed_out_allow_recheck(
        &mut self,
        tx_hash: &B256,
        expected_generation: u64,
        release: bool,
        now: u64,
    ) -> Option<BufferStatus> {
        let entry = self.entries.get_mut(tx_hash)?;
        if entry.generation != expected_generation {
            return None;
        }
        if entry.status == BufferStatus::TimedOutAllow && !release {
            entry.status = BufferStatus::Dropped;
            entry.last_transition_at = now;
        }
        Some(entry.status)
    }

    /// tx_hashes currently in `NotSubmitted` (batch-submit candidates).
    pub fn not_submitted(&self) -> Vec<B256> {
        self.entries
            .values()
            .filter(|e| e.status == BufferStatus::NotSubmitted)
            .map(|e| e.tx_hash)
            .collect()
    }

    /// Hex tx_hashes of entries already in flight at RCS (`Submitted`/`Pending`/`Approved`),
    /// used to drive the adjudication query.
    pub fn in_flight_hashes(&self) -> Vec<String> {
        self.entries
            .values()
            .filter(|e| {
                matches!(
                    e.status,
                    BufferStatus::Submitted | BufferStatus::Pending | BufferStatus::Approved
                )
            })
            .map(|e| format!("{:#x}", e.tx_hash))
            .collect()
    }

    /// Lifecycle snapshot used to fence status-query responses across network awaits.
    pub fn in_flight_generations(&self) -> HashMap<B256, u64> {
        self.entries
            .values()
            .filter(|entry| {
                matches!(
                    entry.status,
                    BufferStatus::Submitted | BufferStatus::Pending | BufferStatus::Approved
                )
            })
            .map(|entry| (entry.tx_hash, entry.generation))
            .collect()
    }

    /// Removes entries for transactions observed in a newly canonical chain segment.
    pub fn remove_canonical(&mut self, hashes: &[B256]) -> usize {
        hashes.iter().filter(|hash| self.entries.remove(*hash).is_some()).count()
    }

    /// Conservatively invalidates reusable state after canonical notifications were lost.
    /// Dropped tombstones remain fail-closed; every other lifecycle must be re-screened.
    pub fn recover_after_canonical_lag(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, entry| entry.status == BufferStatus::Dropped);
        before - self.entries.len()
    }

    /// Snapshot of terminally dropped transaction hashes for txpool reconciliation.
    pub fn dropped_hashes(&self) -> Vec<B256> {
        self.entries
            .values()
            .filter(|entry| entry.status == BufferStatus::Dropped)
            .map(|entry| entry.tx_hash)
            .collect()
    }

    /// Current dropped lifecycle tokens for builder-side reconciliation.
    pub fn dropped_lifecycles(&self) -> Vec<(B256, u64)> {
        self.entries
            .values()
            .filter(|entry| entry.status == BufferStatus::Dropped)
            .map(|entry| (entry.tx_hash, entry.generation))
            .collect()
    }

    /// Snapshot of non-terminal lifecycle tokens for builder-side txpool reconciliation.
    pub fn non_terminal_lifecycles(&self) -> Vec<(B256, u64)> {
        self.entries
            .values()
            .filter(|entry| !entry.status.is_terminal())
            .map(|entry| (entry.tx_hash, entry.generation))
            .collect()
    }

    /// Removes the exact non-terminal lifecycle, if it is still current.
    pub fn remove_non_terminal_if_generation(
        &mut self,
        tx_hash: &B256,
        expected_generation: u64,
    ) -> bool {
        let should_remove = self.entries.get(tx_hash).is_some_and(|entry| {
            entry.generation == expected_generation && !entry.status.is_terminal()
        });
        if should_remove {
            self.entries.remove(tx_hash);
        }
        should_remove
    }

    /// Applies a `202` submit response: `accepted` hashes in `NotSubmitted` advance to
    /// `Submitted`; a hash only in `rejected_malformed` stays `NotSubmitted` for a later retry.
    pub fn apply_submit_response(
        &mut self,
        accepted: &[String],
        expected_generations: &HashMap<B256, u64>,
        cfg: &FilterConfig,
        now: u64,
    ) -> Vec<(B256, u64, Resolution)> {
        let accepted = accepted
            .iter()
            .filter_map(|hash| parse_hash(hash))
            .collect::<std::collections::HashSet<_>>();
        let mut resolved = Vec::new();

        for (hash, expected_generation) in expected_generations {
            let Some(entry) = self.entries.get_mut(hash) else { continue };
            if entry.generation != *expected_generation {
                continue;
            }
            if let Some(resolution) = resolve_total_timeout(entry, cfg, now) {
                resolved.push((*hash, *expected_generation, resolution));
                continue;
            }
            if accepted.contains(hash) && entry.status == BufferStatus::NotSubmitted {
                entry.status = BufferStatus::Submitted;
                entry.last_transition_at = now;
            }
        }
        resolved
    }

    /// Applies an RCS query `status` for `tx_hash`. `denied` /
    /// `outdated` tombstone the entry as `Dropped` in place (no removal, re-buffer, or re-submit)
    /// and return `Some(Resolution::Discard)` for logging. Unrecognized/absent
    /// statuses are left to [`Self::check_timeouts`] (no optimistic pass). Terminal entries
    /// are ignored.
    pub fn apply_query_status(
        &mut self,
        tx_hash: &B256,
        expected_generation: u64,
        status: &str,
        cfg: &FilterConfig,
        now: u64,
    ) -> Option<QueryResolution> {
        let entry = self.entries.get_mut(tx_hash)?;
        if entry.generation != expected_generation || entry.status.is_terminal() {
            return None;
        }
        if let Some(resolution) = resolve_total_timeout(entry, cfg, now) {
            return Some(QueryResolution::RetryTimeout(resolution));
        }
        match status {
            "pending" => {
                if entry.status == BufferStatus::Submitted {
                    entry.status = BufferStatus::Pending;
                    entry.last_transition_at = now;
                }
                None
            }
            "approved" => {
                if matches!(entry.status, BufferStatus::Submitted | BufferStatus::Pending) {
                    entry.status = BufferStatus::Approved;
                    entry.last_transition_at = now;
                }
                None
            }
            // denied / outdated → tombstone as Dropped (RCS `denied`/`outdated` both map to
            // local discard). Kept in place so the tx is not re-submitted.
            "denied" | "outdated" => {
                entry.status = BufferStatus::Dropped;
                entry.last_transition_at = now;
                Some(if status == "outdated" {
                    QueryResolution::Outdated
                } else {
                    QueryResolution::Denied
                })
            }
            // Unrecognized status → no terminal state; fall through to timeout handling.
            _ => None,
        }
    }

    /// Resolves every retryable entry whose cumulative deadline has elapsed.
    ///
    /// This is run before submit/query network work so expired entries are not sent, and again
    /// while applying each response to close the network-await race.
    pub fn resolve_total_timeouts(
        &mut self,
        cfg: &FilterConfig,
        now: u64,
    ) -> Vec<(B256, u64, Resolution)> {
        let mut resolved = Vec::new();
        for entry in self.entries.values_mut() {
            if let Some(resolution) = resolve_total_timeout(entry, cfg, now) {
                resolved.push((entry.tx_hash, entry.generation, resolution));
            }
        }
        resolved
    }

    /// Advances timeout-driven transitions. Outer fallback (cumulative > total retry timeout)
    /// is evaluated **before** the regular per-state stalls.
    /// Terminal tombstones are skipped (no re-resolution, no clock reset). On outer-fallback
    /// the entry is tombstoned in place (`TimedOutAllow` for fail-open, `Dropped` for
    /// fail-close) — not removed — and the resolution is returned for logging.
    pub fn check_timeouts(&mut self, cfg: &FilterConfig, now: u64) -> Vec<(B256, u64, Resolution)> {
        let mut terminal = Vec::new();

        for entry in self.entries.values_mut() {
            if let Some(resolution) = resolve_total_timeout(entry, cfg, now) {
                terminal.push((entry.tx_hash, entry.generation, resolution));
                continue;
            }

            // Skip terminal tombstones. Approved is intentionally not terminal, but it is neither
            // eligible for cumulative retry fallback nor for the Submitted/Pending stall paths.
            if entry.status.is_terminal() {
                continue;
            }

            // Regular per-state stalls.
            let since = now.saturating_sub(entry.last_transition_at);
            match entry.status {
                BufferStatus::Submitted if since > cfg.submitted_confirmation_timeout.as_secs() => {
                    entry.status = BufferStatus::NotSubmitted;
                    entry.last_transition_at = now;
                }
                BufferStatus::Pending if since > cfg.risk_module_unresponsive_timeout.as_secs() => {
                    entry.status = BufferStatus::NotSubmitted;
                    entry.last_transition_at = now;
                }
                _ => {}
            }
        }

        terminal
    }

    /// Evicts terminal tombstones (`TimedOutAllow`/`Dropped`) that have been terminal for
    /// longer than `retention` seconds, bounding pool memory over the node's lifetime
    /// (`terminal_entry_retention_seconds`). Non-terminal entries are not pruned by age: retryable
    /// states use the outer timeout, while `Approved` remains subject to its
    /// RCS/consistency lifecycle and builder-side txpool-presence reconciliation.
    /// Retention exceeds `total_retry_timeout` so a tombstone is not evicted while a still-live
    /// duplicate of the tx might be re-screened within the same adjudication window; a tx still
    /// in the mempool after eviction is simply re-screened (and, if it re-matches, re-submitted —
    /// RCS is idempotent), trading a rare bounded re-submit for bounded memory. Returns the
    /// number of entries evicted. Non-terminal memory is bounded separately by builder-side
    /// txpool-presence reconciliation.
    pub fn prune_terminal(&mut self, retention_secs: u64, now: u64) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, e| {
            !(e.status.is_terminal() && now.saturating_sub(e.last_transition_at) > retention_secs)
        });
        before - self.entries.len()
    }
}

/// Applies the cumulative fallback only while RCS has not produced an approval decision.
fn resolve_total_timeout(
    entry: &mut BufferEntry,
    cfg: &FilterConfig,
    now: u64,
) -> Option<Resolution> {
    if !matches!(
        entry.status,
        BufferStatus::NotSubmitted | BufferStatus::Submitted | BufferStatus::Pending
    ) || now.saturating_sub(entry.first_not_submitted_at) <= cfg.total_retry_timeout.as_secs()
    {
        return None;
    }

    let (resolution, status) = match entry.timeout_action {
        TimeoutAction::Allow => (Resolution::ReleaseForPackaging, BufferStatus::TimedOutAllow),
        TimeoutAction::Deny => (Resolution::Discard, BufferStatus::Dropped),
    };
    entry.status = status;
    entry.last_transition_at = now;
    Some(resolution)
}

/// Parses a `0x`-prefixed 64-hex tx hash string into a [`B256`].
fn parse_hash(s: &str) -> Option<B256> {
    s.parse().ok()
}

#[path = "../tests/pool__test_helpers.rs"]
#[cfg(test)]
mod test_helpers;

#[path = "../tests/pool__tests.rs"]
#[cfg(test)]
mod tests;
