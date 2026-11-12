//! Filter configuration. All values are process-local startup parameters — the master switch
//! and all timeouts are read once at startup and never hot-swapped at runtime.

use std::time::Duration;

/// Locally supported RCS `protocol_version` set. Membership is an **exact match**:
/// `protocol_version` only bumps on breaking schema changes, so there is no "partial
/// compatibility" middle state.
///
/// The set is deliberately **exactly** `[1]`. The `callstack` section is part of the version-one
/// rule schema, so there is no later version to tolerate: a snapshot or request declaring any
/// other version was authored against a different `callstack` contract, and adjudicating it under
/// this one would be a silent misinterpretation. A named rejection is strictly safer.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u32] = &[1];

/// The oldest rule protocol version this build accepts.
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// Hard safety ceiling for concurrent RCS submit requests from one filter instance.
pub const MAX_SUBMIT_CONCURRENCY: usize = 64;

/// Returns true iff `pv` is a `protocol_version` this build knows how to process.
pub fn is_supported_protocol(pv: u32) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&pv)
}

/// Fully-resolved filter configuration. Constructed from CLI args at node startup.
#[derive(Debug, Clone)]
pub struct FilterConfig {
    /// Master switch. When `false` the [`crate::FilterHandle`] is never constructed
    /// (`None`), so the hot path pays zero cost.
    pub enabled: bool,
    /// RCS REST base URL. Required when `enabled=true`.
    pub rcs_base_url: String,
    /// TCP connect timeout for each RCS request (default 1s).
    pub connect_timeout: Duration,
    /// Total timeout including response body decoding for each RCS request (default 3s).
    pub request_timeout: Duration,
    /// Initial retry delay for RCS worker failures (default 200ms).
    pub retry_initial_backoff: Duration,
    /// Maximum retry delay for RCS worker failures (default 5s).
    pub retry_max_backoff: Duration,
    /// Batch-submit accumulation window (default 200ms).
    pub batch_window: Duration,
    /// Maximum number of independent block-height submit groups in flight at once.
    /// `1` preserves the legacy height-ordered serial behavior.
    pub submit_max_concurrency: usize,
    /// `Submitted → NotSubmitted` timeout when a submitted tx is not confirmed by a
    /// query (default 8s).
    pub submitted_confirmation_timeout: Duration,
    /// `Pending → NotSubmitted` timeout when a pending tx sees no terminal status
    /// (including query absence) (default 20s).
    pub risk_module_unresponsive_timeout: Duration,
    /// Outer fallback: cumulative time from `first_not_submitted_at` after which the tx
    /// is resolved by its `audit_timeout_action` (default 90s). Must be greater than the RCS
    /// active/standby switch grace period.
    pub total_retry_timeout: Duration,
    /// `GET /rules/version` poll interval (default 2s).
    pub rules_version_poll_interval: Duration,
    /// How long a terminal buffer-pool tombstone (`TimedOutAllow`/`Dropped`) is retained
    /// before eviction, bounding pool memory (`terminal_entry_retention_seconds`, default 300s).
    /// Must exceed `total_retry_timeout` so a timeout tombstone outlives the full
    /// retry window. Non-terminal entries are bounded separately by builder-side reconciliation
    /// with txpool; `Approved` is intentionally not resolved by this retention setting.
    pub terminal_entry_retention: Duration,
    /// Maximum internal native-transfer observation candidates collected for one transaction
    /// during simulation. The Nth candidate is allowed; the (N+1)th overflows and fails the
    /// transaction closed. Must be `>= 1` (default 10_000).
    pub max_native_transfers_per_tx: usize,
    /// Maximum complete event bindings evaluated across all rules for one transaction. Replaces
    /// the historical fixed matcher ceiling; used identically in first screening and the
    /// post-approval consistency recompute. Must be `>= 1` (default 10_000).
    pub max_event_bindings_per_tx: usize,
    /// Maximum call-frame observations captured for one transaction during simulation. The Nth
    /// candidate is allowed; the (N+1)th overflows and fails the transaction closed. Bounds
    /// capture-side volume; `max_event_bindings_per_tx` separately bounds match-side
    /// combinatorics. Must be `>= 1` (default 10_000).
    pub max_callstack_observations_per_tx: usize,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rcs_base_url: String::new(),
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(3),
            retry_initial_backoff: Duration::from_millis(200),
            retry_max_backoff: Duration::from_secs(5),
            batch_window: Duration::from_millis(200),
            submit_max_concurrency: 1,
            submitted_confirmation_timeout: Duration::from_secs(8),
            risk_module_unresponsive_timeout: Duration::from_secs(20),
            total_retry_timeout: Duration::from_secs(90),
            rules_version_poll_interval: Duration::from_millis(2000),
            terminal_entry_retention: Duration::from_secs(300),
            max_native_transfers_per_tx: 10_000,
            max_event_bindings_per_tx: 10_000,
            max_callstack_observations_per_tx: 10_000,
        }
    }
}

