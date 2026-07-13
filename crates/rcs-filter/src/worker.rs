//! Background workers (TD §4.7/§4.9). All RCS network IO lives here; the hot path
//! ([`crate::handle::FilterHandle::screen_tx`]) never blocks on the network.
//!
//! Four cooperating tokio tasks, each run under a supervisor ([`spawn_supervised`]) so a
//! panic is logged (never silent) and the task self-heals:
//! - **rules**: FR-2 blocking startup load (unbounded exponential backoff, no default
//!   rules) then FR-3 hot-reload (`content_version`-triggered atomic swap).
//! - **submit**: FR-5 batch submit every `batch_window`.
//! - **query**: FR-5 adjudication poll, mapping RCS status → buffer transitions.
//! - **timeout**: FR-6 timeout tick (outer 90s fallback + 8s/20s stalls) + terminal-tombstone
//!   eviction (bounds pool memory).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::{debug, error, warn};

use crate::client::{QueryParams, RcsClient, SubmitRequest, SubmitTx};
use crate::config::is_supported_protocol;
use crate::handle::Shared;
use crate::rules::load_rules;

/// Initial backoff for the startup load retry loop.
const INITIAL_BACKOFF: Duration = Duration::from_millis(200);
/// Backoff ceiling for the startup load retry loop.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Adjudication poll interval / timeout tick interval.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Delay before a supervised worker is restarted after an unexpected exit/panic.
const SUPERVISOR_RESTART_BACKOFF: Duration = Duration::from_secs(1);

/// Spawns all background workers under supervision. No-op when the filter is disabled (FR-9).
/// Returns the supervisor task handles; aborting them (on [`crate::FilterHandle`] drop) stops
/// the workers and prevents leaked tasks.
pub(crate) fn spawn(shared: Shared, client: Arc<dyn RcsClient>) -> Vec<JoinHandle<()>> {
    if !shared.config.enabled {
        return Vec::new();
    }
    vec![
        spawn_supervised("rules", {
            let shared = shared.clone();
            let client = client.clone();
            move || rules_task(shared.clone(), client.clone())
        }),
        spawn_supervised("submit", {
            let shared = shared.clone();
            let client = client.clone();
            move || submit_task(shared.clone(), client.clone())
        }),
        spawn_supervised("query", {
            let shared = shared.clone();
            let client = client.clone();
            move || query_task(shared.clone(), client.clone())
        }),
        spawn_supervised("timeout", {
            let shared = shared.clone();
            move || timeout_task(shared.clone())
        }),
    ]
}

/// Aborts the wrapped task when dropped, so aborting a supervisor also stops its worker.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs `make()` under a supervisor: the worker loop should never return, so any exit or
/// panic is logged (never silent) and the worker is restarted after a short backoff. The
/// returned handle is the supervisor; aborting it stops the worker for good.
fn spawn_supervised<F, Fut>(name: &'static str, make: F) -> JoinHandle<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            let handle = tokio::spawn(make());
            // If the supervisor itself is cancelled while awaiting, this guard aborts the
            // in-flight worker task rather than detaching (leaking) it.
            let guard = AbortOnDrop(handle.abort_handle());
            match handle.await {
                Ok(()) => {
                    warn!(target: "rcs_filter", worker = name, "worker exited unexpectedly; restarting");
                }
                Err(e) if e.is_panic() => {
                    error!(target: "rcs_filter", worker = name, "worker panicked; restarting");
                }
                Err(_) => return, // cancelled (supervisor aborted) → stop.
            }
            drop(guard); // worker already finished; nothing to abort.
            tokio::time::sleep(SUPERVISOR_RESTART_BACKOFF).await;
        }
    })
}

/// FR-2 blocking startup load followed by FR-3 hot-reload polling.
async fn rules_task(shared: Shared, client: Arc<dyn RcsClient>) {
    // FR-2: block until a valid, supported rule set is loaded. Unbounded exponential
    // backoff; never fall back to default rules.
    let mut backoff = INITIAL_BACKOFF;
    while !matches!(load_and_install(&shared, &client).await, Ok(true)) {
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }

    // FR-3: poll `content_version`; only pull the full `/rules` body on change. The lightweight
    // probe also carries `protocol_version`, so an unsupported version is filtered out *here*
    // without pulling the body every tick (#5 busy-loop suppression). Because the decision is
    // re-derived from each probe (no sticky "rejected version" state), a later protocol fix —
    // even one that keeps the same `content_version` — recovers automatically on the next poll.
    let interval = shared.config.rules_version_poll_interval;
    loop {
        tokio::time::sleep(interval).await;
        let current = shared.current_rules().content_version;
        match client.get_rules_version().await {
            Ok(v) if v.content_version != current => {
                if !is_supported_protocol(v.protocol_version) {
                    // Keep the old rules and keep mining; do not pull the body (it would only be
                    // rejected). Distinct from the startup block, which never mines without rules.
                    warn!(
                        target: "rcs_filter",
                        protocol_version = v.protocol_version,
                        "advertised unsupported protocol_version; keeping current rules"
                    );
                } else if let Err(e) = load_and_install(&shared, &client).await {
                    warn!(target: "rcs_filter", error = %e, "hot-reload rule pull failed; keeping current rules");
                }
            }
            Ok(_) => {}
            Err(e) => {
                debug!(target: "rcs_filter", error = %e, "rules/version probe failed; keeping current rules")
            }
        }
    }
}

