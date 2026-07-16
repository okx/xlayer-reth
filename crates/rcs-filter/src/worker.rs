//! Background workers (TD §4.7/§4.9). All RCS network IO lives here; the hot path
//! ([`crate::handle::FilterHandle::screen_tx`]) never blocks on the network.
//!
//! Four cooperating tokio tasks, each run under a supervisor ([`spawn_supervised`]) so a
//! panic is logged (never silent) and the task self-heals:
//! - **rules**: FR-2 asynchronous initial load (unbounded exponential backoff, empty rules
//!   until ready) then FR-3 hot-reload (`content_version`-triggered atomic swap).
//! - **submit**: FR-5 batch submit every `batch_window`.
//! - **query**: FR-5 adjudication poll, mapping RCS status → buffer transitions.
//! - **timeout**: FR-6 timeout tick (outer 90s fallback + 8s/20s stalls) + terminal-tombstone
//!   eviction (bounds pool memory).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::client::{QueryParams, RcsClient};
use crate::config::is_supported_protocol;
use crate::handle::{Shared, TerminalEvent, TerminalReason};
use crate::metrics::RequestEndpoint;
use crate::pool::{QueryResolution, Resolution};
use crate::rules::load_rules;

pub(crate) use crate::submit::submit_once;

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

/// FR-2 asynchronous initial load followed by FR-3 hot-reload polling.
async fn rules_task(shared: Shared, client: Arc<dyn RcsClient>) {
    // FR-2: retry until a valid, supported rule set is loaded. Screening remains live with
    // an empty snapshot until then; readiness metrics make that fail-open window observable.
    let mut backoff =
        RetryBackoff::new(shared.config.retry_initial_backoff, shared.config.retry_max_backoff);
    let mut failures = 0u64;
    loop {
        let failure = match load_and_install(&shared, &client).await {
            Ok(true) => {
                if failures > 0 {
                    info!(target: "rcs_filter", endpoint = "/rules", attempts = failures, "RCS endpoint recovered");
                }
                break;
            }
            Ok(false) => "unsupported_protocol".to_string(),
            Err(error) => error_class(&error).to_string(),
        };
        failures += 1;
        let delay = backoff.next_delay();
        shared.metrics.rules_retry_delay_seconds.set(delay.as_secs_f64());
        if failures == 1 || backoff.is_capped() {
            warn!(target: "rcs_filter", endpoint = "/rules", attempt = failures, next_delay = ?delay, error_class = %failure, "RCS request retrying");
        }
        tokio::time::sleep(delay).await;
    }
    shared.metrics.rules_retry_delay_seconds.set(0.0);

    // FR-3: poll `content_version`; only pull the full `/rules` body on change. The lightweight
    // probe also carries `protocol_version`, so an unsupported version is filtered out *here*
    // without pulling the body every tick (#5 busy-loop suppression). Because the decision is
    // re-derived from each probe (no sticky "rejected version" state), a later protocol fix —
    // even one that keeps the same `content_version` — recovers automatically on the next poll.
    let interval = shared.config.rules_version_poll_interval;
    loop {
        tokio::time::sleep(interval).await;
        let current = shared.current_rules().content_version;
        let started = std::time::Instant::now();
        let version_result = client.get_rules_version().await;
        shared.metrics.record_request(
            RequestEndpoint::RulesVersion,
            started.elapsed(),
            &version_result,
        );
        match version_result {
            Ok(v) if v.content_version != current => {
                if !is_supported_protocol(v.protocol_version) {
                    // Keep the old rules and keep mining; do not pull the body because it would
                    // only be rejected. Initial loading follows the same live/retry policy.
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
    let started = std::time::Instant::now();
    let result = client.get_rules().await;
    shared.metrics.record_request(RequestEndpoint::Rules, started.elapsed(), &result);
    let resp = result?;
    if !is_supported_protocol(resp.protocol_version) {
        warn!(
            target: "rcs_filter",
            protocol_version = resp.protocol_version,
            "unsupported protocol_version; rejecting rule set"
        );
        return Ok(false);
    }
    let set = load_rules(resp.protocol_version, resp.content_version, resp.rules);
    shared.install_rules(set);
    Ok(true)
}

/// FR-5 batch-submit loop.
async fn submit_task(shared: Shared, client: Arc<dyn RcsClient>) {
    let mut backoff =
        RetryBackoff::new(shared.config.retry_initial_backoff, shared.config.retry_max_backoff);
    let mut failures = 0u64;
    loop {
        tokio::time::sleep(shared.config.batch_window).await;
        match submit_once(&shared, &client).await {
            Ok(()) => {
                if failures > 0 {
                    info!(target: "rcs_filter", endpoint = "/permission-requests/submit", attempts = failures, "RCS endpoint recovered");
                }
                failures = 0;
                backoff.reset();
            }
            Err(e) => {
                failures += 1;
                let delay = backoff.next_delay();
                shared.metrics.submit_retry_delay_seconds.set(delay.as_secs_f64());
                if failures == 1 || backoff.is_capped() {
                    warn!(target: "rcs_filter", endpoint = "/permission-requests/submit", attempt = failures, next_delay = ?delay, error_class = error_class(&e), error = %e, "batch submit failed; keeping NotSubmitted");
                }
                tokio::time::sleep(delay).await;
            }
        }
        if backoff.current == backoff.initial {
            shared.metrics.submit_retry_delay_seconds.set(0.0);
        }
    }
}

/// FR-5 adjudication poll loop.
async fn query_task(shared: Shared, client: Arc<dyn RcsClient>) {
    let mut backoff =
        RetryBackoff::new(shared.config.retry_initial_backoff, shared.config.retry_max_backoff);
    let mut failures = 0u64;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        match query_once(&shared, &client).await {
            Ok(()) => {
                if failures > 0 {
                    info!(target: "rcs_filter", endpoint = "/permission-requests/query", attempts = failures, "RCS endpoint recovered");
                }
                failures = 0;
                backoff.reset();
            }
            Err(e) => {
                failures += 1;
                let delay = backoff.next_delay();
                shared.metrics.query_retry_delay_seconds.set(delay.as_secs_f64());
                if failures == 1 || backoff.is_capped() {
                    warn!(target: "rcs_filter", endpoint = "/permission-requests/query", attempt = failures, next_delay = ?delay, error_class = error_class(&e), error = %e, "query failed; timeout fallback will apply");
                }
                tokio::time::sleep(delay).await;
            }
        }
        if backoff.current == backoff.initial {
            shared.metrics.query_retry_delay_seconds.set(0.0);
        }
    }
}

/// Queries the status of all in-flight txs and applies transitions. Absent/unrecognized
/// statuses leave the entry untouched (handled by the timeout task) — no optimistic pass.
pub(crate) async fn query_once(shared: &Shared, client: &Arc<dyn RcsClient>) -> crate::Result<()> {
    let (expected_generations, expired) = {
        let mut pool = shared.pool_lock();
        let now = shared.clock.now_unix();
        let expired = pool.resolve_total_timeouts(&shared.config, now);
        (pool.in_flight_generations(), expired)
    };
    shared.emit_timeout_resolutions(&expired);
    if !expired.is_empty() {
        shared.update_buffer_metric();
    }
    if expected_generations.is_empty() {
        return Ok(());
    }

    let mut first_error = None;
    for status in ["pending", "approved", "denied", "outdated"] {
        let started = std::time::Instant::now();
        let result = client.query(QueryParams::Status(status.to_string())).await;
        shared.metrics.record_request(RequestEndpoint::Query, started.elapsed(), &result);
        let resp = match result {
            Ok(resp) => resp,
            Err(error) => {
                let expired = {
                    let mut pool = shared.pool_lock();
                    let now = shared.clock.now_unix();
                    pool.resolve_total_timeouts(&shared.config, now)
                };
                shared.emit_timeout_resolutions(&expired);
                if !expired.is_empty() {
                    shared.update_buffer_metric();
                }
                first_error.get_or_insert(error);
                continue;
            }
        };
        // Parse, filter and deduplicate outside the pool lock. The resulting set is bounded by
        // this node's in-flight snapshot even if RCS returns an unexpectedly large response.
        let mut relevant = std::collections::BTreeMap::new();
        for tx in &resp.txs {
            let Ok(hash) = tx.tx_hash.parse() else {
                continue;
            };
            let Some(expected_generation) = expected_generations.get(&hash).copied() else {
                continue;
            };
            if let Some(reason) = &tx.reason {
                debug!(target: "rcs_filter", tx_hash = %tx.tx_hash, status = %tx.status, %reason, "query result");
            }
            let priority = match tx.status.as_str() {
                "denied" | "outdated" => 2,
                "approved" => 1,
                "pending" => 0,
                _ => continue,
            };
            let replace = relevant
                .get(&hash)
                .is_none_or(|(current_priority, _, _)| priority > *current_priority);
            if replace {
                relevant.insert(hash, (priority, expected_generation, tx));
            }
        }
        let terminal = {
            let mut pool = shared.pool_lock();
            let now = shared.clock.now_unix();
            let mut terminal = Vec::new();
            for (hash, (_, expected_generation, tx)) in relevant {
                // Deadline check and query transition happen under one pool lock. Events are
                // emitted only after the lock is released.
                if let Some(resolution) = pool.apply_query_status(
                    &hash,
                    expected_generation,
                    &tx.status,
                    &shared.config,
                    now,
                ) {
                    debug!(target: "rcs_filter", tx_hash = %tx.tx_hash, ?resolution, "query resolution");
                    let reason = match resolution {
                        QueryResolution::RetryTimeout(Resolution::Discard) => {
                            Some(TerminalReason::FailCloseTimeout)
                        }
                        QueryResolution::RetryTimeout(Resolution::ReleaseForPackaging) => None,
                        QueryResolution::Denied => Some(TerminalReason::Denied),
                        QueryResolution::Outdated => Some(TerminalReason::Outdated),
                    };
                    if let Some(reason) = reason {
                        terminal.push(TerminalEvent {
                            tx_hash: hash,
                            generation: expected_generation,
                            reason,
                        });
                    }
                }
            }
            terminal
        };
        for event in terminal {
            shared.emit_terminal(event);
        }
    }
    shared.update_buffer_metric();
    first_error.map_or(Ok(()), Err)
}

#[derive(Debug)]
struct RetryBackoff {
    initial: Duration,
    maximum: Duration,
    current: Duration,
    jitter_state: u64,
}

impl RetryBackoff {
    fn new(initial: Duration, maximum: Duration) -> Self {
        static SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let seed = SEED.fetch_add(0x9e3779b97f4a7c15, Ordering::Relaxed);
        Self::with_seed(initial, maximum, seed)
    }

    fn with_seed(initial: Duration, maximum: Duration, seed: u64) -> Self {
        Self { initial, maximum, current: initial, jitter_state: seed.max(1) }
    }

    fn next_delay(&mut self) -> Duration {
        let base = self.next_base_delay();
        self.jitter_state ^= self.jitter_state << 13;
        self.jitter_state ^= self.jitter_state >> 7;
        self.jitter_state ^= self.jitter_state << 17;
        // Equal jitter in [50%, 100%] keeps the configured maximum a hard cap.
        let base_nanos = base.as_nanos();
        let factor = 500u128 + u128::from(self.jitter_state % 501);
        let jittered = (base_nanos.saturating_mul(factor) / 1000).min(u64::MAX.into());
        Duration::from_nanos(jittered as u64)
    }

    fn next_base_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = self.current.saturating_mul(2).min(self.maximum);
        delay
    }

    fn reset(&mut self) {
        self.current = self.initial;
    }

    fn is_capped(&self) -> bool {
        self.current == self.maximum
    }
}

fn error_class(error: &crate::FilterError) -> &'static str {
    match error {
        crate::FilterError::Transport(_) => "transport",
        crate::FilterError::Timeout(_) => "timeout",
        crate::FilterError::UnexpectedStatus(_) => "unexpected_status",
        crate::FilterError::Decode(_) => "decode",
        crate::FilterError::UnsupportedProtocol(_) => "unsupported_protocol",
        crate::FilterError::Config(_) => "config",
    }
}

/// FR-6 timeout tick loop + terminal-tombstone eviction (bounds pool memory).
async fn timeout_task(shared: Shared) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let (_, pruned) = timeout_once(&shared);
        if pruned > 0 {
            debug!(target: "rcs_filter", pruned, "evicted expired terminal tombstones");
        }
    }
}

