//! Per-transaction screening algorithm: filter-skip (zero-decode) →
//! topic0 candidate lookup → event decode → JSONLogic eval → action merge
//! (`deny > audit > allow`, both intra-log and cross-log).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use alloy_dyn_abi::{DynSolEvent, DynSolType, DynSolValue};
use alloy_primitives::{Address, Log, B256};
use serde_json::Value;
use thiserror::Error;

use crate::callstack::{
    CallObservation, CallObservationKind, ObservationUnavailable, Observations,
};
use crate::client::ActionItem;
use crate::handle::ScreenInput;
use crate::rules::{
    eval_compiled, Action, CompiledDirectDelegatecall, CompiledEvent, CompiledRule, RuleSet,
    TimeoutAction, DEFAULT_MAX_EVENT_BINDINGS_PER_TX,
};

type ActionsByLog = BTreeMap<String, BTreeMap<usize, ActionItem>>;

#[cfg(test)]
std::thread_local! {
    static TEST_DECODE_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Outcome of matching one transaction against a rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchOutcome {
    /// No rule requires action → package normally.
    Allow,
    /// At least one `deny` rule matched → drop the whole tx immediately.
    Deny,
    /// One or more `audit` rules matched (no deny) → submit to RCS.
    Audit {
        /// `{ audit_type: [ActionItem] }` grouped for the submit payload.
        actions: BTreeMap<String, Vec<ActionItem>>,
        /// Merged timeout fallback across matched audit rules (deny is stricter).
        timeout_action: TimeoutAction,
    },
}

/// A failure detected before visiting complete bindings: either a resource budget, or an
/// observation set that cannot be adjudicated. Every variant fails closed at the caller — none of
/// them may degrade to allow.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MatchError {
    #[error("rule '{rule_id}' complete binding count overflowed")]
    BindingCountOverflow { rule_id: String },
    #[error(
        "rule '{rule_id}' would raise the complete binding count to {attempted}, exceeding the per-transaction budget of {limit}"
    )]
    BindingBudgetExceeded { rule_id: String, attempted: usize, limit: usize },
    /// A rule declares direct-delegatecall aliases but the execution path produced no adjudicable
    /// observation set. Distinct from "observed nothing", which suppresses the rule.
    #[error("rule '{rule_id}' declares callstack selectors but observations are unavailable ({reason})", reason = reason.as_str())]
    ObservationsUnavailable { rule_id: String, reason: ObservationUnavailable },
    /// The observation set was captured under a different rule snapshot than the one evaluating
    /// it, so an unobserved frame is indistinguishable from an absent one.
    #[error("rule '{rule_id}' observation set was captured under a different rule snapshot")]
    ObservationSnapshotMismatch { rule_id: String },
}

impl MatchError {
    /// Operational classification, so the four cases can be logged and counted separately rather
    /// than sharing one line: a rule-driven denial, a budget overflow, an unavailable or
    /// incomplete observation set, and a snapshot-stamp mismatch are different incidents.
    pub fn kind(&self) -> MatchErrorKind {
        match self {
            MatchError::BindingCountOverflow { .. } | MatchError::BindingBudgetExceeded { .. } => {
                MatchErrorKind::BindingBudget
            }
            MatchError::ObservationsUnavailable { .. } => MatchErrorKind::ObservationsUnavailable,
            MatchError::ObservationSnapshotMismatch { .. } => {
                MatchErrorKind::ObservationSnapshotMismatch
            }
        }
    }
}

/// Fixed, low-cardinality classification of a [`MatchError`], suitable as a metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchErrorKind {
    BindingBudget,
    ObservationsUnavailable,
    ObservationSnapshotMismatch,
}

/// Fallible screening entry used by callers that can surface malformed/error outcomes.
///
/// `max_event_bindings` is the authoritative per-transaction complete-binding budget. Callers with
/// a [`crate::FilterConfig`] pass its configured value; callers without one pass
/// [`DEFAULT_MAX_EVENT_BINDINGS_PER_TX`].
pub fn try_evaluate(
    rules: &RuleSet,
    input: &ScreenInput,
    max_event_bindings: usize,
) -> Result<MatchOutcome, MatchError> {
    evaluate_checked(rules, input, max_event_bindings)
}

