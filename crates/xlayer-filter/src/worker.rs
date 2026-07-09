//! Background workers (TD §4.7/§4.9). All RCS network IO lives here; the hot path
//! ([`crate::handle::FilterHandle::screen_tx`]) never blocks on the network.
//!
//! Four cooperating tokio tasks:
//! - **rules**: FR-2 blocking startup load (unbounded exponential backoff, no default
//!   rules) then FR-3 hot-reload (`content_version`-triggered atomic swap).
//! - **submit**: FR-5 batch submit every `batch_window`.
//! - **query**: FR-5 adjudication poll, mapping RCS status → buffer transitions.
//! - **timeout**: FR-6 timeout tick (outer 90s fallback + 8s/20s stalls).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

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

/// Spawns all background workers. No-op when the filter is disabled (FR-9).
pub(crate) fn spawn(shared: Shared, client: Arc<dyn RcsClient>) {
    if !shared.config.enabled {
        return;
    }
    tokio::spawn(rules_task(shared.clone(), client.clone()));
    tokio::spawn(submit_task(shared.clone(), client.clone()));
    tokio::spawn(query_task(shared.clone(), client.clone()));
    tokio::spawn(timeout_task(shared));
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

    // FR-3: poll `content_version`; only pull on change.
    let interval = shared.config.rules_version_poll_interval;
    loop {
        tokio::time::sleep(interval).await;
        let current = shared.rules.read().expect("rules lock").content_version;
        match client.get_rules_version().await {
            Ok(v) if v.content_version != current => {
                // Full pull + atomic swap. Unsupported protocol_version is rejected here
                // (keep old rules, keep mining) — distinct from the startup block.
                if let Err(e) = load_and_install(&shared, &client).await {
                    warn!(target: "xlayer_filter", error = %e, "hot-reload rule pull failed; keeping current rules");
                }
            }
            Ok(_) => {}
            Err(e) => {
                debug!(target: "xlayer_filter", error = %e, "rules/version probe failed; keeping current rules")
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
            target: "xlayer_filter",
            protocol_version = resp.protocol_version,
            "unsupported protocol_version; rejecting rule set"
        );
        return Ok(false);
    }
    let set = load_rules(resp.protocol_version, resp.content_version, resp.rules);
    *shared.rules.write().expect("rules lock") = Arc::new(set);
    shared.ready.store(true, Ordering::Release);
    Ok(true)
}

/// FR-5 batch-submit loop.
async fn submit_task(shared: Shared, client: Arc<dyn RcsClient>) {
    loop {
        tokio::time::sleep(shared.config.batch_window).await;
        if let Err(e) = submit_once(&shared, &client).await {
            warn!(target: "xlayer_filter", error = %e, "batch submit failed; keeping NotSubmitted");
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
        let pool = shared.pool.lock().expect("pool lock");
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
            warn!(target: "xlayer_filter", tx_hash = %rejected, "submit rejected_malformed; retrying");
        }
        let now = shared.clock.now_unix();
        shared.pool.lock().expect("pool lock").apply_submit_response(&resp.accepted, now);
    }
    Ok(())
}

/// FR-5 adjudication poll loop.
async fn query_task(shared: Shared, client: Arc<dyn RcsClient>) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        if let Err(e) = query_once(&shared, &client).await {
            debug!(target: "xlayer_filter", error = %e, "query failed; timeout fallback will apply");
        }
    }
}

/// Queries the status of all in-flight txs and applies transitions. Absent/unrecognized
/// statuses leave the entry untouched (handled by the timeout task) — no optimistic pass.
pub(crate) async fn query_once(shared: &Shared, client: &Arc<dyn RcsClient>) -> crate::Result<()> {
    let in_flight: Vec<String> = {
        let pool = shared.pool.lock().expect("pool lock");
        pool.in_flight_hashes()
    };
    if in_flight.is_empty() {
        return Ok(());
    }

    let resp = client.query(QueryParams::TxHashes(in_flight)).await?;
    let now = shared.clock.now_unix();
    let mut pool = shared.pool.lock().expect("pool lock");
    for tx in &resp.txs {
        if let Ok(hash) = tx.tx_hash.parse() {
            if let Some(reason) = &tx.reason {
                debug!(target: "xlayer_filter", tx_hash = %tx.tx_hash, status = %tx.status, %reason, "query result");
            }
            // `denied`/`outdated` tombstone the entry as `Dropped` in place (G3) — the entry
            // is intentionally NOT removed, so the tx is neither re-buffered nor re-submitted.
            if let Some(resolution) = pool.apply_query_status(&hash, &tx.status, now) {
                debug!(target: "xlayer_filter", tx_hash = %tx.tx_hash, ?resolution, "query resolution");
            }
        }
    }
    Ok(())
}

/// FR-6 timeout tick loop.
async fn timeout_task(shared: Shared) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let now = shared.clock.now_unix();
        let resolved = shared.pool.lock().expect("pool lock").check_timeouts(&shared.config, now);
        for (hash, resolution) in resolved {
            debug!(target: "xlayer_filter", tx_hash = %format!("{hash:#x}"), ?resolution, "timeout resolution");
        }
    }
}
