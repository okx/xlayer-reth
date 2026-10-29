//! Operational signals for the audit RPC's call-frame identification.

use reth_metrics::{metrics::Counter, Metrics};

/// Identification signals for synthetic blacklist replay prefixes.
///
/// A dedicated `synthetic_ban` family, deliberately separate from both the pool path's
/// `rcs_filter` counters and the deposit path's `rcs_filter.deposit_identification` ones: a
/// replay prefix is the single class whose verdict and commit are unconditional, so its
/// identification is pure observation and must never be read as, or aggregated with, a series an
/// operator can act on.
#[derive(Metrics, Clone)]
#[metrics(scope = "rcs_filter.synthetic_ban_identification")]
pub struct SyntheticBanIdentificationMetrics {
    /// Prefixes identified with no rule objecting.
    pub clean_total: Counter,
    /// Prefixes whose identified code matched a deny-worthy rule. Reported only — a prefix is
    /// never denied, and its state is still committed.
    pub objected_total: Counter,
    /// Prefixes whose executed code could NOT be identified (capture unavailable, incomplete,
    /// over budget, or a snapshot-stamp mismatch). Never reported as "identified, nothing
    /// matched".
    pub not_identified_total: Counter,
    /// Prefixes in a request that declares no call-frame selector, where there is nothing to
    /// identify.
    pub not_applicable_total: Counter,
}

impl SyntheticBanIdentificationMetrics {
    /// The counter one classification increments.
    ///
    /// Returns the handle instead of incrementing inline so the classification → series mapping
    /// is assertable on its own, without standing up a metrics recorder.
    pub fn counter(&self, identification: super::verdict::CallstackIdentification) -> &Counter {
        use super::verdict::CallstackIdentification as Id;
        match identification {
            Id::Clean => &self.clean_total,
            Id::Objected => &self.objected_total,
            Id::NotIdentified => &self.not_identified_total,
            Id::NotApplicable => &self.not_applicable_total,
        }
    }
}