fn checked_binding_count(candidate_counts: impl IntoIterator<Item = usize>) -> Option<usize> {
    candidate_counts.into_iter().try_fold(1usize, usize::checked_mul)
}

/// Runs the full screening algorithm for one transaction. Resource-budget errors fail closed;
/// callers with a malformed/error channel should use [`try_evaluate`] to preserve the reason.
pub fn evaluate(rules: &RuleSet, input: &ScreenInput) -> MatchOutcome {
    match try_evaluate(rules, input, DEFAULT_MAX_EVENT_BINDINGS_PER_TX) {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::warn!(target: "rcs_filter", %error, "matching budget exceeded; failing closed");
            MatchOutcome::Deny
        }
    }
}

fn evaluate_checked(
    rules: &RuleSet,
    input: &ScreenInput,
    max_event_bindings: usize,
) -> Result<MatchOutcome, MatchError> {
    // Merge audit rules at the physical-log level. Multiple rules can match the same log, but
    // RCS must receive that event only once per audit type or it would account the same action
    // multiple times.
    let mut actions_by_log = ActionsByLog::new();
    let mut has_audit = false;
    let mut timeout_action = TimeoutAction::Allow;
    let mut complete_binding_count = 0usize;

    // Emergency Deny-All: evaluate every rule for every transaction. The legacy
    // topic0 candidate index is intentionally bypassed here (see `candidate_rule_indices`).
    for idx in 0..rules.rules.len() {
        let rule = &rules.rules[idx];

        // Stage one: filter-skip with zero decoding.
        if let Some(ca) = rule.contract_address
            && input.tx_to != Some(ca)
        {
            continue;
        }
        if let Some(o) = rule.origin
            && input.origin != o
        {
            continue;
        }

        // Stage two: decode every physical match. Missing events contribute one null candidate,
        // then complete bindings are visited lazily instead of materializing their Cartesian
        // product.
        let candidates: Vec<Vec<Option<MatchedEvent>>> = rule
            .events
            .iter()
            .map(|event| {
                let matched = find_logs_for_event(event, input.logs);
                if matched.is_empty() {
                    vec![None]
                } else {
                    matched.into_iter().map(Some).collect()
                }
            })
            .collect();
        // Callstack candidates, resolved against the observation set. A rule without aliases
        // takes no part in this and keeps the pre-existing path byte for byte, including when the
        // observation set is unavailable.
        //
        // Suppression is decided HERE, before the binding budget is charged, so a suppressed rule
        // contributes zero to it rather than one.
        let frame_candidates = match callstack_candidates(rule, &input.observations, rules)? {
            CallstackCandidates::Suppressed => continue,
            CallstackCandidates::Candidates(candidates) => candidates,
        };
        // The other half of suppression, for a rule declaring BOTH families: every declared event
        // must also have a physical binding, or the whole rule is not evaluated. A null-bound
        // missing event is safe for an event-only rule — that is deployed behaviour, preserved
        // below — but on a rule that also declares an alias it reopens exactly the hazard
        // suppression exists to close: the negated membership tests these rules carry read a
        // one-sided null as "not equal", so the rule objects to a transaction that merely never
        // emitted the event. Decided here, after the unavailable / foreign-stamp cases have
        // already failed closed above, and before the budget is charged, so a suppressed rule
        // contributes zero to it rather than one.
        if !rule.callstack.is_empty() && has_missing_event(&candidates) {
            continue;
        }
        // The budget covers the full event x callstack product, not each family separately:
        // bounding them independently would still let a handful of selectors and a handful of
        // events enumerate a large combination count.
        let rule_binding_count = checked_binding_count(
            candidates.iter().map(Vec::len).chain(frame_candidates.iter().map(Vec::len)),
        )
        .ok_or_else(|| MatchError::BindingCountOverflow { rule_id: rule.id.clone() })?;
        complete_binding_count = complete_binding_count
            .checked_add(rule_binding_count)
            .ok_or_else(|| MatchError::BindingCountOverflow { rule_id: rule.id.clone() })?;
        if complete_binding_count > max_event_bindings {
            return Err(MatchError::BindingBudgetExceeded {
                rule_id: rule.id.clone(),
                attempted: complete_binding_count,
                limit: max_event_bindings,
            });
        }
        let mut current = Vec::with_capacity(candidates.len());
        let mut frame_current: Vec<&CallObservation> = Vec::with_capacity(frame_candidates.len());
        let mut visit = |binding: &[Option<MatchedEvent>]| {
            // Missing events are null-bound; evaluate the condition for every binding, including
            // no-log transactions. Whether any *physical* event backed this binding decides only
            // whether an Audit may carry content (an all-null binding must never submit empties).
            // Callstack observations never make a binding physical: the action payload is
            // event-oriented, and a callstack-only audit rule is rejected at load.
            let has_physical = binding.iter().any(Option::is_some);
            visit_frame_bindings(0, &frame_candidates, &mut frame_current, &mut |frames| {
                let bindings = build_bindings(rule, input, binding, frames);
                if !eval_compiled(&rule.compiled_condition, &bindings) {
                    return ControlFlow::Continue(());
                }

                // Stage three: action merge with deny short-circuit.
                match rule.action {
                    Action::Deny => ControlFlow::Break(MatchOutcome::Deny),
                    Action::Allow => ControlFlow::Continue(()),
                    Action::Audit => {
                        // Never emit an empty-action Audit: an all-null binding that satisfies an
                        // audit condition submits no content to RCS. Treat it as a no-op.
                        if !has_physical {
                            return ControlFlow::Continue(());
                        }
                        has_audit = true;
                        timeout_action = timeout_action.stricter(rule.audit_timeout_action);
                        for audit_type in &rule.audit_types {
                            let by_log = actions_by_log.entry(audit_type.clone()).or_default();
                            for (event, matched) in rule.events.iter().zip(binding) {
                                let Some(matched) = matched else { continue };
                                by_log.entry(matched.log_index).or_insert_with(|| ActionItem {
                                    name: event.var_name.clone(),
                                    address: matched.address.clone(),
                                    params: matched.params.clone(),
                                });
                            }
                        }
                        ControlFlow::Continue(())
                    }
                }
            })
        };
        if let ControlFlow::Break(outcome) =
            visit_bindings(0, &candidates, &mut current, &mut visit)
        {
            return Ok(outcome);
        }
    }

    if has_audit {
        let actions = actions_by_log
            .into_iter()
            .map(|(audit_type, by_log)| (audit_type, by_log.into_values().collect()))
            .collect();
        Ok(MatchOutcome::Audit { actions, timeout_action })
    } else {
        Ok(MatchOutcome::Allow)
    }
}

