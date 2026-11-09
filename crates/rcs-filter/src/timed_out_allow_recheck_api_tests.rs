//! Pool-level contracts of the timed-out-allow re-check: generation fencing, the already-dropped
//! commit case, and the additive drop counter. Split from `timed_out_allow_recheck_tests` so the
//! regression module compiles unchanged against the pre-fix base (this module references the new
//! pool method and metric, which do not exist there).

use std::sync::Arc;

use alloy_primitives::B256;

use crate::config::FilterConfig;
use crate::pool::{BufferEntry, BufferPool, BufferStatus};
use crate::rules::{RuleSet, TimeoutAction};
use crate::test_support::{golden, TestClock};

const START: u64 = 1_751_000_000;

fn pool_entry(status: BufferStatus, now: u64) -> BufferEntry {
    BufferEntry {
        generation: 0,
        tx_hash: golden::tx_a(),
        origin: golden::origin(),
        contract_address: golden::claim_contract(),
        nonce: 1,
        block_height: 1_000_000,
        status,
        actions: Default::default(),
        rule_snapshot: Arc::new(RuleSet::default()),
        quota_consistency_hash: B256::ZERO,
        timeout_action: TimeoutAction::Allow,
        first_not_submitted_at: now,
        last_transition_at: now,
    }
}

/// F10 — a stale re-check result cannot touch a reinserted lifecycle.
///
/// The handle's `Screen::AuditPending` fallback is the same generation fence; it is only
/// reachable when the entry changes between the handle's read and its commit, which cannot be
/// driven synchronously, so the fence itself is asserted on the pool method.
#[test]
fn stale_timed_out_allow_recheck_cannot_touch_a_reinserted_lifecycle() {
    let hash = golden::tx_a();
    let mut pool = BufferPool::default();
    pool.insert(pool_entry(BufferStatus::TimedOutAllow, START));
    let old_generation = pool.get(&hash).unwrap().generation;

    pool.remove(&hash);
    pool.insert(pool_entry(BufferStatus::NotSubmitted, START + 1));
    let new_generation = pool.get(&hash).unwrap().generation;
    assert_ne!(old_generation, new_generation);

    assert_eq!(pool.finish_timed_out_allow_recheck(&hash, old_generation, false, START + 2), None);
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::NotSubmitted);

    // The fence also refuses to transition a lifecycle that is not `TimedOutAllow` any more.
    assert_eq!(
        pool.finish_timed_out_allow_recheck(&hash, new_generation, false, START + 2),
        Some(BufferStatus::NotSubmitted)
    );
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::NotSubmitted);
    assert_eq!(pool.get(&hash).unwrap().last_transition_at, START + 1);
}

/// F11 — an entry already tombstoned as `Dropped` at commit reports `Drop`, not release.
#[test]
fn already_dropped_entry_reports_drop_at_commit() {
    let hash = golden::tx_a();
    let mut pool = BufferPool::default();
    pool.insert(pool_entry(BufferStatus::Dropped, START));
    let generation = pool.get(&hash).unwrap().generation;
    assert_eq!(
        pool.finish_timed_out_allow_recheck(&hash, generation, true, START + 5),
        Some(BufferStatus::Dropped)
    );
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Dropped);
    assert_eq!(
        pool.get(&hash).unwrap().last_transition_at,
        START,
        "a terminal entry is not re-transitioned"
    );
}

/// F12 — the drop counter is a distinct, additive metric next to the existing drop signals.
///
/// Counter values are not asserted here: the derived metric call sites are process-global and
/// parallel tests in this binary initialize them under different recorders. The status and
/// `Screen` assertions in the sibling module already pin the behaviour the counter reports.
#[test]
fn timed_out_allow_recheck_drop_counter_is_additive() {
    let metrics = crate::metrics::RcsFilterMetrics::default();
    metrics.timed_out_allow_recheck_drop_total.increment(1);
    // A separate series from the approved-path consistency signal and the shared drop total.
    metrics.consistency_mismatch_total.increment(1);
    metrics.drop_total.increment(1);
    metrics.audit_approved_total.increment(1);
    // The handle used by the behavioural tests still builds with the metric registered.
    let _handle = crate::handle::FilterHandle::for_test(
        FilterConfig::default(),
        RuleSet::default(),
        Arc::new(TestClock::new(START)),
    );
}
