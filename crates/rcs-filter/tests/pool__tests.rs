use super::*;
use std::str::FromStr;

fn entry(now: u64, timeout_action: TimeoutAction) -> BufferEntry {
    BufferEntry {
        generation: 0,
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
        rule_snapshot: Arc::new(RuleSet::default()),
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
    pool.apply_submit_response_current(&[hash.to_string()], 1);
    assert_eq!(pool.get(&parse_hash(hash).unwrap()).unwrap().status, BufferStatus::Submitted);
}

#[test]
fn query_denied_and_outdated_tombstone_as_dropped() {
    let hash = B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    for status in ["denied", "outdated"] {
        let mut pool = BufferPool::default();
        let mut e = entry(0, TimeoutAction::Allow);
        e.status = BufferStatus::Pending;
        pool.insert(e);
        assert_eq!(
            pool.apply_query_status_current(&hash, status, 5),
            Some(if status == "outdated" {
                QueryResolution::Outdated
            } else {
                QueryResolution::Denied
            })
        );
        // Tombstoned in place (not removed) so it is not re-submitted.
        assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Dropped);
        assert!(pool.not_submitted().is_empty());
        assert!(pool.in_flight_hashes().is_empty());
        // A second query on a terminal entry is a no-op.
        assert_eq!(pool.apply_query_status_current(&hash, "denied", 6), None);
    }
}

#[test]
fn concurrent_outdated_beats_consistency_pass() {
    let hash = B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();

    let mut pool = BufferPool::default();
    let mut approved = entry(0, TimeoutAction::Allow);
    approved.status = BufferStatus::Approved;
    pool.insert(approved);

    let generation = pool.get(&hash).unwrap().generation;
    assert_eq!(
        pool.finish_consistency_check(&hash, generation, true, 5),
        Some(BufferStatus::Approved)
    );
    assert_eq!(pool.get(&hash).unwrap().last_transition_at, 0);

    let mut pool = BufferPool::default();
    let mut outdated = entry(0, TimeoutAction::Allow);
    outdated.status = BufferStatus::Approved;
    pool.insert(outdated);
    assert_eq!(
        pool.apply_query_status_current(&hash, "outdated", 6),
        Some(QueryResolution::Outdated)
    );

    let generation = pool.get(&hash).unwrap().generation;
    assert_eq!(
        pool.finish_consistency_check(&hash, generation, true, 7),
        Some(BufferStatus::Dropped)
    );
    assert_eq!(pool.get(&hash).unwrap().last_transition_at, 6);
}

#[test]
fn concurrent_pending_beats_consistency_pass() {
    let hash = B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    let mut pool = BufferPool::default();
    let mut pending = entry(0, TimeoutAction::Allow);
    pending.status = BufferStatus::Pending;
    pool.insert(pending);

    let generation = pool.get(&hash).unwrap().generation;
    assert_eq!(
        pool.finish_consistency_check(&hash, generation, true, 7),
        Some(BufferStatus::Pending)
    );
    assert_eq!(pool.get(&hash).unwrap().last_transition_at, 0);
}

#[test]
fn inconsistent_approved_entry_is_dropped() {
    let hash = B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    let mut pool = BufferPool::default();
    let mut approved = entry(0, TimeoutAction::Allow);
    approved.status = BufferStatus::Approved;
    pool.insert(approved);

    let generation = pool.get(&hash).unwrap().generation;
    assert_eq!(
        pool.finish_consistency_check(&hash, generation, false, 5),
        Some(BufferStatus::Dropped)
    );
}

#[test]
fn stale_consistency_result_cannot_mutate_reinserted_hash() {
    let hash = entry(0, TimeoutAction::Allow).tx_hash;
    let mut pool = BufferPool::default();
    let mut first = entry(0, TimeoutAction::Allow);
    first.status = BufferStatus::Approved;
    pool.insert(first);
    let old_generation = pool.get(&hash).unwrap().generation;

    pool.remove(&hash);
    let mut reinserted = entry(1, TimeoutAction::Allow);
    reinserted.status = BufferStatus::Approved;
    pool.insert(reinserted);
    let new_generation = pool.get(&hash).unwrap().generation;
    assert_ne!(old_generation, new_generation);

    assert_eq!(pool.finish_consistency_check(&hash, old_generation, true, 2), None);
    assert_eq!(pool.finish_consistency_check(&hash, old_generation, false, 2), None);
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Approved);
}

#[test]
fn stale_submit_and_query_responses_cannot_advance_reinserted_hash() {
    let hash = entry(0, TimeoutAction::Allow).tx_hash;
    let mut pool = BufferPool::default();
    pool.insert(entry(0, TimeoutAction::Allow));
    let old_generation = pool.get(&hash).unwrap().generation;
    let expected = HashMap::from([(hash, old_generation)]);

    pool.remove(&hash);
    pool.insert(entry(1, TimeoutAction::Allow));
    let new_generation = pool.get(&hash).unwrap().generation;
    assert_ne!(old_generation, new_generation);

    pool.apply_submit_response(&[format!("{hash:#x}")], &expected, &FilterConfig::default(), 2);
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::NotSubmitted);

    pool.apply_submit_response_current(&[format!("{hash:#x}")], 2);
    assert_eq!(
        pool.apply_query_status(&hash, old_generation, "approved", &FilterConfig::default(), 3,),
        None
    );
    assert_eq!(
        pool.apply_query_status(&hash, old_generation, "denied", &FilterConfig::default(), 3,),
        None
    );
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Submitted);
}