/// Whether any declared event of this rule produced no physical match.
///
/// Each dimension of `candidates` is either the single null candidate standing for a missing event
/// or one entry per matching log, so "no physical match" is exactly an all-`None` dimension. Empty
/// for a rule declaring no event, which therefore never has a missing one.
fn has_missing_event(candidates: &[Vec<Option<MatchedEvent>>]) -> bool {
    candidates.iter().any(|event| event.iter().all(Option::is_none))
}

/// A matched named event and the physical transaction log that produced it.
#[derive(Clone)]
struct MatchedEvent {
    log_index: usize,
    address: String,
    params: BTreeMap<String, Value>,
}

/// Deduplicated candidate rule indices across all logs (topic0 index lookup), sorted for
/// deterministic deny short-circuit order.
///
/// Retained but no longer consulted: `evaluate_checked` now evaluates every rule for every
/// transaction (Emergency Deny-All). Kept as a ready-made fast-path for a future
/// performance task — see Decision D1 in
/// `docs/superpowers/plans/2026-09-02-emergency-deny-all-matcher-plan.md`.
#[allow(dead_code)]
fn candidate_rule_indices(rules: &RuleSet, logs: &[Log]) -> Vec<usize> {
    let mut set = BTreeSet::new();
    for log in logs {
        if let Some(topic0) = log.data.topics().first() {
            for &idx in rules.candidates_for_topic0(topic0) {
                set.insert(idx);
            }
        }
        for &idx in rules.candidates_for_anonymous(log.data.topics().len()) {
            set.insert(idx);
        }
    }
    set.into_iter().collect()
}

