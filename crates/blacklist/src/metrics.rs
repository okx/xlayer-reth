//! FR-7 — Prometheus metrics for the blacklist feature.
//!
//! Static-name metrics use the `reth_metrics` derive (matching the repo's
//! `BuilderMetrics` pattern). The category-labelled `exec_revert_total` uses a dynamic
//! label, which the derive does not support, so it is emitted directly via the `metrics`
//! facade. [TD §4.7]

use crate::eval::HitCategory;
use reth_metrics::{
    metrics::{Counter, Gauge, Histogram},
    Metrics,
};

/// Blacklist metrics under the `xlayer_blacklist` scope.
#[derive(Metrics, Clone)]
#[metrics(scope = "xlayer_blacklist")]
pub struct BlacklistMetrics {
    /// Current block snapshot size → `xlayer_blacklist_cache_size`.
    pub cache_size: Gauge,
    /// Ingress mempool rejection count → `xlayer_blacklist_pool_rejected_total`.
    pub pool_rejected: Counter,
    /// Block-head snapshot read duration → `xlayer_blacklist_snapshot_read_duration_seconds`.
    pub snapshot_read_duration_seconds: Histogram,
}

/// Metric name for the category-labelled execution-gate revert counter.
const EXEC_REVERT_TOTAL: &str = "xlayer_blacklist_exec_revert_total";

impl BlacklistMetrics {
    /// Increment `xlayer_blacklist_exec_revert_total{hook=<category>}` by one.
    ///
    /// Uses the `metrics` facade directly because the `hook` label is dynamic (the derive
    /// macro only supports static metric definitions). [TD §4.7]
    pub fn increment_exec_revert(category: HitCategory) {
        metrics::counter!(EXEC_REVERT_TOTAL, "hook" => category.metric_label()).increment(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_struct_constructs() {
        // The derive provides Default which registers the metrics; just ensure it builds.
        let _m = BlacklistMetrics::default();
    }

    #[test]
    fn exec_revert_labels_cover_all_categories() {
        // No panic emitting each category label.
        BlacklistMetrics::increment_exec_revert(HitCategory::Call);
        BlacklistMetrics::increment_exec_revert(HitCategory::Log);
        BlacklistMetrics::increment_exec_revert(HitCategory::EthBalance);
        BlacklistMetrics::increment_exec_revert(HitCategory::SelfDestruct);
    }
}