#[test]
fn canonical_lag_recovery_keeps_only_fail_closed_tombstones() {
    let mut pool = BufferPool::default();
    for (index, status) in [
        BufferStatus::NotSubmitted,
        BufferStatus::Submitted,
        BufferStatus::Pending,
        BufferStatus::Approved,
        BufferStatus::TimedOutAllow,
        BufferStatus::Dropped,
    ]
    .into_iter()
    .enumerate()
    {
        let mut item = entry(0, TimeoutAction::Allow);
        item.tx_hash = B256::with_last_byte(index as u8);
        item.status = status;
        pool.insert(item);
    }
    assert_eq!(pool.recover_after_canonical_lag(), 5);
    assert_eq!(pool.len(), 1);
    assert_eq!(pool.get(&B256::with_last_byte(5)).unwrap().status, BufferStatus::Dropped);
}

#[test]
fn unrecognized_status_is_not_terminal() {
    let hash = B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    let mut pool = BufferPool::default();
    let mut e = entry(0, TimeoutAction::Allow);
    e.status = BufferStatus::Pending;
    pool.insert(e);
    assert_eq!(pool.apply_query_status_current(&hash, "weird", 5), None);
}

#[test]
fn outer_timeout_boundary_is_strict_and_tombstones_release() {
    let cfg = FilterConfig::default(); // total_retry 90s
    let hash = B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    // Exactly 90s → not triggered.
    let mut pool = BufferPool::default();
    pool.insert(entry(0, TimeoutAction::Allow));
    assert!(pool.check_timeouts(&cfg, 90).is_empty());
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::NotSubmitted);
    // 91s → fail-open release, tombstoned in place, NOT removed.
    let out = pool.check_timeouts(&cfg, 91);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].2, Resolution::ReleaseForPackaging);
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::TimedOutAllow);
    // Re-tick does not re-resolve a terminal entry (no clock reset, no loop).
    assert!(pool.check_timeouts(&cfg, 200).is_empty());
}

#[test]
fn outer_timeout_fail_close_tombstones_dropped() {
    let cfg = FilterConfig::default();
    let hash = B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    let mut pool = BufferPool::default();
    pool.insert(entry(0, TimeoutAction::Deny));
    let out = pool.check_timeouts(&cfg, 91);
    assert_eq!(out[0].2, Resolution::Discard);
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Dropped);
}

#[test]
fn total_timeout_resolves_submitted_and_pending() {
    let cfg = FilterConfig::default();
    for status in [BufferStatus::Submitted, BufferStatus::Pending] {
        let mut pool = BufferPool::default();
        let mut item = entry(0, TimeoutAction::Allow);
        item.status = status;
        pool.insert(item);

        let resolved = pool.resolve_total_timeouts(&cfg, 91);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].2, Resolution::ReleaseForPackaging);
        assert_eq!(pool.get(&resolved[0].0).unwrap().status, BufferStatus::TimedOutAllow);
    }
}

#[test]
fn approved_ignores_total_retry_timeout() {
    let cfg = FilterConfig::default();
    for timeout_action in [TimeoutAction::Allow, TimeoutAction::Deny] {
        let mut pool = BufferPool::default();
        let mut item = entry(0, timeout_action);
        item.status = BufferStatus::Approved;
        let hash = item.tx_hash;
        pool.insert(item);

        assert!(pool.resolve_total_timeouts(&cfg, 91).is_empty());
        assert!(pool.check_timeouts(&cfg, 200).is_empty());
        assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Approved);
    }
}

#[test]
fn late_denied_cannot_override_expired_fail_open() {
    let cfg = FilterConfig::default();
    let mut pool = BufferPool::default();
    let mut item = entry(0, TimeoutAction::Allow);
    item.status = BufferStatus::Pending;
    let hash = item.tx_hash;
    pool.insert(item);
    let generation = pool.get(&hash).unwrap().generation;

    assert_eq!(
        pool.apply_query_status(&hash, generation, "denied", &cfg, 91),
        Some(QueryResolution::RetryTimeout(Resolution::ReleaseForPackaging))
    );
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::TimedOutAllow);
}

#[test]
fn late_approved_cannot_override_expired_fail_close() {
    let cfg = FilterConfig::default();
    let mut pool = BufferPool::default();
    let mut item = entry(0, TimeoutAction::Deny);
    item.status = BufferStatus::Pending;
    let hash = item.tx_hash;
    pool.insert(item);
    let generation = pool.get(&hash).unwrap().generation;

    assert_eq!(
        pool.apply_query_status(&hash, generation, "approved", &cfg, 91),
        Some(QueryResolution::RetryTimeout(Resolution::Discard))
    );
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Dropped);
}

