//! RCS submit batching and dependency-aware bounded concurrency.
//!
//! Groups that can affect the same nonce or quota reservation are submitted in ascending block
//! height order. Independent groups use a work-conserving ready queue, so one slow request only
//! occupies one concurrency slot rather than blocking the rest of the batch.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use alloy_primitives::{Address, B256, U256};
use futures_util::stream::{FuturesUnordered, StreamExt};
use tracing::warn;

use crate::client::{RcsClient, SubmitRequest, SubmitTx};
use crate::handle::Shared;
use crate::metrics::RequestEndpoint;
use crate::pool::BufferStatus;

type NonceKey = (Address, u64);
type QuotaKey = (Address, Address);

#[derive(Debug)]
struct SubmitEntry {
    tx: SubmitTx,
    hash: B256,
    generation: u64,
    origin: Address,
}

#[derive(Debug, Default)]
struct GroupConflictProfile {
    nonce_keys: HashSet<NonceKey>,
    quota_keys: HashSet<QuotaKey>,
    exclusive: bool,
}

#[derive(Debug)]
struct SubmitGroup {
    block_height: u64,
    entries: Vec<SubmitEntry>,
    profile: GroupConflictProfile,
}

impl SubmitGroup {
    fn new(block_height: u64, entries: Vec<SubmitEntry>) -> Self {
        let profile = profile_entries(&entries);
        Self { block_height, entries, profile }
    }
}

#[derive(Debug)]
struct SubmitNode {
    group: Option<SubmitGroup>,
    remaining_predecessors: usize,
    successors: Vec<usize>,
}

#[derive(Debug)]
enum ExecutionSegment {
    Normal(Vec<SubmitNode>),
    Exclusive(SubmitGroup),
}

#[derive(Debug, Default)]
struct SubmitBatchStats {
    current_in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl SubmitBatchStats {
    fn enter(&self) -> InFlightGuard<'_> {
        let current = self.current_in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.max_in_flight.fetch_max(current, Ordering::Relaxed);
        InFlightGuard(self)
    }

    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::Relaxed)
    }
}

struct InFlightGuard<'a>(&'a SubmitBatchStats);

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.current_in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

fn profile_entries(entries: &[SubmitEntry]) -> GroupConflictProfile {
    let mut profile = GroupConflictProfile::default();
    for entry in entries {
        profile.nonce_keys.insert((entry.origin, entry.tx.nonce));
        if entry.tx.actions.is_empty() {
            profile.exclusive = true;
            continue;
        }
        for (audit_type, items) in &entry.tx.actions {
            if audit_type != "quota" || items.is_empty() {
                profile.exclusive = true;
                continue;
            }
            for item in items {
                let Some(to) = known_quota_recipient(&item.params) else {
                    profile.exclusive = true;
                    continue;
                };
                let Ok(token) = item.address.parse::<Address>() else {
                    profile.exclusive = true;
                    continue;
                };
                profile.quota_keys.insert((to, token));
            }
        }
    }
    profile
}

fn known_quota_recipient(params: &BTreeMap<String, serde_json::Value>) -> Option<Address> {
    let to = address_param(params, "to")?;
    let known_shape = match params.len() {
        // ERC20 Transfer(from, to, value)
        3 => address_param(params, "from").is_some() && uint_param(params, "value"),
        // ERC1155 TransferSingle(operator, from, to, id, value), or
        // ERC1155 TransferBatch(operator, from, to, ids, values).
        5 if address_param(params, "operator").is_some()
            && address_param(params, "from").is_some() =>
        {
            (uint_param(params, "id") && uint_param(params, "value"))
                || matching_uint_arrays(params, "ids", "values")
        }
        _ => false,
    };
    known_shape.then_some(to)
}

fn address_param(params: &BTreeMap<String, serde_json::Value>, name: &str) -> Option<Address> {
    params.get(name)?.as_str()?.parse().ok()
}

fn uint_param(params: &BTreeMap<String, serde_json::Value>, name: &str) -> bool {
    params
        .get(name)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<U256>().ok())
        .is_some()
}

