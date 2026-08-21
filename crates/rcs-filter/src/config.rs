//! Filter configuration. All values are process-local startup parameters — the master switch
//! and all timeouts are read once at startup and never hot-swapped at runtime.

use std::time::Duration;

/// Locally supported RCS `protocol_version` set. Membership is an **exact match**:
/// `protocol_version` only bumps on breaking schema changes, so there is no "partial
/// compatibility" middle state.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u32] = &[1];

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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_expected_values() {
        let c = FilterConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.batch_window, Duration::from_millis(200));
        assert_eq!(c.submit_max_concurrency, 1);
        assert_eq!(c.connect_timeout, Duration::from_secs(1));
        assert_eq!(c.request_timeout, Duration::from_secs(3));
        assert_eq!(c.retry_initial_backoff, Duration::from_millis(200));
        assert_eq!(c.retry_max_backoff, Duration::from_secs(5));
        assert_eq!(c.submitted_confirmation_timeout, Duration::from_secs(8));
        assert_eq!(c.risk_module_unresponsive_timeout, Duration::from_secs(20));
        assert_eq!(c.total_retry_timeout, Duration::from_secs(90));
        assert_eq!(c.rules_version_poll_interval, Duration::from_millis(2000));
        assert_eq!(c.terminal_entry_retention, Duration::from_secs(300));
    }

    #[test]
    fn retention_must_exceed_total_retry_timeout() {
        let base =
            FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };
        // Equal → rejected.
        let eq = FilterConfig { terminal_entry_retention: Duration::from_secs(90), ..base.clone() };
        assert!(eq.validate().is_err());
        // Strictly less → rejected.
        let lt = FilterConfig { terminal_entry_retention: Duration::from_secs(30), ..base.clone() };
        assert!(lt.validate().is_err());
        // Strictly greater → accepted.
        let gt = FilterConfig { terminal_entry_retention: Duration::from_secs(91), ..base.clone() };
        assert!(gt.validate().is_ok());
        // Disabled → the check is skipped even with an otherwise-invalid retention.
        let disabled = FilterConfig {
            enabled: false,
            terminal_entry_retention: Duration::from_secs(1),
            ..base
        };
        assert!(disabled.validate().is_ok());
    }

    #[test]
    fn protocol_version_is_exact_match() {
        assert!(is_supported_protocol(1));
        assert!(!is_supported_protocol(2));
        assert!(!is_supported_protocol(0));
    }

    #[test]
    fn enabled_requires_base_url() {
        let c = FilterConfig { enabled: true, ..Default::default() };
        assert!(c.validate().is_err());
        let c =
            FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };
        assert!(c.validate().is_ok());
    }

    #[test]
    fn network_timeout_and_backoff_bounds_are_validated() {
        let base =
            FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };
        assert!(FilterConfig { connect_timeout: Duration::ZERO, ..base.clone() }
            .validate()
            .is_err());
        assert!(FilterConfig {
            connect_timeout: Duration::from_secs(4),
            request_timeout: Duration::from_secs(3),
            ..base.clone()
        }
        .validate()
        .is_err());
        assert!(FilterConfig {
            retry_initial_backoff: Duration::from_secs(6),
            retry_max_backoff: Duration::from_secs(5),
            ..base
        }
        .validate()
        .is_err());
    }

    #[test]
    fn scheduling_and_state_durations_must_be_non_zero() {
        let base =
            FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };
        for invalid in [
            FilterConfig { batch_window: Duration::ZERO, ..base.clone() },
            FilterConfig { submitted_confirmation_timeout: Duration::ZERO, ..base.clone() },
            FilterConfig { risk_module_unresponsive_timeout: Duration::ZERO, ..base.clone() },
            FilterConfig { total_retry_timeout: Duration::ZERO, ..base.clone() },
            FilterConfig { rules_version_poll_interval: Duration::ZERO, ..base.clone() },
            FilterConfig { terminal_entry_retention: Duration::ZERO, ..base.clone() },
        ] {
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn submit_concurrency_boundaries_are_validated_when_enabled() {
        let base =
            FilterConfig { enabled: true, rcs_base_url: "http://rcs".into(), ..Default::default() };

        assert!(FilterConfig { submit_max_concurrency: 0, ..base.clone() }.validate().is_err());
        assert!(FilterConfig { submit_max_concurrency: 1, ..base.clone() }.validate().is_ok());
        assert!(FilterConfig { submit_max_concurrency: 4, ..base.clone() }.validate().is_ok());
        assert!(FilterConfig { submit_max_concurrency: MAX_SUBMIT_CONCURRENCY, ..base.clone() }
            .validate()
            .is_ok());
        assert!(FilterConfig { submit_max_concurrency: MAX_SUBMIT_CONCURRENCY + 1, ..base }
            .validate()
            .is_err());
    }

    #[test]
    fn disabled_filter_skips_submit_concurrency_validation() {
        let config = FilterConfig { submit_max_concurrency: 0, ..Default::default() };
        assert!(config.validate().is_ok());
    }
}
