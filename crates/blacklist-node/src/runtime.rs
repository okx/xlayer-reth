//! FR-6/FR-7 — runtime context: snapshot handle, chain-id dispatch, metrics emission.
//!
//! [`BlacklistRuntimeCtx`] is the shared state threaded into the ingress validator and
//! the three execution faces. It owns the atomically-replaceable per-block snapshot
//! (`Arc<ArcSwap<BlacklistSnapshot>>`, [TD §4.2]) and the chain-id→mirror dispatch
//! ([`xlayer_blacklist::mirror_address_for_chain`], FR-6), and is the single place that
//! emits the `xlayer_blacklist_*` Prometheus metrics (FR-7).
//!
//! Dependencies: `arc-swap`, `alloy-primitives`, `xlayer-blacklist`, `reth-metrics`/`metrics`.

use alloy_primitives::Address;
use arc_swap::ArcSwap;
use std::sync::Arc;
use xlayer_blacklist::eval::Hit;
use xlayer_blacklist::metrics::BlacklistMetrics;
use xlayer_blacklist::{mirror_address_for_chain, BlacklistSnapshot};

/// Shared, atomically-replaceable handle to the current block's blacklist snapshot.
///
/// Read on every ingress tx and every execution face; replaced wholesale once per block
/// (commit) and rebuilt on reorg. `ArcSwap` gives lock-free reads + atomic replace.
pub type SnapshotHandle = Arc<ArcSwap<BlacklistSnapshot>>;

/// Per-node runtime context for the blacklist feature.
///
/// Cloneable so it can be shared across the validator, pool maintenance task, and the
/// executor wrapper without further `Arc` wrapping (the snapshot handle and metrics are
/// already shared internally).
#[derive(Clone)]
pub struct BlacklistRuntimeCtx {
    chain_id: u64,
    mirror: Option<Address>,
    snapshot: SnapshotHandle,
    metrics: BlacklistMetrics,
}

// `BlacklistMetrics` (reth_metrics `Metrics` derive) does not implement `Debug`, so a
// `derive(Debug)` here would not compile. Provide a manual impl over the inspectable
// fields; the metric handles carry no useful debug state.
impl core::fmt::Debug for BlacklistRuntimeCtx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BlacklistRuntimeCtx")
            .field("chain_id", &self.chain_id)
            .field("mirror", &self.mirror)
            .field("snapshot", &self.snapshot)
            .finish_non_exhaustive()
    }
}

impl BlacklistRuntimeCtx {
    /// Build a runtime context for the given chain id. The feature is enabled iff the
    /// chain id resolves to a mirror address (FR-6); otherwise every read is a no-op.
    pub fn new(chain_id: u64) -> Self {
        Self {
            chain_id,
            mirror: mirror_address_for_chain(chain_id),
            snapshot: Arc::new(ArcSwap::from_pointee(BlacklistSnapshot::empty())),
            metrics: BlacklistMetrics::default(),
        }
    }

    /// Whether the feature is active on this chain (mirror address resolved). [FR-6]
    pub fn is_enabled(&self) -> bool {
        self.mirror.is_some()
    }

    /// The running chain id.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// The resolved mirror contract address, if any.
    pub fn mirror(&self) -> Option<Address> {
        self.mirror
    }

    /// A clone of the shared snapshot handle (for the validator / pool task).
    pub fn snapshot_handle(&self) -> SnapshotHandle {
        Arc::clone(&self.snapshot)
    }

    /// Load the current snapshot (cheap atomic load).
    pub fn load_snapshot(&self) -> Arc<BlacklistSnapshot> {
        self.snapshot.load_full()
    }

    /// Atomically replace the snapshot (called once per block / on reorg) and update the
    /// `xlayer_blacklist_cache_size` gauge. [DM-1.8 / DM-1.9]
    pub fn store_snapshot(&self, snapshot: BlacklistSnapshot) {
        self.metrics.cache_size.set(snapshot.len() as f64);
        self.snapshot.store(Arc::new(snapshot));
    }

    /// `xlayer_blacklist_pool_rejected_total += 1` (ingress reject). [DM-7.1]
    pub fn record_pool_reject(&self) {
        self.metrics.pool_rejected.increment(1);
    }

    /// `xlayer_blacklist_exec_revert_total{hook=<category>} += 1` (exec-gate hit). [DM-7.2/7.4]
    pub fn record_exec_revert(&self, hit: &Hit) {
        BlacklistMetrics::increment_exec_revert(hit.category);
    }

    /// Observe a block-head snapshot read duration into the histogram. [DM-7.3]
    pub fn record_snapshot_read(&self, seconds: f64) {
        self.metrics.snapshot_read_duration_seconds.record(seconds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn devnet_is_enabled_unrecognized_is_not() {
        assert!(BlacklistRuntimeCtx::new(195).is_enabled());
        assert!(!BlacklistRuntimeCtx::new(10).is_enabled());
    }

    #[test]
    fn snapshot_store_then_load_roundtrips() {
        let ctx = BlacklistRuntimeCtx::new(195);
        assert!(ctx.load_snapshot().is_empty());
        let snap: BlacklistSnapshot =
            [address!("00000000000000000000000000000000000000aa")].into_iter().collect();
        ctx.store_snapshot(snap);
        assert_eq!(ctx.load_snapshot().len(), 1);
    }
}
