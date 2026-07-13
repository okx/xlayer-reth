//! Filter configuration (TD §4.3). All values are process-local startup parameters —
//! the master switch and all timeouts are read once at startup and never hot-swapped
//! at runtime (TD §4.3 invariant).

use std::time::Duration;

/// Locally supported RCS `protocol_version` set. Membership is an **exact match**
/// (contract §2.1 disambiguation): `protocol_version` only bumps on breaking schema
/// changes, so there is no "partial compatibility" middle state.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u32] = &[1];

/// Returns true iff `pv` is a `protocol_version` this build knows how to process.
pub fn is_supported_protocol(pv: u32) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&pv)
}

/// Fully-resolved filter configuration. Constructed from CLI args at node startup.
#[derive(Debug, Clone)]
pub struct FilterConfig {
    /// FR-9 master switch. When `false` the [`crate::FilterHandle`] is never constructed
    /// (`None`), so the hot path pays zero cost.
    pub enabled: bool,
    /// RCS REST base URL (FR-2/FR-5). Required when `enabled=true`.
    pub rcs_base_url: String,
    /// Batch-submit accumulation window (FR-5, default 200ms).
    pub batch_window: Duration,
    /// `Submitted → NotSubmitted` timeout when a submitted tx is not confirmed by a
    /// query (FR-6, default 8s).
    pub submitted_confirmation_timeout: Duration,
    /// `Pending → NotSubmitted` timeout when a pending tx sees no terminal status
    /// (including query absence) (FR-6, default 20s).
    pub risk_module_unresponsive_timeout: Duration,
    /// Outer fallback: cumulative time from `first_not_submitted_at` after which the tx
    /// is resolved by its `audit_timeout_action` (FR-6, default 90s). Must be greater
    /// than the RCS active/standby switch grace period (deployment invariant, TD §7 R-1).
    pub total_retry_timeout: Duration,
    /// `GET /rules/version` poll interval (FR-3, default 2s).
    pub rules_version_poll_interval: Duration,
    /// How long a terminal buffer-pool tombstone (`ReleasePending`/`Dropped`) is retained
    /// before eviction, bounding pool memory (contract §2.5 `terminal_entry_retention_seconds`,
    /// default 300s). Must exceed `total_retry_timeout` so a tombstone outlives the full
    /// adjudication window (every entry is guaranteed terminal within `total_retry_timeout`),
    /// leaving a margin before a still-mempooled duplicate could be re-screened.
    pub terminal_entry_retention: Duration,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rcs_base_url: String::new(),
            batch_window: Duration::from_millis(200),
            submitted_confirmation_timeout: Duration::from_secs(8),
            risk_module_unresponsive_timeout: Duration::from_secs(20),
            total_retry_timeout: Duration::from_secs(90),
            rules_version_poll_interval: Duration::from_millis(2000),
            terminal_entry_retention: Duration::from_secs(300),
        }
    }
}

impl FilterConfig {
    /// Validates the configuration. When `enabled` the `rcs_base_url` must be non-empty
    /// (FR-9 AC2: enabling the switch without a URL is a startup error).
    pub fn validate(&self) -> crate::Result<()> {
        if self.enabled && self.rcs_base_url.trim().is_empty() {
            return Err(crate::FilterError::Config(
                "xlayer-filter enabled but rcs_base_url is empty".to_string(),
            ));
        }
        if self.enabled && self.terminal_entry_retention <= self.total_retry_timeout {
            return Err(crate::FilterError::Config(
                "xlayer-filter terminal_entry_retention must exceed total_retry_timeout"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_td_4_3() {
        let c = FilterConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.batch_window, Duration::from_millis(200));
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
}