impl FilterConfig {
    /// Validates the configuration. Enabling the filter without an `rcs_base_url` is a startup
    /// error.
    pub fn validate(&self) -> crate::Result<()> {
        if self.enabled && self.rcs_base_url.trim().is_empty() {
            return Err(crate::FilterError::Config(
                "rcs-filter enabled but rcs_base_url is empty".to_string(),
            ));
        }
        if self.enabled && self.terminal_entry_retention <= self.total_retry_timeout {
            return Err(crate::FilterError::Config(
                "rcs-filter terminal_entry_retention must exceed total_retry_timeout".to_string(),
            ));
        }
        if self.enabled
            && (self.connect_timeout.is_zero()
                || self.request_timeout.is_zero()
                || self.retry_initial_backoff.is_zero()
                || self.retry_max_backoff.is_zero()
                || self.batch_window.is_zero()
                || self.submitted_confirmation_timeout.is_zero()
                || self.risk_module_unresponsive_timeout.is_zero()
                || self.total_retry_timeout.is_zero()
                || self.rules_version_poll_interval.is_zero()
                || self.terminal_entry_retention.is_zero())
        {
            return Err(crate::FilterError::Config(
                "rcs-filter durations must be non-zero".to_string(),
            ));
        }
        if self.enabled && self.connect_timeout > self.request_timeout {
            return Err(crate::FilterError::Config(
                "rcs-filter connect_timeout must not exceed request_timeout".to_string(),
            ));
        }
        if self.enabled
            && (self.connect_timeout >= self.total_retry_timeout
                || self.request_timeout >= self.total_retry_timeout)
        {
            return Err(crate::FilterError::Config(
                "rcs-filter network timeouts must be less than total_retry_timeout".to_string(),
            ));
        }
        if self.enabled && self.retry_initial_backoff > self.retry_max_backoff {
            return Err(crate::FilterError::Config(
                "rcs-filter retry_initial_backoff must not exceed retry_max_backoff".to_string(),
            ));
        }
        if self.enabled && !(1..=MAX_SUBMIT_CONCURRENCY).contains(&self.submit_max_concurrency) {
            return Err(crate::FilterError::Config(format!(
                "rcs-filter submit_max_concurrency must be between 1 and {MAX_SUBMIT_CONCURRENCY}"
            )));
        }
        // Resource budgets are validated unconditionally (even when the filter is disabled): a
        // zero budget is always a startup misconfiguration, and startup must fail fast on it.
        if self.max_native_transfers_per_tx == 0 {
            return Err(crate::FilterError::Config(
                "rcs-filter max_native_transfers_per_tx must be >= 1".to_string(),
            ));
        }
        if self.max_event_bindings_per_tx == 0 {
            return Err(crate::FilterError::Config(
                "rcs-filter max_event_bindings_per_tx must be >= 1".to_string(),
            ));
        }
        if self.max_callstack_observations_per_tx == 0 {
            return Err(crate::FilterError::Config(
                "rcs-filter max_callstack_observations_per_tx must be >= 1".to_string(),
            ));
        }
        Ok(())
    }
}

#[path = "../tests/config__tests.rs"]
#[cfg(test)]
mod tests;
