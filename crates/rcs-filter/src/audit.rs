//! Pure adjudication helpers of the `xlayer_auditTransactions` RPC.
//!
//! Everything here is EVM-free and RPC-free: the RPC crate keeps the JSON-RPC handler, the error
//! shaping, the EVM construction and the per-transaction execution loop, and calls into this
//! module for the decisions that only need the request's own rules, the capture outcome and the
//! bytes of the built deposit.

pub mod synthetic_ban;

use std::collections::{BTreeMap, HashSet};

use alloy_primitives::B256;

use crate::callstack::{ObservationUnavailable, Observations};
use crate::capture::{CallstackCaptureConfig, CaptureOutcome, ObservationBudget, RcsInspector};
use crate::config::MIN_PROTOCOL_VERSION;
use crate::rules::{load_rules, RawRule, RuleSet};

/// What a `known_allowed` entry IS.
///
/// The two classes have **opposite safe defaults** — Deny-priority is safe for an `Original` and
/// unsafe for a `SyntheticBan`, and committing is safe for a `SyntheticBan` and unsafe for an
/// `Original` — so the class is never guessed from `to`, position or any other heuristic. It is
/// decided per entry, in this order:
///
/// 1. **Label map present** (`known_allowed_kinds` is `Some`): the caller's label is used for
///    every entry and nothing is inferred. A partial map is rejected at the boundary.
/// 2. **No label map**: the entry's built deposit is checked against RCS's v1 synthetic-ban
///    derivation ([`synthetic_ban::verify_synthetic_ban`]). A match is a `SyntheticBan`; anything
///    else is an `Original`.
///
/// Step 2 is a **content binding, not source authentication**. It proves only that the entry's
/// own bytes match the derivation; anyone authoring a request can build such an entry. It is
/// sound for a caller that derives every original's `source_hash` from the real L1 log and builds
/// ban prefixes only through that derivation, which is how RCS builds its requests. A matching
/// entry gets exactly the `Allow` + commit every unlabelled entry used to get, so the check grants
/// no caller anything it did not already have. For any other caller that sends `known_allowed`
/// without labels this is a behaviour change: an entry that does not match is now re-adjudicated
/// on the call-frame dimension and can be `Deny` or the non-verdict, with nothing committed. Such
/// a caller keeps the unconditional `Allow` + commit for an entry by labelling it `SyntheticBan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownAllowedKind {
    /// An ordinary deposit the caller's adjudicator already resolved to Allow.
    Original,
    /// A control prefix the caller prepended for blacklist replay. It is authored by the caller
    /// itself to rebuild base state it already decided, not an unadjudicated user payload.
    SyntheticBan,
}

/// How a `known_allowed` entry's class was decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownAllowedResolution {
    /// Read from the request's label map.
    Labelled,
    /// Decided by the content-binding check, because the request supplied no label map.
    Inferred,
}

/// The class of an unlabelled `known_allowed` entry whose built transaction is a deposit, decided
/// from that deposit's own `source_hash` and `input` alone.
///
/// The caller passes the bytes `build_deposit_tx` decoded once from the request, so every
/// spelling of one request decodes to one class. A transaction that is not a deposit cannot be a
/// synthetic ban and never reaches this function: the caller classifies it as `Original`.
pub fn deposit_known_allowed_kind(source_hash: B256, input: &[u8]) -> KnownAllowedKind {
    if synthetic_ban::verify_synthetic_ban(source_hash, input) {
        KnownAllowedKind::SyntheticBan
    } else {
        KnownAllowedKind::Original
    }
}

/// `Some(kind)` when `source_hash` is a `known_allowed` entry.
///
/// A request with a label map is read from it: a partial map was already refused at the boundary,
/// so every entry has a label and nothing is inferred (an entry missing from a validated map
/// cannot occur; it would be adjudicated as an ordinary deposit). Without a map the class comes
/// from `infer`, which is called only in that case (see [`KnownAllowedKind`]).
pub fn resolve_known_allowed_kind(
    known_allowed: &HashSet<String>,
    kinds: Option<&BTreeMap<String, KnownAllowedKind>>,
    source_hash: &str,
    infer: impl FnOnce() -> KnownAllowedKind,
) -> Option<(KnownAllowedKind, KnownAllowedResolution)> {
    if !known_allowed.contains(source_hash) {
        return None;
    }
    match kinds {
        Some(kinds) => {
            kinds.get(source_hash).copied().map(|kind| (kind, KnownAllowedResolution::Labelled))
        }
        None => Some((infer(), KnownAllowedResolution::Inferred)),
    }
}

/// The `known_allowed` entries a supplied `known_allowed_kinds` map does not label, sorted and
/// deduplicated. Empty when no map is supplied or there are no entries.
///
/// Labels are optional, and that is load-bearing: a caller that sends no map at all is the
/// ordinary case, and refusing it would make every round that carries a non-empty
/// `known_allowed` fail permanently. A **partial** map is a different thing: a caller that labels
/// some entries and not others is label-aware, so the omission is ambiguous rather than
/// conventional, and the node does not silently pick a class for the remainder.
pub fn unlabelled_known_allowed<'a>(
    known_allowed: &'a [String],
    kinds: Option<&BTreeMap<String, KnownAllowedKind>>,
) -> Vec<&'a str> {
    if kinds.is_none() || known_allowed.is_empty() {
        return Vec::new();
    }
    let mut unlabelled: Vec<&str> = known_allowed
        .iter()
        .filter(|hash| !kinds.is_some_and(|kinds| kinds.contains_key(hash.as_str())))
        .map(String::as_str)
        .collect();
    unlabelled.sort_unstable();
    unlabelled.dedup();
    unlabelled
}

/// The callstack-only projection of a request's rules, used ONLY to adjudicate a `known_allowed`
/// `Original`. `None` when the request declares no selector, where there is nothing to adjudicate
/// and the pre-existing unconditional `Allow` applies unchanged.
///
/// Filtering out rules with no `callstack` section leaves the selector address set — and
/// therefore the `SnapshotStamp` — unchanged, so capture and screening still provably share one
/// configuration and the existing stamp check keeps its meaning.
pub fn callstack_only_projection(rules: &RuleSet, raw: &[RawRule]) -> Option<RuleSet> {
    if !rules.has_callstack_rules() {
        return None;
    }
    let projected: Vec<RawRule> = raw
        .iter()
        .filter(|rule| rule.callstack.as_ref().is_some_and(Option::is_some))
        .cloned()
        .collect();
    Some(load_rules(MIN_PROTOCOL_VERSION, 0, projected))
}

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