/// Builds JSONLogic bindings for one complete physical-log combination. Tx-level vars
/// (`contract_address`, `origin`, `value`, `nonce`) plus `<name>.<param>` / `<name>.address`
/// per declared event; unmatched events bind all their variables to `null`.
fn build_bindings(
    rule: &CompiledRule,
    input: &ScreenInput,
    binding: &[Option<MatchedEvent>],
    frames: &[&CallObservation],
) -> crate::rules::Bindings {
    let mut b = crate::rules::Bindings::new();
    b.insert(
        "contract_address".to_string(),
        input.tx_to.map(addr_lower).map(Value::String).unwrap_or(Value::Null),
    );
    b.insert("origin".to_string(), Value::String(addr_lower(input.origin)));
    // `value` = tx.value as decimal string; retained for backward compatibility with deployed rules.
    b.insert("value".to_string(), Value::String(input.value.to_string()));
    b.insert("nonce".to_string(), Value::Number(input.nonce.into()));

    for (event, matched) in rule.events.iter().zip(binding) {
        match matched {
            Some(matched) => {
                for (pname, pval) in &matched.params {
                    b.insert(format!("{}.{}", event.var_name, pname), pval.clone());
                }
                b.insert(
                    format!("{}.address", event.var_name),
                    Value::String(matched.address.clone()),
                );
            }
            None => {
                for input_def in &event.inputs {
                    b.insert(format!("{}.{}", event.var_name, input_def.name), Value::Null);
                }
                b.insert(format!("{}.address", event.var_name), Value::Null);
            }
        }
    }

    for (alias, observed) in rule.callstack.iter().zip(frames) {
        bind_direct_delegatecall(&mut b, alias, observed);
    }

    b
}

/// Binds one alias's four flat condition variables: the selected proxy's own address and executed
/// code hash (`<alias>.proxyAddress`, `<alias>.proxyCodeHash`) and the delegate target's address
/// and executed code hash (`<alias>.delegateTo`, `<alias>.delegateCodeHash`).
///
/// There is no `Option` and no null branch: a rule whose alias produced no observation is
/// suppressed before any binding is built, so this is only ever reached with a real observation.
/// That is what removes the null-binding hazard at its source instead of asking every rule author
/// to guard against it — a guard an author can forget is not a guard.
fn bind_direct_delegatecall(
    b: &mut crate::rules::Bindings,
    alias: &CompiledDirectDelegatecall,
    observation: &CallObservation,
) {
    let name = &alias.alias;
    // The proxy side is always present: candidate selection already required
    // `observation.address == alias.address`, and `code_hash` is the hash of the code the proxy's
    // own frame executed.
    b.insert(format!("{name}.proxyAddress"), Value::String(addr_lower(observation.address)));
    b.insert(format!("{name}.proxyCodeHash"), Value::String(hash_lower(observation.code_hash)));
    // Both target values are present for a direct-delegatecall observation, and only such
    // observations are candidates, so a missing one is an invariant violation rather than a null
    // binding.
    b.insert(
        format!("{name}.delegateTo"),
        Value::String(addr_lower(
            observation.impl_address.expect("a direct delegatecall carries its target address"),
        )),
    );
    b.insert(
        format!("{name}.delegateCodeHash"),
        Value::String(hash_lower(
            observation.impl_code_hash.expect("a direct delegatecall carries its target code hash"),
        )),
    );
}

/// The three outcomes of resolving one rule's aliases against the observation set.
enum CallstackCandidates<'obs> {
    /// At least one declared alias produced no observation, so the whole rule is not evaluated.
    Suppressed,
    /// One candidate dimension per declared alias, each non-empty, in execution order. Empty for
    /// a rule declaring no alias.
    Candidates(Vec<Vec<&'obs CallObservation>>),
}