fn matching_uint_arrays(
    params: &BTreeMap<String, serde_json::Value>,
    left: &str,
    right: &str,
) -> bool {
    let Some(left) = params.get(left).and_then(serde_json::Value::as_array) else { return false };
    let Some(right) = params.get(right).and_then(serde_json::Value::as_array) else { return false };
    !left.is_empty()
        && left.len() == right.len()
        && left
            .iter()
            .chain(right)
            .all(|value| value.as_str().and_then(|value| value.parse::<U256>().ok()).is_some())
}

fn build_submit_schedule(groups: Vec<SubmitGroup>) -> Vec<ExecutionSegment> {
    let mut segments = Vec::new();
    let mut normal = Vec::new();
    for group in groups {
        if group.profile.exclusive {
            if !normal.is_empty() {
                segments.push(ExecutionSegment::Normal(build_normal_nodes(std::mem::take(
                    &mut normal,
                ))));
            }
            segments.push(ExecutionSegment::Exclusive(group));
        } else {
            normal.push(group);
        }
    }
    if !normal.is_empty() {
        segments.push(ExecutionSegment::Normal(build_normal_nodes(normal)));
    }
    segments
}

fn build_normal_nodes(groups: Vec<SubmitGroup>) -> Vec<SubmitNode> {
    let mut nodes: Vec<SubmitNode> = Vec::with_capacity(groups.len());
    let mut last_nonce = HashMap::<NonceKey, usize>::new();
    let mut last_quota = HashMap::<QuotaKey, usize>::new();

    for group in groups {
        let nonce_keys = group.profile.nonce_keys.iter().copied().collect::<Vec<_>>();
        let quota_keys = group.profile.quota_keys.iter().copied().collect::<Vec<_>>();
        let mut predecessors = BTreeSet::new();
        for key in &nonce_keys {
            if let Some(previous) = last_nonce.get(key) {
                predecessors.insert(*previous);
            }
        }
        for key in &quota_keys {
            if let Some(previous) = last_quota.get(key) {
                predecessors.insert(*previous);
            }
        }

        let index = nodes.len();
        nodes.push(SubmitNode {
            group: Some(group),
            remaining_predecessors: predecessors.len(),
            successors: Vec::new(),
        });
        for predecessor in predecessors {
            nodes[predecessor].successors.push(index);
        }
        for key in nonce_keys {
            last_nonce.insert(key, index);
        }
        for key in quota_keys {
            last_quota.insert(key, index);
        }
    }
    nodes
}

/// Collects `NotSubmitted` transactions, submits one request per block height, and advances
/// accepted hashes to `Submitted`. The pool lock is never held across an await.
pub(crate) async fn submit_once(shared: &Shared, client: &Arc<dyn RcsClient>) -> crate::Result<()> {
    let mut grouped = BTreeMap::<u64, Vec<SubmitEntry>>::new();
    let expired = {
        let mut pool = shared.pool_lock();
        let now = shared.clock.now_unix();
        let expired = pool.resolve_total_timeouts(&shared.config, now);
        for hash in pool.not_submitted() {
            if let Some(entry) = pool.get(&hash) {
                grouped.entry(entry.block_height).or_default().push(SubmitEntry {
                    tx: SubmitTx {
                        tx_hash: format!("{:#x}", entry.tx_hash),
                        origin: format!("{:#x}", entry.origin),
                        contract_address: format!("{:#x}", entry.contract_address),
                        nonce: entry.nonce,
                        actions: entry.actions.clone(),
                    },
                    hash: entry.tx_hash,
                    generation: entry.generation,
                    origin: entry.origin,
                });
            }
        }
        expired
    };
    shared.emit_timeout_resolutions(&expired);
    if !expired.is_empty() {
        shared.update_buffer_metric();
    }
    if grouped.is_empty() {
        return Ok(());
    }

    let started = Instant::now();
    let group_count = grouped.len();
    let groups =
        grouped.into_iter().map(|(height, entries)| SubmitGroup::new(height, entries)).collect();
    let schedule = build_submit_schedule(groups);
    let stats = SubmitBatchStats::default();
    let mut errors = Vec::new();

    for segment in schedule {
        match segment {
            ExecutionSegment::Normal(nodes) => {
                errors.extend(
                    run_normal_segment(
                        shared,
                        client,
                        nodes,
                        shared.config.submit_max_concurrency,
                        &stats,
                    )
                    .await,
                );
            }
            ExecutionSegment::Exclusive(group) => {
                let block_height = group.block_height;
                if let Err(error) = submit_group(shared, client, group, &stats).await {
                    errors.push((block_height, error));
                }
            }
        }
    }

    shared.metrics.submit_batch_groups.record(group_count as f64);
    shared.metrics.submit_batch_max_in_flight.record(stats.max_in_flight() as f64);
    shared.metrics.submit_batch_duration_seconds.record(started.elapsed().as_secs_f64());

    errors.into_iter().min_by_key(|(height, _)| *height).map_or(Ok(()), |(_, error)| Err(error))
}

