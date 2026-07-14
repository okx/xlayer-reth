//! Buffer pool + audit state machine (FR-5/FR-6, TD §4.7).
//!
//! `bufferPool[tx_hash]` is a Filter-local side table (distinct from the RCS-facing
//! `status` in contract §2.5). The transaction entity itself stays in the tx pool; this
//! table only tracks adjudication progress across block-building rounds.
//!
//! Terminal outcomes do **not** remove the entry — they transition it to a builder-visible
//! **tombstone** (`ReleasePending` = release into the block, `Dropped` = discard). Removing
//! the entry would let the builder re-screen the tx from scratch on the next round, resetting
//! `first_not_submitted_at` (timeout clock) and re-submitting a denied/outdated tx — an
//! infinite loop. The tombstone gives `screen_tx` a deterministic mapping
//! (`ReleasePending → Screen::AuditApproved`, `Dropped → Screen::Drop` for tx-pool eviction)
//! and keeps the tx out of the submit / query / timeout scans for the rest of
//! the build cycle. To bound memory over the node's (unbounded) lifetime, tombstones are
//! evicted by [`BufferPool::prune_terminal`] once they have been terminal for longer than
//! `terminal_entry_retention_seconds` (contract §2.5) — long enough that a still-relevant tx
//! is never re-screened during its mempool lifetime, short enough that the table stays finite.

use std::collections::{BTreeMap, HashMap};

use alloy_primitives::{Address, B256};

use crate::client::ActionItem;
use crate::config::FilterConfig;
use crate::rules::TimeoutAction;

/// In-memory audit state (TD §4.7). `ReleasePending` and `Dropped` are **terminal
/// tombstones** kept in the pool (not removed) so the outcome is visible to the builder's
/// `screen_tx` on the next round without re-screening from scratch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferStatus {
    NotSubmitted,
    Submitted,
    Pending,
    Approved,
    /// Terminal: release the tx into the block (approved + consistency-pass, or fail-open
    /// timeout). `screen_tx` maps this to `Screen::AuditApproved`.
    ReleasePending,
    /// Terminal: discard the tx (denied / outdated / consistency-mismatch / fail-close
    /// timeout). Never packaged, never re-submitted, never re-admitted this build cycle.
    Dropped,
}

