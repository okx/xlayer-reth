//! Shared, EVM-free helpers of the `xlayer_auditTransactions` RPC: inspector choice,
//! capture-outcome mapping, and the request-level adjudicability decision that yields the
//! screening evidence.

use alloy_primitives::Log;

use crate::callstack::{ObservationUnavailable, Observations};
use crate::capture::{CaptureOutcome, ObservationBudget, RcsInspector, RcsInspectorConfig};
use crate::rules::RuleSet;

/// The capture inspector a request needs, or `None` when it declares no valid compiled rule — in
/// which case NO inspector is constructed at all and the caller keeps its plain EVM.
///
/// `NativeAndCallstack`: the audit path captures internal native transfers and call frames from the
/// same single inspection pass the payload builder uses, so a deposit is screened against the same
/// evidence either path would produce. It differs from the builder profile only in the
/// caller-supplied budgets. An event-only request still constructs the inspector: that is what
/// makes a virtual native-transfer log reachable for a request that declares no call-frame
/// selector. An empty rule set stays a plain EVM execution — no rule, no capture cost.
pub fn audit_inspector(
    rules: &RuleSet,
    max_native_transfers_per_tx: usize,
    max_callstack_observations_per_tx: usize,
) -> Option<RcsInspector> {
    (!rules.rules.is_empty()).then(|| {
        RcsInspector::new(RcsInspectorConfig::audit(
            rules,
            max_native_transfers_per_tx,
            max_callstack_observations_per_tx,
        ))
    })
}

/// Maps a capture outcome to the observation state screening must see. Every non-`Complete`
/// variant is an EXPLICIT unavailable reason — never `Available { items: [] }`, which would read
/// as "captured, nothing matched".
pub fn observations_from(outcome: &CaptureOutcome) -> Observations<'_> {
    match outcome {
        CaptureOutcome::Complete { observations, .. } => {
            Observations::available(observations.stamp, &observations.items)
        }
        CaptureOutcome::LimitExceeded { budget: ObservationBudget::Callstack, .. } => {
            Observations::Unavailable { reason: ObservationUnavailable::BudgetExceeded }
        }
        CaptureOutcome::LimitExceeded { .. } | CaptureOutcome::InvariantViolation { .. } => {
            // Defensive once the native budget is live: a native-budget overflow is an
            // unidentifiable capture, not an ignorable one. Screening rejects that outcome before
            // this mapping in the production path, so only the test-only recorder reaches it.
            Observations::Unavailable { reason: ObservationUnavailable::Incomplete }
        }
        CaptureOutcome::Passthrough => Observations::NOT_CAPTURED,
    }
}

/// The screening evidence for one transaction. Both fields always describe the SAME capture window
/// — the ordered real + virtual logs and the call-frame observations come from one outcome value,
/// so they can never describe different executions.
pub struct ScreenCapture<'a> {
    pub logs: &'a [Log],
    pub observations: Observations<'a>,
}

/// Decides adjudicability from the capture outcome first and returns the evidence to screen.
///
/// The match is on the outcome, not on which rule kinds the request declared. A capture that ran
/// always supplies its own screened stream, and every non-`Complete` outcome is unadjudicable —
/// never `allow` — including for a request that declares no call-frame selector. Only
/// `Passthrough` with an empty rule set is a legitimate no-capture request, and it screens the
/// plain EVM's raw execution logs; a `Passthrough` with rules means a rule-bearing request should
/// have had an inspector and is treated as a state-machine failure, failing closed rather than
/// silently substituting raw logs.
///
/// The stamp comparison is the same identity check the matcher applies per rule, hoisted to the
/// request level: an observation set produced under a different rule snapshot cannot distinguish
/// an absent frame from an unobserved one.
pub fn screen_capture<'a>(
    rules: &RuleSet,
    capture: &'a CaptureOutcome,
    result_logs: &'a [Log],
) -> Result<ScreenCapture<'a>, ObservationUnavailable> {
    match capture {
        // A capture that ran always supplies the screened stream, even if the request's rule set
        // is empty: the evidence comes from the capture, never from a substitute.
        CaptureOutcome::Complete { logs, observations } => {
            if observations.stamp != rules.stamp() {
                return Err(ObservationUnavailable::SnapshotMismatch);
            }
            Ok(ScreenCapture {
                logs,
                observations: Observations::available(observations.stamp, &observations.items),
            })
        }
        // `Passthrough` means NO inspector was built. That is only legitimate for a request with no
        // compiled rule at all; the plain EVM then screens the raw execution logs.
        CaptureOutcome::Passthrough if rules.rules.is_empty() => {
            Ok(ScreenCapture { logs: result_logs, observations: Observations::NOT_CAPTURED })
        }
        // A rule-bearing request must have had an inspector: reaching here is a state-machine bug.
        CaptureOutcome::Passthrough => Err(ObservationUnavailable::NotCaptured),
        // Non-`Complete` outcomes are unadjudicable regardless of which rule kinds were declared.
        CaptureOutcome::LimitExceeded { .. } => Err(ObservationUnavailable::BudgetExceeded),
        CaptureOutcome::InvariantViolation { .. } => Err(ObservationUnavailable::Incomplete),
    }
}
