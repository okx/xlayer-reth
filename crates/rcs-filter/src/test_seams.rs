//! Minimal in-process seams for `rcs-filter`'s own integration tests (`tests/`) and for dependent
//! crates' tests. Compiled only with the non-default `test-utils` feature, which is enabled from
//! `[dev-dependencies]` only — no production build contains any of it.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use crate::client::RcsClient;
use crate::clock::Clock;
use crate::config::FilterConfig;
use crate::handle::Shared;
use crate::pool::BufferPool;
use crate::rules::RuleSet;

/// The worker state the production `load_and_install` runs against, built exactly as the tests'
/// former in-crate fixtures built `Shared`.
#[derive(Clone)]
pub struct WorkerShared(Shared);

/// Builds the worker state: the given config, rule slot, clock and readiness, an empty buffer
/// pool, a terminal-event channel of `terminal_capacity` and default metrics.
pub fn worker_shared(
    config: FilterConfig,
    rules: RuleSet,
    clock: Arc<dyn Clock>,
    ready: bool,
    terminal_capacity: usize,
) -> WorkerShared {
    let (terminal_events, _) = tokio::sync::broadcast::channel(terminal_capacity);
    WorkerShared(Shared {
        config,
        rules: Arc::new(RwLock::new(Arc::new(rules))),
        pool: Arc::new(Mutex::new(BufferPool::default())),
        clock,
        ready: Arc::new(AtomicBool::new(ready)),
        terminal_events,
        metrics: crate::metrics::RcsFilterMetrics::default(),
    })
}

impl WorkerShared {
    /// The atomically hot-swapped rule slot.
    pub fn rules_slot(&self) -> &Arc<RwLock<Arc<RuleSet>>> {
        &self.0.rules
    }
}

/// Delegates to the production `worker::load_and_install`.
pub async fn load_and_install(
    shared: &WorkerShared,
    client: &Arc<dyn RcsClient>,
) -> crate::Result<bool> {
    crate::worker::load_and_install(&shared.0, client).await
}

/// The crate's metric handles, for a test that asserts the drop counters are separate series.
#[derive(Default)]
pub struct FilterMetrics(crate::metrics::RcsFilterMetrics);

impl FilterMetrics {
    pub fn timed_out_allow_recheck_drop_total(&self) -> &metrics::Counter {
        &self.0.timed_out_allow_recheck_drop_total
    }
    pub fn consistency_mismatch_total(&self) -> &metrics::Counter {
        &self.0.consistency_mismatch_total
    }
    pub fn drop_total(&self) -> &metrics::Counter {
        &self.0.drop_total
    }
    pub fn audit_approved_total(&self) -> &metrics::Counter {
        &self.0.audit_approved_total
    }
}

/// The rule-validation bounds the strict-schema tests pin.
pub const MAX_UNKNOWN_KEYS_REPORTED: usize = crate::rules::MAX_UNKNOWN_KEYS_REPORTED;
pub const MAX_UNKNOWN_KEY_BYTES: usize = crate::rules::MAX_UNKNOWN_KEY_BYTES;

/// Delegates to the private `rules::validation::bound_key`.
pub fn bound_key(key: &str) -> String {
    crate::rules::bound_key(key)
}
