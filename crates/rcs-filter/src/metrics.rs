use std::time::Duration;

use reth_metrics::{
    metrics::{Counter, Gauge, Histogram},
    Metrics,
};

use crate::FilterError;

#[derive(Debug, Clone, Copy)]
pub(crate) enum RequestEndpoint {
    Rules,
    RulesVersion,
    Submit,
    Query,
}

/// Operational signals for the RCS filter and its external dependency.
#[derive(Metrics, Clone)]
#[metrics(scope = "rcs_filter")]
pub(crate) struct RcsFilterMetrics {
    /// Transactions allowed by rule evaluation.
    pub allow_total: Counter,
    /// Transactions denied by rule evaluation.
    pub deny_total: Counter,
    /// Audit transactions deferred pending adjudication.
    pub audit_pending_total: Counter,
    /// Audit transactions released after approval.
    pub audit_approved_total: Counter,
    /// Transactions dropped from a terminal decision.
    pub drop_total: Counter,
    /// Approval consistency mismatches.
    pub consistency_mismatch_total: Counter,
    /// Total buffered transactions.
    pub buffered_transactions: Gauge,
    /// Buffered transactions awaiting submission.
    pub buffer_not_submitted: Gauge,
    /// Buffered transactions submitted to RCS.
    pub buffer_submitted: Gauge,
    /// Buffered transactions pending RCS adjudication.
    pub buffer_pending: Gauge,
    /// Buffered transactions approved by RCS.
    pub buffer_approved: Gauge,
    /// Buffered fail-open timeout tombstones.
    pub buffer_timed_out_allow: Gauge,
    /// Buffered discard tombstones.
    pub buffer_dropped: Gauge,
    /// Terminal discard events emitted to the builder.
    pub terminal_discard_events_total: Counter,
    /// Canonical buffered entries removed.
    pub canonical_cleanup_total: Counter,
    /// Canonical notifications skipped by a lagging receiver.
    pub canonical_channel_lag_total: Counter,
    /// Reusable lifecycle entries cleared after canonical receiver lag.
    pub canonical_lag_recovery_total: Counter,
    /// Terminal events skipped by a lagging receiver.
    pub terminal_channel_lag_total: Counter,
    /// Dropped entries scanned during reconciliation.
    pub terminal_reconciliation_total: Counter,
    /// Terminal discards that removed a txpool transaction.
    pub txpool_discard_success_total: Counter,
    /// Terminal discards whose transaction was already absent.
    pub txpool_discard_already_absent_total: Counter,
    /// Non-terminal lifecycle entries removed after their transaction left txpool.
    pub txpool_absent_cleanup_total: Counter,

    /// Whether the first supported rule snapshot has been installed (0 or 1).
    pub rules_ready: Gauge,
    /// Content version of the currently installed rule snapshot.
    pub rules_content_version: Gauge,

    /// Full rules endpoint requests.
    pub rules_requests_total: Counter,
    /// Full rules endpoint request latency.
    pub rules_request_duration_seconds: Histogram,
    /// Full rules endpoint transport errors.
    pub rules_transport_errors_total: Counter,
    /// Full rules endpoint request timeouts.
    pub rules_timeout_total: Counter,
    /// Full rules endpoint decode errors.
    pub rules_decode_errors_total: Counter,
    /// Full rules endpoint unexpected statuses.
    pub rules_unexpected_status_total: Counter,
    /// Rules version endpoint requests.
    pub rules_version_requests_total: Counter,
    /// Rules version endpoint request latency.
    pub rules_version_request_duration_seconds: Histogram,
    /// Rules version endpoint transport errors.
    pub rules_version_transport_errors_total: Counter,
    /// Rules version endpoint request timeouts.
    pub rules_version_timeout_total: Counter,
    /// Rules version endpoint decode errors.
    pub rules_version_decode_errors_total: Counter,
    /// Rules version endpoint unexpected statuses.
    pub rules_version_unexpected_status_total: Counter,
    /// Submit endpoint requests.
    pub submit_requests_total: Counter,
    /// Submit endpoint request latency.
    pub submit_request_duration_seconds: Histogram,
    /// Submit endpoint transport errors.
    pub submit_transport_errors_total: Counter,
    /// Submit endpoint request timeouts.
    pub submit_timeout_total: Counter,
    /// Submit endpoint decode errors.
    pub submit_decode_errors_total: Counter,
    /// Submit endpoint unexpected statuses.
    pub submit_unexpected_status_total: Counter,
    /// Query endpoint requests.
    pub query_requests_total: Counter,
    /// Query endpoint request latency.
    pub query_request_duration_seconds: Histogram,
    /// Query endpoint transport errors.
    pub query_transport_errors_total: Counter,
    /// Query endpoint request timeouts.
    pub query_timeout_total: Counter,
    /// Query endpoint decode errors.
    pub query_decode_errors_total: Counter,
    /// Query endpoint unexpected statuses.
    pub query_unexpected_status_total: Counter,

    /// Current rules-worker retry delay.
    pub rules_retry_delay_seconds: Gauge,
    /// Current submit-worker retry delay.
    pub submit_retry_delay_seconds: Gauge,
    /// Current query-worker retry delay.
    pub query_retry_delay_seconds: Gauge,
}

impl RcsFilterMetrics {
    pub(crate) fn record_request<T>(
        &self,
        endpoint: RequestEndpoint,
        elapsed: Duration,
        result: &crate::Result<T>,
    ) {
        let (requests, latency, transport, timeout, decode, status) = match endpoint {
            RequestEndpoint::Rules => (
                &self.rules_requests_total,
                &self.rules_request_duration_seconds,
                &self.rules_transport_errors_total,
                &self.rules_timeout_total,
                &self.rules_decode_errors_total,
                &self.rules_unexpected_status_total,
            ),
            RequestEndpoint::RulesVersion => (
                &self.rules_version_requests_total,
                &self.rules_version_request_duration_seconds,
                &self.rules_version_transport_errors_total,
                &self.rules_version_timeout_total,
                &self.rules_version_decode_errors_total,
                &self.rules_version_unexpected_status_total,
            ),
            RequestEndpoint::Submit => (
                &self.submit_requests_total,
                &self.submit_request_duration_seconds,
                &self.submit_transport_errors_total,
                &self.submit_timeout_total,
                &self.submit_decode_errors_total,
                &self.submit_unexpected_status_total,
            ),
            RequestEndpoint::Query => (
                &self.query_requests_total,
                &self.query_request_duration_seconds,
                &self.query_transport_errors_total,
                &self.query_timeout_total,
                &self.query_decode_errors_total,
                &self.query_unexpected_status_total,
            ),
        };
        requests.increment(1);
        latency.record(elapsed.as_secs_f64());
        if let Err(error) = result {
            match error {
                FilterError::Transport(_) => transport.increment(1),
                FilterError::Timeout(_) => timeout.increment(1),
                FilterError::Decode(_) => decode.increment(1),
                FilterError::UnexpectedStatus(_) => status.increment(1),
                FilterError::UnsupportedProtocol(_) | FilterError::Config(_) => {}
            }
        }
    }
}