async fn run_normal_segment(
    shared: &Shared,
    client: &Arc<dyn RcsClient>,
    mut nodes: Vec<SubmitNode>,
    max_concurrency: usize,
    stats: &SubmitBatchStats,
) -> Vec<(u64, crate::FilterError)> {
    let mut ready = BinaryHeap::new();
    for (index, node) in nodes.iter().enumerate() {
        if node.remaining_predecessors == 0 {
            let height = node.group.as_ref().expect("unstarted node has group").block_height;
            ready.push(Reverse((height, index)));
        }
    }

    let mut active = FuturesUnordered::new();
    let mut errors = Vec::new();
    let mut completed = 0usize;

    while active.len() < max_concurrency {
        let Some(Reverse((_, index))) = ready.pop() else { break };
        let group = nodes[index].group.take().expect("ready node has group");
        active.push(execute_group(index, shared, client, group, stats));
    }

    while let Some((index, block_height, result)) = active.next().await {
        completed += 1;
        if let Err(error) = result {
            errors.push((block_height, error));
        }
        let successors = std::mem::take(&mut nodes[index].successors);
        for successor in successors {
            let node = &mut nodes[successor];
            node.remaining_predecessors -= 1;
            if node.remaining_predecessors == 0 {
                let height = node.group.as_ref().expect("unstarted node has group").block_height;
                ready.push(Reverse((height, successor)));
            }
        }
        while active.len() < max_concurrency {
            let Some(Reverse((_, next))) = ready.pop() else { break };
            let group = nodes[next].group.take().expect("ready node has group");
            active.push(execute_group(next, shared, client, group, stats));
        }
    }

    debug_assert_eq!(completed, nodes.len(), "submit dependency graph must be acyclic");
    errors
}

async fn execute_group(
    index: usize,
    shared: &Shared,
    client: &Arc<dyn RcsClient>,
    group: SubmitGroup,
    stats: &SubmitBatchStats,
) -> (usize, u64, crate::Result<()>) {
    let block_height = group.block_height;
    let result = submit_group(shared, client, group, stats).await;
    (index, block_height, result)
}