/// Resolves each declared alias of `rule` to its candidate observations.
///
/// Returns [`CallstackCandidates::Suppressed`] when any declared alias produced no observation.
/// Suppression — not a null candidate — is the required semantics: an unobserved alias binds no
/// variable at all, so `condition` never runs and the rule contributes no verdict. Binding the
/// absent value as null instead would make the negated membership test these rules are written
/// with evaluate true for every transaction that never touched the proxy, turning an
/// implementation allowlist into a production-wide objection.
///
/// Returns an error for a rule that declares an alias while the observation set cannot be
/// adjudicated: unavailable or incomplete capture, or a set captured under a different snapshot.
/// Both fail closed, and that is deliberately NOT the same as suppression — "capture did not run"
/// must never read as "capture ran and this proxy was untouched". Rules without aliases are
/// untouched by either condition.
fn callstack_candidates<'obs>(
    rule: &CompiledRule,
    observations: &Observations<'obs>,
    rules: &RuleSet,
) -> Result<CallstackCandidates<'obs>, MatchError> {
    if rule.callstack.is_empty() {
        return Ok(CallstackCandidates::Candidates(Vec::new()));
    }
    let items = match observations {
        Observations::Unavailable { reason } => {
            return Err(MatchError::ObservationsUnavailable {
                rule_id: rule.id.clone(),
                reason: *reason,
            });
        }
        Observations::Available { stamp, items } => {
            if *stamp != rules.stamp() {
                return Err(MatchError::ObservationSnapshotMismatch { rule_id: rule.id.clone() });
            }
            *items
        }
    };
    let mut candidates = Vec::with_capacity(rule.callstack.len());
    for alias in &rule.callstack {
        let matched: Vec<&CallObservation> =
            items.iter().filter(|observation| alias_accepts(alias, observation)).collect();
        if matched.is_empty() {
            return Ok(CallstackCandidates::Suppressed);
        }
        candidates.push(matched);
    }
    Ok(CallstackCandidates::Candidates(candidates))
}

/// Whether an observation is a candidate for an alias.
///
/// Only a direct delegatecall of the alias's own address qualifies. `Call`-kind observations are
/// still produced — they are what lets capture attribute a delegatecall at all — but no condition
/// variable under this contract can read one, so they are never candidates. Because the target
/// address and target code hash are present exactly for a direct delegatecall, both bound
/// variables are always real strings.
fn alias_accepts(alias: &CompiledDirectDelegatecall, observation: &CallObservation) -> bool {
    observation.kind == CallObservationKind::DirectDelegatecall
        && observation.address == alias.address
}

