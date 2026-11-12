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

#[path = "../tests/submit__tests.rs"]
#[cfg(test)]
mod tests;