async fn submit_group(
    shared: &Shared,
    client: &Arc<dyn RcsClient>,
    group: SubmitGroup,
    stats: &SubmitBatchStats,
) -> crate::Result<()> {
    let block_height = group.block_height;
    let (entries, expired) = {
        let mut pool = shared.pool_lock();
        let now = shared.clock.now_unix();
        let expired = pool.resolve_total_timeouts(&shared.config, now);
        let entries = group
            .entries
            .into_iter()
            .filter(|entry| pool.matches(&entry.hash, entry.generation, BufferStatus::NotSubmitted))
            .collect::<Vec<_>>();
        (entries, expired)
    };
    shared.emit_timeout_resolutions(&expired);
    if !expired.is_empty() {
        shared.update_buffer_metric();
    }
    if entries.is_empty() {
        return Ok(());
    }

    let expected_generations =
        entries.iter().map(|entry| (entry.hash, entry.generation)).collect::<HashMap<_, _>>();
    let txs = entries.into_iter().map(|entry| entry.tx).collect();
    let request_started = Instant::now();
    let in_flight = stats.enter();
    let result = client.submit(SubmitRequest { xlayer_block_height: block_height, txs }).await;
    drop(in_flight);
    shared.metrics.record_request(RequestEndpoint::Submit, request_started.elapsed(), &result);
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            warn!(target: "rcs_filter", block_height, %error, "submit group failed; continuing other heights");
            let expired = {
                let mut pool = shared.pool_lock();
                let now = shared.clock.now_unix();
                pool.resolve_total_timeouts(&shared.config, now)
            };
            shared.emit_timeout_resolutions(&expired);
            if !expired.is_empty() {
                shared.update_buffer_metric();
            }
            return Err(error);
        }
    };
    for rejected in &response.rejected_malformed {
        warn!(target: "rcs_filter", tx_hash = %rejected, "submit rejected_malformed; retrying");
    }
    let expired = {
        let mut pool = shared.pool_lock();
        let now = shared.clock.now_unix();
        pool.apply_submit_response(&response.accepted, &expected_generations, &shared.config, now)
    };
    shared.emit_timeout_resolutions(&expired);
    shared.update_buffer_metric();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::client::ActionItem;

    fn address(byte: u8) -> Address {
        Address::repeat_byte(byte)
    }

    fn quota_entry(
        hash_byte: u8,
        origin: Address,
        nonce: u64,
        to: &str,
        token: &str,
    ) -> SubmitEntry {
        let hash = B256::repeat_byte(hash_byte);
        SubmitEntry {
            tx: SubmitTx {
                tx_hash: format!("{hash:#x}"),
                origin: format!("{origin:#x}"),
                contract_address: format!("{:#x}", address(0xee)),
                nonce,
                actions: BTreeMap::from([(
                    "quota".to_string(),
                    vec![ActionItem {
                        name: "transfer".to_string(),
                        address: token.to_string(),
                        params: BTreeMap::from([
                            ("from".to_string(), json!(format!("{origin:#x}"))),
                            ("to".to_string(), json!(to)),
                            ("value".to_string(), json!("1")),
                        ]),
                    }],
                )]),
            },
            hash,
            generation: u64::from(hash_byte),
            origin,
        }
    }

    fn group(height: u64, entry: SubmitEntry) -> SubmitGroup {
        SubmitGroup::new(height, vec![entry])
    }

    fn normal_nodes(schedule: &[ExecutionSegment]) -> &[SubmitNode] {
        assert_eq!(schedule.len(), 1);
        let ExecutionSegment::Normal(nodes) = &schedule[0] else {
            panic!("expected one normal segment")
        };
        nodes
    }

    #[test]
    fn empty_planner_has_no_segments() {
        assert!(build_submit_schedule(Vec::new()).is_empty());
    }

    #[test]
    fn independent_groups_have_no_predecessors() {
        let groups = (1..=4)
            .map(|i| {
                group(
                    100 + u64::from(i),
                    quota_entry(
                        i,
                        address(i),
                        u64::from(i),
                        &format!("{:#x}", address(i + 10)),
                        &format!("{:#x}", address(i + 20)),
                    ),
                )
            })
            .collect();
        let schedule = build_submit_schedule(groups);
        let nodes = normal_nodes(&schedule);
        assert!(nodes.iter().all(|node| node.remaining_predecessors == 0));
        assert!(nodes.iter().all(|node| node.successors.is_empty()));
    }

    #[test]
    fn same_nonce_groups_form_nearest_predecessor_chain() {
        let origin = address(1);
        let groups = (1..=3)
            .map(|i| {
                group(
                    100 + u64::from(i),
                    quota_entry(
                        i,
                        origin,
                        7,
                        &format!("{:#x}", address(i + 10)),
                        &format!("{:#x}", address(i + 20)),
                    ),
                )
            })
            .collect();
        let schedule = build_submit_schedule(groups);
        let nodes = normal_nodes(&schedule);
        assert_eq!(
            nodes.iter().map(|node| node.remaining_predecessors).collect::<Vec<_>>(),
            [0, 1, 1]
        );
        assert_eq!(nodes[0].successors, [1]);
        assert_eq!(nodes[1].successors, [2]);
        assert!(nodes[2].successors.is_empty());
    }

    #[test]
    fn same_quota_groups_form_nearest_predecessor_chain() {
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let groups = (1..=3)
            .map(|i| {
                group(100 + u64::from(i), quota_entry(i, address(i), u64::from(i), &to, &token))
            })
            .collect();
        let schedule = build_submit_schedule(groups);
        let nodes = normal_nodes(&schedule);
        assert_eq!(
            nodes.iter().map(|node| node.remaining_predecessors).collect::<Vec<_>>(),
            [0, 1, 1]
        );
        assert_eq!(nodes[0].successors, [1]);
        assert_eq!(nodes[1].successors, [2]);
    }

    #[test]
    fn different_erc1155_ids_with_same_recipient_and_token_still_conflict() {
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let mut first = quota_entry(1, address(1), 1, &to, &token);
        let mut second = quota_entry(2, address(2), 2, &to, &token);
        for (entry, id) in [(&mut first, "7"), (&mut second, "8")] {
            entry.tx.actions.get_mut("quota").unwrap()[0].params = BTreeMap::from([
                ("operator".to_string(), json!(format!("{:#x}", address(20)))),
                ("from".to_string(), json!(format!("{:#x}", address(21)))),
                ("to".to_string(), json!(to.clone())),
                ("id".to_string(), json!(id)),
                ("value".to_string(), json!("1")),
            ]);
        }
        let schedule = build_submit_schedule(vec![group(100, first), group(101, second)]);
        let nodes = normal_nodes(&schedule);
        assert_eq!(nodes[1].remaining_predecessors, 1);
        assert_eq!(nodes[0].successors, [1]);
    }

    #[test]
    fn valid_erc1155_batch_shape_remains_non_exclusive() {
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let mut entry = quota_entry(1, address(1), 1, &to, &token);
        entry.tx.actions.get_mut("quota").unwrap()[0].params = BTreeMap::from([
            ("operator".to_string(), json!(format!("{:#x}", address(20)))),
            ("from".to_string(), json!(format!("{:#x}", address(21)))),
            ("to".to_string(), json!(to)),
            ("ids".to_string(), json!(["7", "8"])),
            ("values".to_string(), json!(["100", "200"])),
        ]);

        let schedule = build_submit_schedule(vec![group(100, entry)]);
        assert!(matches!(schedule.as_slice(), [ExecutionSegment::Normal(_)]));
    }

    #[test]
    fn shared_nonce_and_quota_predecessor_is_counted_once() {
        let origin = address(1);
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let schedule = build_submit_schedule(vec![
            group(100, quota_entry(1, origin, 7, &to, &token)),
            group(101, quota_entry(2, origin, 7, &to, &token)),
        ]);
        let nodes = normal_nodes(&schedule);
        assert_eq!(nodes[1].remaining_predecessors, 1);
        assert_eq!(nodes[0].successors, [1]);
    }

    #[test]
    fn group_with_two_distinct_conflicts_waits_for_both_predecessors() {
        let origin_a = address(1);
        let origin_b = address(2);
        let to_x = format!("{:#x}", address(10));
        let to_y = format!("{:#x}", address(12));
        let token = format!("{:#x}", address(11));
        let schedule = build_submit_schedule(vec![
            group(100, quota_entry(1, origin_a, 7, &to_x, &token)),
            group(101, quota_entry(2, origin_b, 8, &to_y, &token)),
            group(102, quota_entry(3, origin_a, 7, &to_y, &token)),
        ]);
        let nodes = normal_nodes(&schedule);
        assert_eq!(nodes[2].remaining_predecessors, 2);
        assert_eq!(nodes[0].successors, [2]);
        assert_eq!(nodes[1].successors, [2]);
    }

    #[test]
    fn unknown_audit_type_is_an_exclusive_barrier() {
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let mut custom = quota_entry(2, address(2), 2, &to, &token);
        let items = custom.tx.actions.remove("quota").unwrap();
        custom.tx.actions.insert("custom".to_string(), items);
        let schedule = build_submit_schedule(vec![
            group(100, quota_entry(1, address(1), 1, &to, &token)),
            group(101, custom),
            group(102, quota_entry(3, address(3), 3, &to, &token)),
        ]);
        assert!(matches!(
            schedule.as_slice(),
            [
                ExecutionSegment::Normal(_),
                ExecutionSegment::Exclusive(_),
                ExecutionSegment::Normal(_)
            ]
        ));
    }

    #[test]
    fn malformed_or_empty_actions_are_exclusive() {
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let mut malformed = quota_entry(1, address(1), 1, &to, &token);
        malformed.tx.actions.get_mut("quota").unwrap()[0].params.remove("to");
        let mut empty = quota_entry(2, address(2), 2, &to, &token);
        empty.tx.actions.clear();
        let schedule = build_submit_schedule(vec![group(100, malformed), group(101, empty)]);
        assert!(schedule.iter().all(|segment| matches!(segment, ExecutionSegment::Exclusive(_))));
    }

    #[test]
    fn empty_quota_items_and_invalid_token_are_exclusive() {
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let mut empty_items = quota_entry(1, address(1), 1, &to, &token);
        empty_items.tx.actions.get_mut("quota").unwrap().clear();
        let mut invalid_token = quota_entry(2, address(2), 2, &to, &token);
        invalid_token.tx.actions.get_mut("quota").unwrap()[0].address = "not-an-address".into();

        let schedule =
            build_submit_schedule(vec![group(100, empty_items), group(101, invalid_token)]);
        assert!(schedule.iter().all(|segment| matches!(segment, ExecutionSegment::Exclusive(_))));
    }

    #[test]
    fn malformed_known_quota_shapes_are_exclusive() {
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let mut missing_value = quota_entry(1, address(1), 1, &to, &token);
        missing_value.tx.actions.get_mut("quota").unwrap()[0].params.remove("value");

        let mut invalid_value = quota_entry(2, address(2), 2, &to, &token);
        invalid_value.tx.actions.get_mut("quota").unwrap()[0]
            .params
            .insert("value".to_string(), json!({ "amount": "1" }));

        let mut mismatched_batch = quota_entry(3, address(3), 3, &to, &token);
        mismatched_batch.tx.actions.get_mut("quota").unwrap()[0].params = BTreeMap::from([
            ("operator".to_string(), json!(format!("{:#x}", address(20)))),
            ("from".to_string(), json!(format!("{:#x}", address(21)))),
            ("to".to_string(), json!(to.clone())),
            ("ids".to_string(), json!(["1", "2"])),
            ("values".to_string(), json!(["3"])),
        ]);

        let mut unknown_field = quota_entry(4, address(4), 4, &to, &token);
        unknown_field.tx.actions.get_mut("quota").unwrap()[0]
            .params
            .insert("resource".to_string(), json!("unexpected"));

        let schedule = build_submit_schedule(vec![
            group(100, missing_value),
            group(101, invalid_value),
            group(102, mismatched_batch),
            group(103, unknown_field),
        ]);
        assert!(schedule.iter().all(|segment| matches!(segment, ExecutionSegment::Exclusive(_))));
    }

    #[test]
    fn address_keys_are_compared_after_typed_normalization() {
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let upper_to = format!("0x{}", to[2..].to_uppercase());
        let upper_token = format!("0x{}", token[2..].to_uppercase());
        let schedule = build_submit_schedule(vec![
            group(100, quota_entry(1, address(1), 1, &to, &token)),
            group(101, quota_entry(2, address(2), 2, &upper_to, &upper_token)),
        ]);
        let nodes = normal_nodes(&schedule);
        assert_eq!(nodes[1].remaining_predecessors, 1);
        assert_eq!(nodes[0].successors, [1]);
    }

    #[test]
    fn dependency_edges_always_point_from_lower_to_higher_height() {
        let to = format!("{:#x}", address(10));
        let token = format!("{:#x}", address(11));
        let origin = address(1);
        let schedule = build_submit_schedule(vec![
            group(100, quota_entry(1, origin, 7, &to, &token)),
            group(101, quota_entry(2, origin, 7, &to, &token)),
            group(102, quota_entry(3, origin, 7, &to, &token)),
        ]);
        let nodes = normal_nodes(&schedule);
        for (predecessor, node) in nodes.iter().enumerate() {
            let predecessor_height = node.group.as_ref().unwrap().block_height;
            for successor in &node.successors {
                let successor_height = nodes[*successor].group.as_ref().unwrap().block_height;
                assert!(predecessor < *successor);
                assert!(predecessor_height < successor_height);
            }
        }
    }

    #[test]
    fn batch_stats_track_actual_in_flight_and_release_slots() {
        let stats = SubmitBatchStats::default();
        assert_eq!(stats.max_in_flight(), 0);

        let first = stats.enter();
        let second = stats.enter();
        assert_eq!(stats.current_in_flight.load(Ordering::Relaxed), 2);
        assert_eq!(stats.max_in_flight(), 2);

        drop(first);
        assert_eq!(stats.current_in_flight.load(Ordering::Relaxed), 1);
        drop(second);
        assert_eq!(stats.current_in_flight.load(Ordering::Relaxed), 0);
        assert_eq!(stats.max_in_flight(), 2);
    }
}