impl BufferStatus {
    /// Whether this is a terminal tombstone (excluded from submit/query/timeout scans and
    /// from timeout re-resolution).
    pub fn is_terminal(self) -> bool {
        matches!(self, BufferStatus::ReleasePending | BufferStatus::Dropped)
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

/// One buffered audit transaction.
#[derive(Debug, Clone)]
pub struct BufferEntry {
    pub tx_hash: B256,
    pub origin: Address,
    /// `tx.to` — submitted verbatim as `contract_address` (observational, contract §2.4).
    pub contract_address: Address,
    pub nonce: u64,
    pub block_height: u64,
    pub status: BufferStatus,
    /// The exact `actions` payload submitted (snapshot), reused for the submit request.
    pub actions: BTreeMap<String, Vec<ActionItem>>,
    /// keccak256 of the canonical `actions.quota` computed at submit time (FR-7).
    pub quota_consistency_hash: B256,
    /// Merged fallback action across matched audit rules (FR-6).
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
}

impl BufferPool {
    /// Returns the entry for `tx_hash`, if any.
    pub fn get(&self, tx_hash: &B256) -> Option<&BufferEntry> {
        self.entries.get(tx_hash)
    }

    /// Whether an entry exists (including a terminal tombstone). Presence drives the dedup
    /// short-circuit (FR-4 stage zero); `screen_tx` inspects the status to decide the reuse.
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
    pub fn insert(&mut self, entry: BufferEntry) {
        self.entries.entry(entry.tx_hash).or_insert(entry);
    }

    /// Removes and returns an entry.
    pub fn remove(&mut self, tx_hash: &B256) -> Option<BufferEntry> {
        self.entries.remove(tx_hash)
    }

    /// Completes the pre-package consistency check if the entry is still `Approved`.
    ///
    /// Query and timeout workers can resolve an approved entry while the builder recomputes its
    /// consistency hash without holding the pool lock. Re-checking the status here prevents that
    /// stale computation from overwriting a newer terminal decision such as `Dropped`.
    pub fn finish_consistency_check(
        &mut self,
        tx_hash: &B256,
        consistent: bool,
        now: u64,
    ) -> Option<BufferStatus> {
        let entry = self.entries.get_mut(tx_hash)?;
        if entry.status == BufferStatus::Approved {
            entry.status =
                if consistent { BufferStatus::ReleasePending } else { BufferStatus::Dropped };
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

    /// Applies a `202` submit response: `accepted` hashes in `NotSubmitted` advance to
    /// `Submitted`; a hash only in `rejected_malformed` stays `NotSubmitted` for retry
    /// (FR-5, contract §2.4).
    pub fn apply_submit_response(&mut self, accepted: &[String], now: u64) {
        for hash in accepted {
            if let Some(entry) = parse_hash(hash).and_then(|h| self.entries.get_mut(&h))
                && entry.status == BufferStatus::NotSubmitted
            {
                entry.status = BufferStatus::Submitted;
                entry.last_transition_at = now;
            }
        }
    }

    /// Applies an RCS query `status` for `tx_hash` (contract §2.5 → TD §4.7). `denied` /
    /// `outdated` tombstone the entry as `Dropped` in place (G3: no removal → no re-buffer /
    /// re-submit) and return `Some(Resolution::Discard)` for logging. Unrecognized/absent
    /// statuses are left to [`Self::check_timeouts`] (no optimistic pass). Terminal entries
    /// are ignored.
    pub fn apply_query_status(
        &mut self,
        tx_hash: &B256,
        status: &str,
        now: u64,
    ) -> Option<Resolution> {
        let entry = self.entries.get_mut(tx_hash)?;
        if entry.status.is_terminal() {
            return None;
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
            // local discard). Kept in place so the tx is not re-submitted (G3).
            "denied" | "outdated" => {
                entry.status = BufferStatus::Dropped;
                entry.last_transition_at = now;
                Some(Resolution::Discard)
            }
            // Unrecognized status → no terminal state; fall through to FR-6 timeout.
            _ => None,
        }
    }

    /// Advances timeout-driven transitions (FR-6). Outer fallback (cumulative > total
    /// retry timeout) is evaluated **before** the regular per-state stalls (TD §4.7).
    /// Terminal tombstones are skipped (no re-resolution, no clock reset). On outer-fallback
    /// the entry is tombstoned in place (`ReleasePending` for fail-open, `Dropped` for
    /// fail-close) — not removed — and the resolution is returned for logging.
    pub fn check_timeouts(&mut self, cfg: &FilterConfig, now: u64) -> Vec<(B256, Resolution)> {
        let mut terminal = Vec::new();

        for entry in self.entries.values_mut() {
            // Skip already-terminal tombstones.
            if entry.status.is_terminal() {
                continue;
            }

            // Outer fallback first.
            let cumulative = now.saturating_sub(entry.first_not_submitted_at);
            if cumulative > cfg.total_retry_timeout.as_secs() {
                let (resolution, status) = match entry.timeout_action {
                    TimeoutAction::Allow => {
                        (Resolution::ReleaseForPackaging, BufferStatus::ReleasePending)
                    }
                    TimeoutAction::Deny => (Resolution::Discard, BufferStatus::Dropped),
                };
                entry.status = status;
                entry.last_transition_at = now;
                terminal.push((entry.tx_hash, resolution));
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

    /// Evicts terminal tombstones (`ReleasePending`/`Dropped`) that have been terminal for
    /// longer than `retention` seconds, bounding pool memory over the node's lifetime
    /// (contract §2.5 `terminal_entry_retention_seconds`). Non-terminal entries are never
    /// pruned — the FR-6 outer timeout guarantees every entry reaches a terminal tombstone
    /// within `total_retry_timeout`, so pruning terminal entries alone bounds the pool.
    /// Retention exceeds `total_retry_timeout` so a tombstone is not evicted while a still-live
    /// duplicate of the tx might be re-screened within the same adjudication window; a tx still
    /// in the mempool after eviction is simply re-screened (and, if it re-matches, re-submitted —
    /// RCS is idempotent), trading a rare bounded re-submit for bounded memory. Returns the
    /// number of entries evicted.
    pub fn prune_terminal(&mut self, retention_secs: u64, now: u64) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, e| {
            !(e.status.is_terminal() && now.saturating_sub(e.last_transition_at) > retention_secs)
        });
        before - self.entries.len()
    }
}

/// Parses a `0x`-prefixed 64-hex tx hash string into a [`B256`].
fn parse_hash(s: &str) -> Option<B256> {
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn entry(now: u64, timeout_action: TimeoutAction) -> BufferEntry {
        BufferEntry {
            tx_hash: B256::from_str(
                "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
            origin: Address::ZERO,
            contract_address: Address::ZERO,
            nonce: 1,
            block_height: 1_000_000,
            status: BufferStatus::NotSubmitted,
            actions: BTreeMap::new(),
            quota_consistency_hash: B256::ZERO,
            timeout_action,
            first_not_submitted_at: now,
            last_transition_at: now,
        }
    }

    #[test]
    fn submit_response_advances_only_accepted() {
        let mut pool = BufferPool::default();
        pool.insert(entry(0, TimeoutAction::Allow));
        let hash = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        pool.apply_submit_response(&[hash.to_string()], 1);
        assert_eq!(pool.get(&parse_hash(hash).unwrap()).unwrap().status, BufferStatus::Submitted);
    }

    #[test]
    fn query_denied_and_outdated_tombstone_as_dropped() {
        let hash =
            B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        for status in ["denied", "outdated"] {
            let mut pool = BufferPool::default();
            let mut e = entry(0, TimeoutAction::Allow);
            e.status = BufferStatus::Pending;
            pool.insert(e);
            assert_eq!(pool.apply_query_status(&hash, status, 5), Some(Resolution::Discard));
            // G3: tombstoned in place (not removed) so it is not re-submitted.
            assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Dropped);
            assert!(pool.not_submitted().is_empty());
            assert!(pool.in_flight_hashes().is_empty());
            // A second query on a terminal entry is a no-op.
            assert_eq!(pool.apply_query_status(&hash, "denied", 6), None);
        }
    }

    #[test]
    fn consistency_check_only_transitions_an_approved_entry() {
        let hash =
            B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();

        let mut pool = BufferPool::default();
        let mut approved = entry(0, TimeoutAction::Allow);
        approved.status = BufferStatus::Approved;
        pool.insert(approved);

        assert_eq!(
            pool.finish_consistency_check(&hash, true, 5),
            Some(BufferStatus::ReleasePending)
        );
        assert_eq!(pool.get(&hash).unwrap().last_transition_at, 5);

        let mut pool = BufferPool::default();
        let mut outdated = entry(0, TimeoutAction::Allow);
        outdated.status = BufferStatus::Approved;
        pool.insert(outdated);
        assert_eq!(pool.apply_query_status(&hash, "outdated", 6), Some(Resolution::Discard));

        assert_eq!(pool.finish_consistency_check(&hash, true, 7), Some(BufferStatus::Dropped));
        assert_eq!(pool.get(&hash).unwrap().last_transition_at, 6);
    }

    #[test]
    fn inconsistent_approved_entry_is_dropped() {
        let hash =
            B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        let mut pool = BufferPool::default();
        let mut approved = entry(0, TimeoutAction::Allow);
        approved.status = BufferStatus::Approved;
        pool.insert(approved);

        assert_eq!(pool.finish_consistency_check(&hash, false, 5), Some(BufferStatus::Dropped));
    }

    #[test]
    fn unrecognized_status_is_not_terminal() {
        let hash =
            B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        let mut pool = BufferPool::default();
        let mut e = entry(0, TimeoutAction::Allow);
        e.status = BufferStatus::Pending;
        pool.insert(e);
        assert_eq!(pool.apply_query_status(&hash, "weird", 5), None);
    }

    #[test]
    fn outer_timeout_boundary_is_strict_and_tombstones_release() {
        let cfg = FilterConfig::default(); // total_retry 90s
        let hash =
            B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        // Exactly 90s → not triggered.
        let mut pool = BufferPool::default();
        pool.insert(entry(0, TimeoutAction::Allow));
        assert!(pool.check_timeouts(&cfg, 90).is_empty());
        assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::NotSubmitted);
        // 91s → fail-open release, tombstoned in place (G1), NOT removed.
        let out = pool.check_timeouts(&cfg, 91);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, Resolution::ReleaseForPackaging);
        assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::ReleasePending);
        // Re-tick does not re-resolve a terminal entry (no clock reset, no loop).
        assert!(pool.check_timeouts(&cfg, 200).is_empty());
    }

    #[test]
    fn outer_timeout_fail_close_tombstones_dropped() {
        let cfg = FilterConfig::default();
        let hash =
            B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        let mut pool = BufferPool::default();
        pool.insert(entry(0, TimeoutAction::Deny));
        let out = pool.check_timeouts(&cfg, 91);
        assert_eq!(out[0].1, Resolution::Discard);
        assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Dropped);
    }

    #[test]
    fn prune_terminal_evicts_only_expired_tombstones() {
        let a =
            B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        let b =
            B256::from_str("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap();
        let mut pool = BufferPool::default();
        // Terminal tombstone that last transitioned at t=100.
        let mut term = entry(100, TimeoutAction::Allow);
        term.status = BufferStatus::Dropped;
        term.last_transition_at = 100;
        pool.insert(term);
        // Non-terminal entry (never pruned regardless of age).
        let mut live = entry(0, TimeoutAction::Allow); // NotSubmitted
        live.tx_hash = b;
        live.last_transition_at = 0;
        pool.insert(live);

        let retention = 300;
        // Boundary is strict: exactly `retention` old → kept.
        assert_eq!(pool.prune_terminal(retention, 100 + 300), 0);
        assert_eq!(pool.len(), 2);
        // Older than `retention` → the terminal tombstone is evicted; the live entry stays.
        assert_eq!(pool.prune_terminal(retention, 100 + 301), 1);
        assert!(pool.get(&a).is_none());
        assert!(pool.get(&b).is_some());
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn submitted_stall_reverts_to_not_submitted() {
        let cfg = FilterConfig::default(); // 8s
        let mut pool = BufferPool::default();
        let mut e = entry(0, TimeoutAction::Allow);
        e.status = BufferStatus::Submitted;
        e.last_transition_at = 0;
        pool.insert(e);
        // 8s exactly → no revert; 9s → revert.
        pool.check_timeouts(&cfg, 8);
        let h = parse_hash("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
        assert_eq!(pool.get(&h).unwrap().status, BufferStatus::Submitted);
        pool.check_timeouts(&cfg, 9);
        assert_eq!(pool.get(&h).unwrap().status, BufferStatus::NotSubmitted);
    }
}