#[test]
fn query_before_deadline_applies_normally() {
    let cfg = FilterConfig::default();
    for (status, expected_resolution, expected_status) in [
        ("approved", None, BufferStatus::Approved),
        ("denied", Some(QueryResolution::Denied), BufferStatus::Dropped),
        ("outdated", Some(QueryResolution::Outdated), BufferStatus::Dropped),
    ] {
        let mut pool = BufferPool::default();
        let mut item = entry(0, TimeoutAction::Allow);
        item.status = BufferStatus::Pending;
        let hash = item.tx_hash;
        pool.insert(item);
        let generation = pool.get(&hash).unwrap().generation;

        assert_eq!(
            pool.apply_query_status(&hash, generation, status, &cfg, 89),
            expected_resolution
        );
        assert_eq!(pool.get(&hash).unwrap().status, expected_status);
    }
}

#[test]
fn non_terminal_reconciliation_is_generation_fenced() {
    let mut pool = BufferPool::default();
    let first = entry(0, TimeoutAction::Allow);
    let hash = first.tx_hash;
    pool.insert(first);
    let old_generation = pool.get(&hash).unwrap().generation;
    assert_eq!(pool.non_terminal_lifecycles(), [(hash, old_generation)]);

    pool.remove(&hash);
    let mut replacement = entry(1, TimeoutAction::Allow);
    replacement.status = BufferStatus::Approved;
    pool.insert(replacement);
    let new_generation = pool.get(&hash).unwrap().generation;
    assert_ne!(old_generation, new_generation);
    assert!(!pool.remove_non_terminal_if_generation(&hash, old_generation));
    assert!(pool.remove_non_terminal_if_generation(&hash, new_generation));
    assert!(pool.is_empty());
}

#[test]
fn non_terminal_reconciliation_keeps_terminal_tombstones() {
    let mut pool = BufferPool::default();
    let mut item = entry(0, TimeoutAction::Deny);
    item.status = BufferStatus::Dropped;
    let hash = item.tx_hash;
    pool.insert(item);
    let generation = pool.get(&hash).unwrap().generation;

    assert!(pool.non_terminal_lifecycles().is_empty());
    assert!(!pool.remove_non_terminal_if_generation(&hash, generation));
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Dropped);
}

#[test]
fn prune_terminal_evicts_only_expired_tombstones() {
    let a = B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    let b = B256::from_str("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
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
fn canonical_cleanup_removes_only_committed_hashes() {
    let a = B256::from_str("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    let b = B256::from_str("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        .unwrap();
    let mut pool = BufferPool::default();
    pool.insert(entry(0, TimeoutAction::Allow));
    let mut second = entry(0, TimeoutAction::Allow);
    second.tx_hash = b;
    pool.insert(second);

    assert_eq!(pool.remove_canonical(&[a]), 1);
    assert!(pool.get(&a).is_none());
    assert!(pool.get(&b).is_some());
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
    let h =
        parse_hash("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    assert_eq!(pool.get(&h).unwrap().status, BufferStatus::Submitted);
    pool.check_timeouts(&cfg, 9);
    assert_eq!(pool.get(&h).unwrap().status, BufferStatus::NotSubmitted);
}

#[test]
fn pending_timeout_boundary_is_strict() {
    let cfg = FilterConfig::default();
    let mut pool = BufferPool::default();
    let mut item = entry(0, TimeoutAction::Allow);
    item.status = BufferStatus::Pending;
    let hash = item.tx_hash;
    pool.insert(item);

    pool.check_timeouts(&cfg, 20);
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::Pending);
    pool.check_timeouts(&cfg, 21);
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::NotSubmitted);
    assert_eq!(pool.get(&hash).unwrap().first_not_submitted_at, 0);
}

#[test]
fn retry_cycles_preserve_first_not_submitted_at() {
    let cfg = FilterConfig::default();
    let mut pool = BufferPool::default();
    let item = entry(0, TimeoutAction::Allow);
    let hash = item.tx_hash;
    pool.insert(item);

    pool.apply_submit_response_current(&[format!("{hash:#x}")], 1);
    pool.apply_query_status_current(&hash, "pending", 2);
    pool.check_timeouts(&cfg, 23);
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::NotSubmitted);
    pool.apply_submit_response_current(&[format!("{hash:#x}")], 24);
    pool.apply_query_status_current(&hash, "pending", 25);
    assert!(pool.check_timeouts(&cfg, 90).is_empty());
    assert_eq!(pool.get(&hash).unwrap().first_not_submitted_at, 0);

    let resolved = pool.check_timeouts(&cfg, 91);
    assert_eq!(resolved[0].2, Resolution::ReleaseForPackaging);
    assert_eq!(pool.get(&hash).unwrap().status, BufferStatus::TimedOutAllow);
}