/// Visits every callstack-candidate combination in candidate order without allocating the product,
/// mirroring [`visit_bindings`]. A visitor can break immediately to return a pure deny verdict.
fn visit_frame_bindings<'obs, F>(
    alias_idx: usize,
    candidates: &[Vec<&'obs CallObservation>],
    current: &mut Vec<&'obs CallObservation>,
    visit: &mut F,
) -> ControlFlow<MatchOutcome>
where
    F: FnMut(&[&'obs CallObservation]) -> ControlFlow<MatchOutcome>,
{
    if alias_idx == candidates.len() {
        return visit(current);
    }

    for candidate in &candidates[alias_idx] {
        current.push(candidate);
        if let ControlFlow::Break(outcome) =
            visit_frame_bindings(alias_idx + 1, candidates, current, visit)
        {
            current.pop();
            return ControlFlow::Break(outcome);
        }
        current.pop();
    }
    ControlFlow::Continue(())
}

/// Visits every complete event binding in candidate order without allocating the Cartesian
/// product. A visitor can break immediately, which is used to return a pure deny verdict.
fn visit_bindings<F>(
    event_idx: usize,
    candidates: &[Vec<Option<MatchedEvent>>],
    current: &mut Vec<Option<MatchedEvent>>,
    visit: &mut F,
) -> ControlFlow<MatchOutcome>
where
    F: FnMut(&[Option<MatchedEvent>]) -> ControlFlow<MatchOutcome>,
{
    if event_idx == candidates.len() {
        return visit(current);
    }

    for candidate in &candidates[event_idx] {
        current.push(candidate.clone());
        if let ControlFlow::Break(outcome) =
            visit_bindings(event_idx + 1, candidates, current, visit)
        {
            current.pop();
            return ControlFlow::Break(outcome);
        }
        current.pop();
    }
    ControlFlow::Continue(())
}

/// Returns whether a physical log has the topic shape required before attempting ABI decode.
fn event_matches_shape(event: &CompiledEvent, log: &Log) -> bool {
    if event.anonymous {
        log.data.topics().len() == event.inputs.iter().filter(|input| input.indexed).count()
    } else {
        log.data.topics().first() == Some(&event.topic0)
    }
}

/// Finds every successfully decoded physical log for an event in EVM log order.
fn find_logs_for_event(event: &CompiledEvent, logs: &[Log]) -> Vec<MatchedEvent> {
    logs.iter()
        .enumerate()
        .filter_map(|(log_index, log)| {
            if !event_matches_shape(event, log) {
                return None;
            }
            #[cfg(test)]
            TEST_DECODE_ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
            decode_event(event, log).map(|params| MatchedEvent {
                log_index,
                address: addr_lower(log.address),
                params,
            })
        })
        .collect()
}

/// ABI-decodes a matched log into named JSON values following the event's declared input order
/// (indexed params from topics, the rest from data).
fn decode_event(event: &CompiledEvent, log: &Log) -> Option<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();

    let indexed_types: Vec<DynSolType> =
        event.inputs.iter().filter(|i| i.indexed).map(|i| i.sol_type.clone()).collect();
    let body_types: Vec<DynSolType> =
        event.inputs.iter().filter(|i| !i.indexed).map(|i| i.sol_type.clone()).collect();

    let topic0 = if event.anonymous { None } else { Some(event.topic0) };
    let decoder = DynSolEvent::new_unchecked(topic0, indexed_types, DynSolType::Tuple(body_types));

    let decoded =
        decoder.decode_log_parts(log.data.topics().iter().copied(), &log.data.data).ok()?;

    // Reassemble in declared input order.
    let mut idx_indexed = 0usize;
    let mut idx_body = 0usize;
    for input_def in &event.inputs {
        let value = if input_def.indexed {
            let v = decoded.indexed.get(idx_indexed);
            idx_indexed += 1;
            v
        } else {
            let v = decoded.body.get(idx_body);
            idx_body += 1;
            v
        };
        if let Some(v) = value {
            out.insert(input_def.name.clone(), dyn_value_to_json(v));
        }
    }
    Some(out)
}

/// Converts decoded ABI values to stable JSON. Scalar values retain the existing string wire
/// representation, while ABI arrays remain arrays so `TransferBatch` does not leak Rust debug
/// formatting into the RCS request.
fn dyn_value_to_json(v: &DynSolValue) -> Value {
    match v {
        DynSolValue::Address(a) => Value::String(addr_lower(*a)),
        DynSolValue::Uint(u, _) => Value::String(u.to_string()),
        DynSolValue::Int(i, _) => Value::String(i.to_string()),
        DynSolValue::Bool(b) => Value::String(b.to_string()),
        DynSolValue::String(s) => Value::String(s.clone()),
        DynSolValue::FixedBytes(w, _) => Value::String(format!("{w:#x}")),
        DynSolValue::Bytes(b) => Value::String(format!("0x{}", alloy_primitives::hex::encode(b))),
        DynSolValue::Array(values)
        | DynSolValue::FixedArray(values)
        | DynSolValue::Tuple(values) => {
            Value::Array(values.iter().map(dyn_value_to_json).collect())
        }
        other => Value::String(format!("{other:?}")),
    }
}

/// Lower-cased `0x`-prefixed hex form of an address (no EIP-55 checksum).
fn addr_lower(a: Address) -> String {
    format!("{a:#x}")
}

/// Lower-cased `0x`-prefixed hex form of a 32-byte hash, matching the selector encoding rules so
/// an author's configured value and the observed value compare as equal strings.
fn hash_lower(h: B256) -> String {
    format!("{h:#x}")
}

#[cfg(test)]
mod tests;
