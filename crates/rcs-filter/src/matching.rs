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

/// Binds one alias's two flat condition variables.
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
    // Both values are present for a direct-delegatecall observation, and only such observations
    // are candidates, so a missing one is an invariant violation rather than a null binding.
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
mod tests {
    use super::*;
    use crate::rules::{load_rules, RawRule};
    use crate::test_support::{golden, log_builder};
    use alloy_primitives::{keccak256, Bytes, LogData, U256};
    use serde_json::json;

    fn ruleset_a() -> RuleSet {
        load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()])
    }

    fn audit_rule(
        id: &str,
        event_alias: &str,
        event_name: &str,
        inputs: Value,
        audit_type: &str,
    ) -> RawRule {
        let mut event_abis = serde_json::Map::new();
        event_abis.insert(
            event_alias.to_string(),
            json!({
                "type": "event",
                "name": event_name,
                "inputs": inputs,
                "anonymous": false
            }),
        );
        serde_json::from_value(json!({
            "id": id,
            "event_abis": event_abis,
            "audit_types": [audit_type],
            "condition": true,
            "action": "audit"
        }))
        .unwrap()
    }

    fn erc20_audit_rule(id: &str, event_alias: &str) -> RawRule {
        audit_rule(
            id,
            event_alias,
            "Transfer",
            json!([
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ]),
            "quota",
        )
    }

    fn empty_event_log(address: Address, event_name: &str) -> Log {
        let topic0 = keccak256(format!("{event_name}()").as_bytes());
        Log { address, data: LogData::new_unchecked(vec![topic0], Bytes::new()) }
    }

    fn reset_decode_attempts() {
        TEST_DECODE_ATTEMPTS.with(|attempts| attempts.set(0));
    }

    fn decode_attempts() -> usize {
        TEST_DECODE_ATTEMPTS.with(std::cell::Cell::get)
    }

    fn transfer_input<'a>(logs: &'a [Log]) -> ScreenInput<'a> {
        ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: U256::ZERO,
            block_height: 1_000_000,
            logs,
            observations: Observations::NOT_CAPTURED,
        }
    }

    /// Emergency "deny everything except the TxBlacklist contract" rule. Declares a carrier
    /// Transfer event (empty event_abis is rejected by compile_rule); the deny decision rides on
    /// the tx-level `contract_address` condition, so no-log / contract-creation txs are covered.
    const TX_BLACKLIST: &str = "0xb1ac000000000000000000000000000000000001";

    fn emergency_deny_all_rule() -> RuleSet {
        let raw: RawRule = serde_json::from_value(json!({
            "id": "emergency-deny-all",
            "event_abis": {
                "transfer": {
                    "type": "event", "name": "Transfer",
                    "inputs": [
                        {"name": "from", "type": "address", "indexed": true},
                        {"name": "to", "type": "address", "indexed": true},
                        {"name": "value", "type": "uint256", "indexed": false}
                    ],
                    "anonymous": false
                }
            },
            "condition": { "!=": [ {"var": "contract_address"}, TX_BLACKLIST ] },
            "action": "deny"
        }))
        .unwrap();
        load_rules(1, 1, vec![raw])
    }

    fn input_to(to: Option<Address>, logs: &[Log]) -> ScreenInput<'_> {
        ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: to,
            nonce: 1,
            value: U256::ZERO,
            block_height: 1_000_000,
            logs,
            observations: Observations::NOT_CAPTURED,
        }
    }

    #[test]
    fn no_log_tx_hits_contract_address_deny() {
        // AC#1: a native/no-log transaction to a normal business target is denied.
        let rules = emergency_deny_all_rule();
        assert_eq!(
            evaluate(&rules, &input_to(Some(golden::claim_contract()), &[])),
            MatchOutcome::Deny
        );
    }

    #[test]
    fn blacklist_target_is_not_denied() {
        // AC#3: a transaction whose `to` is the TxBlacklist contract must NOT be denied.
        let rules = emergency_deny_all_rule();
        let blacklist: Address = TX_BLACKLIST.parse().unwrap();
        assert_eq!(evaluate(&rules, &input_to(Some(blacklist), &[])), MatchOutcome::Allow);
    }

    #[test]
    fn contract_creation_is_denied() {
        // AC#4: contract creation (tx.to == null) binds contract_address to null → denied.
        let rules = emergency_deny_all_rule();
        assert_eq!(evaluate(&rules, &input_to(None, &[])), MatchOutcome::Deny);
    }

    #[test]
    fn no_log_tx_negative_condition_on_missing_event_field_denies() {
        // AC#2: a deny rule whose condition tests a MISSING event field under null semantics.
        let raw: RawRule = serde_json::from_value(json!({
            "id": "deny-when-transfer-to-null",
            "event_abis": {
                "transfer": {
                    "type": "event", "name": "Transfer",
                    "inputs": [
                        {"name": "from", "type": "address", "indexed": true},
                        {"name": "to", "type": "address", "indexed": true},
                        {"name": "value", "type": "uint256", "indexed": false}
                    ],
                    "anonymous": false
                }
            },
            "condition": { "==": [ {"var": "transfer.to"}, null ] },
            "action": "deny"
        }))
        .unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        assert_eq!(
            evaluate(&rules, &input_to(Some(golden::claim_contract()), &[])),
            MatchOutcome::Deny
        );
    }

    #[test]
    fn unrelated_log_is_treated_as_missing_event_and_still_denies() {
        // AC#5: a log unrelated to the rule's event decodes to nothing → missing event →
        // the deny-all condition still fires on a no-matching-event transaction.
        let rules = emergency_deny_all_rule();
        let unrelated = empty_event_log(golden::token_x(), "SomethingElse");
        assert_eq!(
            evaluate(&rules, &input_to(Some(golden::claim_contract()), &[unrelated])),
            MatchOutcome::Deny
        );
    }

    #[test]
    fn all_events_missing_audit_emits_no_action_and_allows() {
        // AC#7: an audit rule whose condition holds ONLY under an all-null binding must not
        // produce an Audit (no empty action, no submit) — it stays Allow.
        let mut rule = audit_rule("audit-null", "event", "Event", json!([]), "custom");
        rule.condition = json!(true);
        let rules = load_rules(1, 1, vec![rule]);
        assert_eq!(
            evaluate(&rules, &input_to(Some(golden::claim_contract()), &[])),
            MatchOutcome::Allow
        );
    }

    #[test]
    fn deny_beats_audit_under_all_null_bindings() {
        // AC#6: with both an audit rule (no-op under all-null) and a deny rule (fires under
        // all-null), the transaction is denied. Priority deny > audit > allow is preserved.
        let mut audit = audit_rule("audit-null", "event", "Event", json!([]), "custom");
        audit.condition = json!(true);
        let deny: RawRule = serde_json::from_value(json!({
            "id": "deny-null",
            "event_abis": {
                "transfer": {
                    "type": "event", "name": "Transfer",
                    "inputs": [
                        {"name": "from", "type": "address", "indexed": true},
                        {"name": "to", "type": "address", "indexed": true},
                        {"name": "value", "type": "uint256", "indexed": false}
                    ],
                    "anonymous": false
                }
            },
            "condition": true,
            "action": "deny"
        }))
        .unwrap();
        let rules = load_rules(1, 1, vec![audit, deny]);
        assert_eq!(
            evaluate(&rules, &input_to(Some(golden::claim_contract()), &[])),
            MatchOutcome::Deny
        );
    }

    #[test]
    fn later_matching_log_denies_and_discards_collected_audit_actions() {
        let audit = erc20_audit_rule("audit-first", "transfer");
        let mut deny = erc20_audit_rule("deny-later", "transfer");
        deny.action = Action::Deny;
        deny.condition = json!({
            "==": [
                {"var": "transfer.from"},
                golden::BLACKLISTED_FROM
            ]
        });
        let rules = load_rules(1, 1, vec![audit, deny]);
        let logs = [
            log_builder::erc20_transfer(
                golden::token_x(),
                golden::bridge_erc20(),
                golden::recipient(),
                golden::one_token(),
            ),
            log_builder::erc20_transfer(
                golden::token_x(),
                golden::blacklisted_from(),
                golden::recipient(),
                golden::one_token(),
            ),
        ];

        assert_eq!(evaluate(&rules, &transfer_input(&logs)), MatchOutcome::Deny);
    }

    #[test]
    fn later_matching_log_is_included_in_audit_actions() {
        let mut audit = erc20_audit_rule("audit-later", "transfer");
        audit.condition = json!({
            "==": [
                {"var": "transfer.from"},
                golden::BLACKLISTED_FROM
            ]
        });
        let rules = load_rules(1, 1, vec![audit]);
        let logs = [
            log_builder::erc20_transfer(
                golden::token_x(),
                golden::bridge_erc20(),
                golden::recipient(),
                U256::from(1),
            ),
            log_builder::erc20_transfer(
                golden::token_x(),
                golden::blacklisted_from(),
                golden::recipient(),
                U256::from(2),
            ),
        ];

        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &transfer_input(&logs)) else {
            panic!("later matching physical log must produce Audit")
        };
        assert_eq!(actions["quota"].len(), 1);
        assert_eq!(actions["quota"][0].params["from"], json!(golden::BLACKLISTED_FROM));
        assert_eq!(actions["quota"][0].params["value"], json!("2"));
    }

    #[test]
    fn identical_physical_logs_at_distinct_indexes_are_all_preserved() {
        let rules = load_rules(1, 1, vec![erc20_audit_rule("audit-both", "transfer")]);
        let log = log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        );
        let logs = [log.clone(), log];

        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &transfer_input(&logs)) else {
            panic!("both physical logs must produce Audit")
        };
        assert_eq!(actions["quota"].len(), 2);
        assert_eq!(actions["quota"][0], actions["quota"][1]);
    }

    #[test]
    fn within_budget_non_first_complete_multi_event_binding_is_evaluated() {
        let raw: RawRule = serde_json::from_value(json!({
            "id": "paired-events",
            "event_abis": {
                "event_a": {
                    "type": "event",
                    "name": "EventA",
                    "inputs": [],
                    "anonymous": false
                },
                "event_b": {
                    "type": "event",
                    "name": "EventB",
                    "inputs": [],
                    "anonymous": false
                }
            },
            "audit_types": ["custom"],
            "condition": {
                "and": [
                    {"==": [{"var": "event_a.address"}, golden::CLAIM_CONTRACT]},
                    {"==": [{"var": "event_b.address"}, golden::RECIPIENT]}
                ]
            },
            "action": "audit"
        }))
        .unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        let logs = [
            empty_event_log(golden::token_x(), "EventA"),
            empty_event_log(golden::claim_contract(), "EventB"),
            empty_event_log(golden::claim_contract(), "EventA"),
            empty_event_log(golden::recipient(), "EventB"),
        ];

        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &transfer_input(&logs)) else {
            panic!("non-first complete binding must produce Audit")
        };
        let custom = &actions["custom"];
        assert_eq!(custom.len(), 2);
        assert_eq!(custom[0].name, "event_a");
        assert_eq!(custom[0].address, golden::CLAIM_CONTRACT);
        assert_eq!(custom[1].name, "event_b");
        assert_eq!(custom[1].address, golden::RECIPIENT);
    }

    #[test]
    fn literal_true_rule_without_any_physical_log_denies_but_never_audits() {
        // Deny-All: a literal-true deny rule now denies a no-log transaction; a
        // literal-true audit rule stays Allow because an all-null binding emits no audit content.
        let mut deny = audit_rule("no-log", "event", "Event", json!([]), "custom");
        deny.action = Action::Deny;
        let deny_rules = load_rules(1, 1, vec![deny]);
        assert_eq!(evaluate(&deny_rules, &transfer_input(&[])), MatchOutcome::Deny);

        let audit = audit_rule("no-log", "event", "Event", json!([]), "custom");
        let audit_rules = load_rules(1, 1, vec![audit]);
        assert_eq!(evaluate(&audit_rules, &transfer_input(&[])), MatchOutcome::Allow);
    }

    #[test]
    fn complete_binding_count_rejects_usize_multiplication_overflow() {
        assert_eq!(checked_binding_count([usize::MAX, 2]), None);
    }

    #[test]
    fn excessive_complete_bindings_are_rejected_and_fail_closed() {
        let raw: RawRule = serde_json::from_value(json!({
            "id": "excessive-bindings",
            "event_abis": {
                "a": {"type": "event", "name": "A", "inputs": [], "anonymous": true},
                "b": {"type": "event", "name": "B", "inputs": [], "anonymous": true}
            },
            "audit_types": ["custom"],
            "condition": false,
            "action": "audit"
        }))
        .unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        // 65 × 65 = 4,225 complete bindings, just over the intended 4,096 budget.
        let logs = (0..65)
            .map(|_| Log {
                address: golden::token_x(),
                data: LogData::new_unchecked(vec![], Bytes::new()),
            })
            .collect::<Vec<_>>();
        let input = transfer_input(&logs);

        // An explicit budget of 4,096 rejects the 4,225 bindings.
        assert!(matches!(
            try_evaluate(&rules, &input, 4_096),
            Err(MatchError::BindingBudgetExceeded { .. })
        ));
        // The fail-closed wrapper uses the DEFAULT budget (10,000); a scenario exceeding it
        // (101 × 101 = 10,201) still fails closed to Deny.
        let over_default = (0..101)
            .map(|_| Log {
                address: golden::token_x(),
                data: LogData::new_unchecked(vec![], Bytes::new()),
            })
            .collect::<Vec<_>>();
        assert_eq!(evaluate(&rules, &transfer_input(&over_default)), MatchOutcome::Deny);
    }

    #[test]
    fn complete_binding_budget_is_accumulated_across_candidate_rules() {
        let rule = |id: &str| -> RawRule {
            serde_json::from_value(json!({
                "id": id,
                "event_abis": {
                    "a": {"type": "event", "name": "A", "inputs": [], "anonymous": true},
                    "b": {"type": "event", "name": "B", "inputs": [], "anonymous": true}
                },
                "audit_types": ["custom"],
                "condition": false,
                "action": "audit"
            }))
            .unwrap()
        };
        let rules = load_rules(1, 1, vec![rule("first"), rule("second")]);
        // Each rule has 46 × 46 = 2,116 bindings (individually below budget), but the
        // evaluation total is 4,232 and must be rejected before recursing into rule two.
        let logs = (0..46)
            .map(|_| Log {
                address: golden::token_x(),
                data: LogData::new_unchecked(vec![], Bytes::new()),
            })
            .collect::<Vec<_>>();

        assert!(matches!(
            try_evaluate(&rules, &transfer_input(&logs), 4_096),
            Err(MatchError::BindingBudgetExceeded { .. })
        ));
    }

    // Builds a single audit rule with two anonymous zero-topic events, so K matching logs yield
    // K × K complete bindings — a precise knob for exercising the configurable binding budget.
    fn two_event_rule() -> RuleSet {
        let raw: RawRule = serde_json::from_value(json!({
            "id": "binding-budget",
            "event_abis": {
                "a": {"type": "event", "name": "A", "inputs": [], "anonymous": true},
                "b": {"type": "event", "name": "B", "inputs": [], "anonymous": true}
            },
            "audit_types": ["custom"],
            "condition": false,
            "action": "audit"
        }))
        .unwrap();
        load_rules(1, 1, vec![raw])
    }

    fn zero_topic_logs(count: usize) -> Vec<Log> {
        (0..count)
            .map(|_| Log {
                address: golden::token_x(),
                data: LogData::new_unchecked(vec![], Bytes::new()),
            })
            .collect()
    }

    #[test]
    fn binding_budget_boundary_100_121() {
        let rules = two_event_rule();
        // 10 × 10 = 100 == limit → allowed (budget check is strictly greater-than).
        assert!(evaluate_checked(&rules, &transfer_input(&zero_topic_logs(10)), 100).is_ok());
        // 11 × 11 = 121 > 100 → rejected.
        assert!(matches!(
            evaluate_checked(&rules, &transfer_input(&zero_topic_logs(11)), 100),
            Err(MatchError::BindingBudgetExceeded { .. })
        ));
    }

    #[test]
    fn low_binding_budget_2_3_boundary() {
        // A single-event rule yields exactly K bindings for K matching logs.
        let raw: RawRule = serde_json::from_value(json!({
            "id": "binding-budget-low",
            "event_abis": {"a": {"type": "event", "name": "A", "inputs": [], "anonymous": true}},
            "audit_types": ["custom"],
            "condition": false,
            "action": "audit"
        }))
        .unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        assert!(evaluate_checked(&rules, &transfer_input(&zero_topic_logs(2)), 2).is_ok());
        assert!(matches!(
            evaluate_checked(&rules, &transfer_input(&zero_topic_logs(3)), 2),
            Err(MatchError::BindingBudgetExceeded { .. })
        ));
    }

    #[test]
    fn filter_skip_contract_mismatch_decodes_zero_logs() {
        let mut raw: RawRule = serde_json::from_str(golden::RULE_SCENARIO_A).unwrap();
        raw.contract_address = Some(golden::CLAIM_CONTRACT.to_string());
        let rules = load_rules(1, 1, vec![raw]);
        let logs = [log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        let mut input = transfer_input(&logs);
        input.tx_to = Some(golden::token_x());
        reset_decode_attempts();
        assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
        assert_eq!(decode_attempts(), 0);
    }

    #[test]
    fn filter_skip_origin_mismatch_decodes_zero_logs() {
        let mut raw: RawRule = serde_json::from_str(golden::RULE_SCENARIO_A).unwrap();
        raw.origin = Some(golden::ORIGIN.to_string());
        let rules = load_rules(1, 1, vec![raw]);
        let logs = [log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        let mut input = transfer_input(&logs);
        input.origin = golden::recipient();
        reset_decode_attempts();
        assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
        assert_eq!(decode_attempts(), 0);
    }

    #[test]
    fn missing_named_event_binds_null() {
        let raw = r#"{
          "id":"null-companion",
          "event_abis":{
            "transfer":{"type":"event","name":"Transfer","inputs":[
              {"name":"from","type":"address","indexed":true},
              {"name":"to","type":"address","indexed":true},
              {"name":"value","type":"uint256","indexed":false}],"anonymous":false},
            "companion":{"type":"event","name":"Companion","inputs":[],"anonymous":false}
          },
          "audit_types":["custom"],
          "condition":{"==":[{"var":"companion.address"},null]},
          "action":"audit"
        }"#;
        let rules = load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()]);
        let logs = [log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        let input = transfer_input(&logs);
        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
            panic!("missing companion must bind null and satisfy the condition")
        };
        assert_eq!(actions["custom"].len(), 1);
        assert_eq!(actions["custom"][0].name, "transfer");
    }

    #[test]
    fn same_log_uses_strictest_action_and_audit_union() {
        let mut allow = erc20_audit_rule("allow", "transfer");
        allow.action = Action::Allow;
        let quota = erc20_audit_rule("quota", "transfer");
        let custom = audit_rule(
            "custom",
            "transfer",
            "Transfer",
            json!([
                {"name": "from", "type": "address", "indexed": true},
                {"name": "to", "type": "address", "indexed": true},
                {"name": "value", "type": "uint256", "indexed": false}
            ]),
            "custom",
        );
        let logs = [log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        let input = transfer_input(&logs);
        let rules = load_rules(1, 1, vec![allow, quota, custom]);
        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
            panic!("audit must dominate allow")
        };
        assert_eq!(actions.keys().map(String::as_str).collect::<Vec<_>>(), ["custom", "quota"]);

        let mut deny = erc20_audit_rule("deny", "transfer");
        deny.action = Action::Deny;
        let rules = load_rules(1, 1, vec![erc20_audit_rule("audit", "transfer"), deny]);
        assert_eq!(evaluate(&rules, &input), MatchOutcome::Deny);
    }

    #[test]
    fn deny_stops_remaining_candidate_scan() {
        let mut deny = erc20_audit_rule("deny-first", "transfer");
        deny.action = Action::Deny;
        let rules = load_rules(1, 1, vec![deny, erc20_audit_rule("audit-later", "transfer")]);
        let logs = [log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        reset_decode_attempts();
        assert_eq!(evaluate(&rules, &transfer_input(&logs)), MatchOutcome::Deny);
        assert_eq!(decode_attempts(), 1);
    }

    #[test]
    fn scenario_a_matches_audit_with_quota_item() {
        let rules = ruleset_a();
        let logs = vec![log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };
        match evaluate(&rules, &input) {
            MatchOutcome::Audit { actions, .. } => {
                let quota = actions.get("quota").expect("quota key");
                assert_eq!(quota.len(), 1);
                assert_eq!(quota[0].name, "transfer");
                assert_eq!(quota[0].address, golden::TOKEN_X);
                assert_eq!(quota[0].params.get("value").unwrap(), golden::ONE_TOKEN);
                assert_eq!(quota[0].params.get("from").unwrap(), golden::BRIDGE_ERC20);
            }
            other => panic!("expected Audit, got {other:?}"),
        }
    }

    #[test]
    fn transfer_batch_decodes_uint_arrays_as_json_string_arrays() {
        let rules = load_rules(
            1,
            1,
            vec![audit_rule(
                "batch-quota",
                "transferBatch",
                "TransferBatch",
                json!([
                    {"name": "operator", "type": "address", "indexed": true},
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to", "type": "address", "indexed": true},
                    {"name": "ids", "type": "uint256[]", "indexed": false},
                    {"name": "values", "type": "uint256[]", "indexed": false}
                ]),
                "quota",
            )],
        );
        assert_eq!(rules.rules.len(), 1);

        let logs = vec![log_builder::erc1155_transfer_batch(
            golden::token_x(),
            golden::origin(),
            golden::bridge_erc20(),
            golden::recipient(),
            vec![U256::from(7), U256::from(8)],
            vec![U256::from(100), U256::from(200)],
        )];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };

        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
            panic!("expected audit")
        };
        let item = &actions["quota"][0];
        assert_eq!(item.name, "transferBatch");
        assert_eq!(item.params["operator"], json!(golden::ORIGIN));
        assert_eq!(item.params["from"], json!(golden::BRIDGE_ERC20));
        assert_eq!(item.params["to"], json!(golden::RECIPIENT));
        assert_eq!(item.params["ids"], json!(["7", "8"]));
        assert_eq!(item.params["values"], json!(["100", "200"]));

        let wire = serde_json::to_value(&actions).unwrap();
        assert_eq!(wire["quota"][0]["params"]["ids"], json!(["7", "8"]));
        assert_eq!(wire["quota"][0]["params"]["values"], json!(["100", "200"]));
    }

    #[test]
    fn distinct_aliases_on_same_log_are_deduplicated_by_audit_type_and_log_index() {
        let rules = load_rules(
            1,
            1,
            vec![
                erc20_audit_rule("quota-a", "first_transfer"),
                erc20_audit_rule("quota-b", "second_transfer"),
            ],
        );
        let logs = vec![log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };

        match evaluate(&rules, &input) {
            MatchOutcome::Audit { actions, .. } => {
                let quota = actions.get("quota").expect("quota key");
                assert_eq!(quota.len(), 1);
                assert_eq!(quota[0].name, "first_transfer");
                assert_eq!(quota[0].params.get("value").unwrap(), golden::ONE_TOKEN);
            }
            other => panic!("expected Audit, got {other:?}"),
        }
    }

    #[test]
    fn same_physical_log_is_kept_independently_for_distinct_audit_types() {
        let rules = load_rules(
            1,
            1,
            vec![
                erc20_audit_rule("quota", "quota_transfer"),
                audit_rule(
                    "custom",
                    "custom_transfer",
                    "Transfer",
                    json!([
                        {"name": "from", "type": "address", "indexed": true},
                        {"name": "to", "type": "address", "indexed": true},
                        {"name": "value", "type": "uint256", "indexed": false}
                    ]),
                    "custom",
                ),
            ],
        );
        let logs = [log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];

        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &transfer_input(&logs)) else {
            panic!("both audit types must be preserved")
        };
        assert_eq!(actions["quota"].len(), 1);
        assert_eq!(actions["quota"][0].name, "quota_transfer");
        assert_eq!(actions["custom"].len(), 1);
        assert_eq!(actions["custom"][0].name, "custom_transfer");
    }

    #[test]
    fn identical_cross_rule_items_are_deduplicated() {
        let rules = load_rules(
            1,
            1,
            vec![erc20_audit_rule("quota-a", "transfer"), erc20_audit_rule("quota-b", "transfer")],
        );
        let logs = vec![log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        )];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };
        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
            panic!("expected audit")
        };
        assert_eq!(actions["quota"].len(), 1);
    }

    #[test]
    fn matching_signature_with_malformed_data_does_not_match() {
        let rules = ruleset_a();
        let topic0 = rules.rules[0].events[0].topic0;
        let logs = vec![Log {
            address: golden::token_x(),
            data: LogData::new_unchecked(vec![topic0], Bytes::new()),
        }];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };
        assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
    }

    #[test]
    fn malformed_log_is_missing_event_denies_for_deny_allows_for_audit() {
        // A signature-matching but undecodable log is a missing event (null binding). Under
        // Deny-All that still denies for a deny rule; an audit rule stays Allow (no empty submit).
        let inputs = json!([
            {"name": "from", "type": "address", "indexed": true},
            {"name": "to", "type": "address", "indexed": true},
            {"name": "value", "type": "uint256", "indexed": false}
        ]);
        let expected = [(Action::Deny, MatchOutcome::Deny), (Action::Audit, MatchOutcome::Allow)];
        for (action, want) in expected {
            let mut raw = erc20_audit_rule("malformed", "transfer");
            raw.action = action;
            raw.event_abis.get_mut("transfer").unwrap().inputs =
                serde_json::from_value(inputs.clone()).unwrap();
            let rules = load_rules(1, 1, vec![raw]);
            let topic0 = rules.rules[0].events[0].topic0;
            let logs = vec![Log {
                address: golden::token_x(),
                data: LogData::new_unchecked(vec![topic0], Bytes::new()),
            }];
            let input = ScreenInput {
                tx_hash: golden::tx_a(),
                origin: golden::origin(),
                tx_to: Some(golden::claim_contract()),
                nonce: 1,
                value: alloy_primitives::U256::ZERO,
                block_height: 1_000_000,
                logs: &logs,
                observations: Observations::NOT_CAPTURED,
            };
            assert_eq!(evaluate(&rules, &input), want);
        }
    }

    #[test]
    fn anonymous_indexed_event_matches_by_successful_decode() {
        let raw = r#"{"id":"anon","event_abis":{"transfer":{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":true}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
        let rules = load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()]);
        let normal = log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        );
        let logs = vec![Log {
            address: normal.address,
            data: LogData::new_unchecked(
                normal.data.topics()[1..].to_vec(),
                normal.data.data.clone(),
            ),
        }];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };
        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
            panic!("expected audit")
        };
        assert_eq!(actions["quota"][0].name, "transfer");
        assert_eq!(actions["quota"][0].params["value"], golden::ONE_TOKEN);
    }

    #[test]
    fn anonymous_event_rejects_wrong_topic_count_and_malformed_data() {
        let raw = r#"{"id":"anon","event_abis":{"transfer":{"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":true}},"audit_types":["quota"],"condition":true,"action":"audit"}"#;
        let rules = load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()]);
        let normal = log_builder::erc20_transfer(
            golden::token_x(),
            golden::bridge_erc20(),
            golden::recipient(),
            golden::one_token(),
        );
        let indexed = normal.data.topics()[1..].to_vec();
        let cases = [
            Log {
                address: normal.address,
                data: LogData::new_unchecked(indexed[..1].to_vec(), normal.data.data.clone()),
            },
            Log {
                address: normal.address,
                data: LogData::new_unchecked(
                    [indexed.clone(), vec![keccak256("extra")]].concat(),
                    normal.data.data.clone(),
                ),
            },
            Log { address: normal.address, data: LogData::new_unchecked(indexed, Bytes::new()) },
        ];

        for log in cases {
            let logs = [log];
            let input = ScreenInput {
                tx_hash: golden::tx_a(),
                origin: golden::origin(),
                tx_to: Some(golden::claim_contract()),
                nonce: 1,
                value: alloy_primitives::U256::ZERO,
                block_height: 1_000_000,
                logs: &logs,
                observations: Observations::NOT_CAPTURED,
            };
            assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
        }
    }

    #[test]
    fn anonymous_non_indexed_event_preserves_all_successful_logs() {
        let raw = r#"{"id":"anon","event_abis":{"message":{"type":"event","name":"Message","inputs":[],"anonymous":true}},"audit_types":["custom"],"condition":true,"action":"audit"}"#;
        let rules = load_rules(1, 1, vec![serde_json::from_str(raw).unwrap()]);
        let logs = vec![
            Log { address: golden::token_x(), data: LogData::new_unchecked(vec![], Bytes::new()) },
            Log {
                address: golden::claim_contract(),
                data: LogData::new_unchecked(vec![], Bytes::new()),
            },
        ];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };
        let MatchOutcome::Audit { actions, .. } = evaluate(&rules, &input) else {
            panic!("expected audit")
        };
        assert_eq!(actions["custom"].len(), 2);
        assert_eq!(actions["custom"][0].address, golden::TOKEN_X);
        assert_eq!(actions["custom"][1].address, golden::CLAIM_CONTRACT);
    }

    #[test]
    fn distinct_logs_in_the_same_audit_type_are_preserved() {
        let rules = load_rules(
            1,
            1,
            vec![
                audit_rule("custom-a", "event_a", "EventA", json!([]), "custom"),
                audit_rule("custom-b", "event_b", "EventB", json!([]), "custom"),
            ],
        );
        let logs = vec![
            empty_event_log(golden::token_x(), "EventA"),
            empty_event_log(golden::claim_contract(), "EventB"),
        ];
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };

        match evaluate(&rules, &input) {
            MatchOutcome::Audit { actions, .. } => {
                let custom = actions.get("custom").expect("custom key");
                assert_eq!(custom.len(), 2);
                assert_eq!(custom[0].name, "event_a");
                assert_eq!(custom[1].name, "event_b");
            }
            other => panic!("expected Audit, got {other:?}"),
        }
    }

    #[test]
    fn scenario_b_deny_short_circuits() {
        let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_B).unwrap()]);
        let logs = vec![log_builder::erc20_transfer(
            golden::token_x(),
            golden::blacklisted_from(),
            golden::recipient(),
            golden::one_token(),
        )];
        let input = ScreenInput {
            tx_hash: golden::tx_b(),
            origin: golden::origin_b(),
            tx_to: Some(golden::claim_contract()),
            nonce: 0,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &logs,
            observations: Observations::NOT_CAPTURED,
        };
        assert_eq!(evaluate(&rules, &input), MatchOutcome::Deny);
    }

    #[test]
    fn no_matching_log_allows() {
        let rules = ruleset_a();
        let input = ScreenInput {
            tx_hash: golden::tx_a(),
            origin: golden::origin(),
            tx_to: Some(golden::claim_contract()),
            nonce: 1,
            value: alloy_primitives::U256::ZERO,
            block_height: 1_000_000,
            logs: &[],
            observations: Observations::NOT_CAPTURED,
        };
        assert_eq!(evaluate(&rules, &input), MatchOutcome::Allow);
    }
}
