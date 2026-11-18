//! Shared, EVM-free codeHash helpers of the `xlayer_auditTransactions` RPC: inspector choice,
//! capture-outcome mapping, and the request-level unadjudicable gate.

use crate::callstack::{ObservationUnavailable, Observations};
use crate::capture::{CallstackCaptureConfig, CaptureOutcome, ObservationBudget, RcsInspector};
use crate::rules::RuleSet;

/// The capture inspector a request needs, or `None` when it declares no call-frame selector — in
/// which case NO inspector is constructed at all and the caller keeps its plain EVM.
///
/// `CallstackOnly`: the audit path produces and consumes no virtual native-transfer log, screens
/// against the raw execution logs it already used, and the only live budget is the caller's
/// callstack observation budget, applied PER TRANSACTION inside the batch.
pub fn callstack_only_inspector(
    rules: &RuleSet,
    max_callstack_observations_per_tx: usize,
) -> Option<RcsInspector> {
    rules.has_callstack_rules().then(|| {
        RcsInspector::callstack_only(
            max_callstack_observations_per_tx,
            CallstackCaptureConfig::from_rules(rules),
        )
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
        // The native budget is not applicable in `CallstackOnly` mode and is never consulted, so
        // reaching it would be a capture-state bug; it is unidentifiable, not ignorable.
        CaptureOutcome::LimitExceeded { .. } | CaptureOutcome::InvariantViolation { .. } => {
            Observations::Unavailable { reason: ObservationUnavailable::Incomplete }
        }
        CaptureOutcome::Passthrough => Observations::NOT_CAPTURED,
    }
}

/// Why one transaction cannot be adjudicated AT ALL against the request's rules, decided from the
/// request's own compiled set before a single rule is filtered or matched.
///
/// This has to be a request-level question, not a per-rule one. Matching skips a rule narrowed by
/// `contract_address` / `origin` with zero decoding, so for such a rule the per-rule observation
/// check is never reached and an unidentifiable execution would fall through to an authoritative
/// `Allow` — exactly what the highest-priority rule forbids. `has_callstack_rules()` keeps a
/// request that declares no selector byte-identical to its previous behaviour: nothing is
/// declared, so nothing can be unidentifiable.
///
/// The stamp comparison mirrors the matcher's own: an observation set produced under a different
/// rule snapshot cannot distinguish an absent frame from an unobserved one, so it is unavailable
/// rather than empty.
pub fn unadjudicable_observations(
    rules: &RuleSet,
    observations: Observations<'_>,
) -> Option<ObservationUnavailable> {
    if !rules.has_callstack_rules() {
        return None;
    }
    match observations {
        Observations::Unavailable { reason } => Some(reason),
        Observations::Available { stamp, .. } if stamp != rules.stamp() => {
            Some(ObservationUnavailable::SnapshotMismatch)
        }
        Observations::Available { .. } => None,
    }
}