/// Executes one timeout/prune tick. Kept separate for deterministic state/event testing.
pub(crate) fn timeout_once(
    shared: &Shared,
) -> (Vec<(alloy_primitives::B256, u64, Resolution)>, usize) {
    let retention = shared.config.terminal_entry_retention.as_secs();
    let (resolved, pruned) = {
        let mut pool = shared.pool_lock();
        let now = shared.clock.now_unix();
        let resolved = pool.check_timeouts(&shared.config, now);
        let pruned = pool.prune_terminal(retention, now);
        (resolved, pruned)
    };
    shared.emit_timeout_resolutions(&resolved);
    shared.update_buffer_metric();
    (resolved, pruned)
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

    #[test]
    fn retry_schedule_is_exponential_capped_and_resets() {
        let mut backoff =
            RetryBackoff::with_seed(Duration::from_millis(200), Duration::from_secs(5), 7);
        let bases: Vec<_> = (0..7).map(|_| backoff.next_base_delay()).collect();
        assert_eq!(bases, [200, 400, 800, 1600, 3200, 5000, 5000].map(Duration::from_millis));
        backoff.reset();
        assert_eq!(backoff.next_base_delay(), Duration::from_millis(200));
    }

    #[test]
    fn retry_jitter_stays_between_half_base_and_cap() {
        let mut backoff =
            RetryBackoff::with_seed(Duration::from_millis(200), Duration::from_secs(5), 7);
        for base in [200, 400, 800, 1600, 3200, 5000] {
            let delay = backoff.next_delay();
            assert!(delay >= Duration::from_millis(base / 2));
            assert!(delay <= Duration::from_millis(base));
        }
    }
}