/// Pulls `GET /rules`, validates the protocol version, and atomically installs the new set.
/// Returns `Ok(true)` when installed, `Ok(false)` when the protocol version is unsupported
/// (rejected — no install), `Err` on transport/decode failure.
pub(crate) async fn load_and_install(
    shared: &Shared,
    client: &Arc<dyn RcsClient>,
) -> crate::Result<bool> {
    let resp = client.get_rules().await?;
    if !is_supported_protocol(resp.protocol_version) {
        warn!(
            target: "rcs_filter",
            protocol_version = resp.protocol_version,
            "unsupported protocol_version; rejecting rule set"
        );
        return Ok(false);
    }
    let set = load_rules(resp.protocol_version, resp.content_version, resp.rules);
    *shared.rules_write() = Arc::new(set);
    shared.ready.store(true, Ordering::Release);
    Ok(true)
}

/// FR-5 batch-submit loop.
async fn submit_task(shared: Shared, client: Arc<dyn RcsClient>) {
    loop {
        tokio::time::sleep(shared.config.batch_window).await;
        if let Err(e) = submit_once(&shared, &client).await {
            warn!(target: "rcs_filter", error = %e, "batch submit failed; keeping NotSubmitted");
        }
    }
}

/// Collects `NotSubmitted` txs, submits them grouped by block height, and advances accepted
/// hashes to `Submitted` (FR-5). Never holds the pool lock across an await.
pub(crate) async fn submit_once(shared: &Shared, client: &Arc<dyn RcsClient>) -> crate::Result<()> {
    // Snapshot the batch under the lock, grouped by block height (one request per height).
    let mut groups: std::collections::BTreeMap<u64, Vec<SubmitTx>> =
        std::collections::BTreeMap::new();
    {
        let pool = shared.pool_lock();
        for hash in pool.not_submitted() {
            if let Some(entry) = pool.get(&hash) {
                groups.entry(entry.block_height).or_default().push(SubmitTx {
                    tx_hash: format!("{:#x}", entry.tx_hash),
                    origin: format!("{:#x}", entry.origin),
                    contract_address: format!("{:#x}", entry.contract_address),
                    nonce: entry.nonce,
                    actions: entry.actions.clone(),
                });
            }
        }
    }
    if groups.is_empty() {
        return Ok(());
    }

    for (block_height, txs) in groups {
        let resp = client.submit(SubmitRequest { xlayer_block_height: block_height, txs }).await?;
        for rejected in &resp.rejected_malformed {
            warn!(target: "rcs_filter", tx_hash = %rejected, "submit rejected_malformed; retrying");
        }
        let now = shared.clock.now_unix();
        shared.pool_lock().apply_submit_response(&resp.accepted, now);
    }
    Ok(())
}

/// FR-5 adjudication poll loop.
async fn query_task(shared: Shared, client: Arc<dyn RcsClient>) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        if let Err(e) = query_once(&shared, &client).await {
            debug!(target: "rcs_filter", error = %e, "query failed; timeout fallback will apply");
        }
    }
}

/// Queries the status of all in-flight txs and applies transitions. Absent/unrecognized
/// statuses leave the entry untouched (handled by the timeout task) — no optimistic pass.
pub(crate) async fn query_once(shared: &Shared, client: &Arc<dyn RcsClient>) -> crate::Result<()> {
    let in_flight: Vec<String> = {
        let pool = shared.pool_lock();
        pool.in_flight_hashes()
    };
    if in_flight.is_empty() {
        return Ok(());
    }

    let resp = client.query(QueryParams::TxHashes(in_flight)).await?;
    let now = shared.clock.now_unix();
    let mut pool = shared.pool_lock();
    for tx in &resp.txs {
        if let Ok(hash) = tx.tx_hash.parse() {
            if let Some(reason) = &tx.reason {
                debug!(target: "rcs_filter", tx_hash = %tx.tx_hash, status = %tx.status, %reason, "query result");
            }
            // `denied`/`outdated` tombstone the entry as `Dropped` in place (G3) — the entry
            // is intentionally NOT removed here, so the tx is neither re-buffered nor
            // re-submitted; the timeout task evicts the tombstone after the retention window.
            if let Some(resolution) = pool.apply_query_status(&hash, &tx.status, now) {
                debug!(target: "rcs_filter", tx_hash = %tx.tx_hash, ?resolution, "query resolution");
            }
        }
    }
    Ok(())
}

/// FR-6 timeout tick loop + terminal-tombstone eviction (bounds pool memory).
async fn timeout_task(shared: Shared) {
    let retention = shared.config.terminal_entry_retention.as_secs();
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let now = shared.clock.now_unix();
        let (resolved, pruned) = {
            let mut pool = shared.pool_lock();
            let resolved = pool.check_timeouts(&shared.config, now);
            let pruned = pool.prune_terminal(retention, now);
            (resolved, pruned)
        };
        for (hash, resolution) in resolved {
            debug!(target: "rcs_filter", tx_hash = %format!("{hash:#x}"), ?resolution, "timeout resolution");
        }
        if pruned > 0 {
            debug!(target: "rcs_filter", pruned, "evicted expired terminal tombstones");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    /// The supervisor restarts a worker that panics (so a transient panic is not a silent
    /// permanent death), after a backoff.
    #[tokio::test(start_paused = true)]
    async fn supervisor_restarts_worker_after_panic() {
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        let sup = spawn_supervised("test", move || {
            let counter = counter.clone();
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("first run panics");
                }
                // Later run: park so the supervisor stays on this instance.
                loop {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
            }
        });

        // Allow the first run to panic and the backoff + restart to elapse.
        tokio::time::sleep(SUPERVISOR_RESTART_BACKOFF * 2).await;
        assert!(
            attempts.load(Ordering::SeqCst) >= 2,
            "worker must be restarted after a panic (attempts={})",
            attempts.load(Ordering::SeqCst)
        );
        sup.abort();
    }
}
