//! Pure per-transaction verdict classification for `xlayer_auditTransactions`.
//!
//! Classifies one already-executed deposit transaction's logs against a rule set using the
//! same [`rcs_filter::matching::try_evaluate`] pure function the look-ahead engine's design is
//! built around. This module does no execution and no network IO — unlike
//! [`rcs_filter::handle::FilterHandle::screen_tx`], it never touches a `BufferPool`.

use std::collections::BTreeMap;

use alloy_primitives::{Address, Log, B256, U256};
use rcs_filter::callstack::Observations;
use rcs_filter::client::ActionItem;
use rcs_filter::handle::ScreenInput;
use rcs_filter::matching::{try_evaluate, MatchOutcome};
use rcs_filter::rules::{RuleSet, DEFAULT_MAX_EVENT_BINDINGS_PER_TX};
use serde::Serialize;

use super::deposit::DepositTxRequest;

/// The per-tx classification outcome (wire-serialized as lowercase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Allow,
    Deny,
    Audit,
    /// Never executed/classified because an earlier transaction in the same batch resolved to
    /// `Audit` and the handler stopped executing the rest.
    Unknown,
    Malformed,
}

/// How call-frame identification affected one transaction's verdict — a **diagnostic**, never
/// the protection mechanism.
///
/// The adjudication itself lands in [`Verdict`], which is the only field the real caller acts on.
/// This field explains *why* a verdict came out as it did; no correctness property depends on a
/// consumer reading it. It is emitted only for a v2 request, so a v1 response stays byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallstackIdentification {
    /// Identified, and no selector-bearing rule objected.
    Clean,
    /// Identified, and a selector-bearing rule objected (would deny).
    Objected,
    /// The executed code could not be identified: capture failure, budget overflow or a
    /// snapshot-stamp mismatch. Never reported as `Clean`.
    NotIdentified,
    /// The request declares no callstack selector, so there was nothing to identify.
    NotApplicable,
}

/// One transaction's classification result, as returned to the RPC caller.
#[derive(Debug, Clone, Serialize)]
pub struct AuditResult {
    pub tx_hash: String,
    pub source_hash: String,
    pub verdict: Verdict,
    pub actions: Option<BTreeMap<String, Vec<ActionItem>>>,
    /// Diagnostic only; see [`CallstackIdentification`]. Absent from the serialized response
    /// unless the request advertised protocol version 2, so `tx_hash` / `source_hash` /
    /// `verdict` / `actions` keep their exact previous wire shape for every existing caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callstack_identification: Option<CallstackIdentification>,
}

impl AuditResult {
    /// Builds a `Malformed` result for a request that cannot be classified, either because
    /// `build_deposit_tx` failed or because matching exceeded its resource budget. The empty
    /// `tx_hash` consistently indicates that no authoritative screening verdict was produced.
    pub fn malformed(source_hash: String) -> Self {
        Self {
            tx_hash: String::new(),
            source_hash,
            verdict: Verdict::Malformed,
            actions: None,
            callstack_identification: None,
        }
    }

    /// Attaches the identification diagnostic, for a v2 request only.
    pub fn with_identification(mut self, identification: Option<CallstackIdentification>) -> Self {
        self.callstack_identification = identification;
        self
    }

    /// Builds an `Unknown` result for a tx that was never executed because an earlier tx in the
    /// same batch stopped the loop with an `Audit` verdict (see `rpc.rs`'s `assemble_results`).
    pub fn unknown(source_hash: String) -> Self {
        Self {
            tx_hash: String::new(),
            source_hash,
            verdict: Verdict::Unknown,
            actions: None,
            callstack_identification: None,
        }
    }
}

/// The observation state of an audit-RPC execution that ran **no** capture.
///
/// No longer the normal path: the audit RPC now attaches the shared capture inspector whenever
/// the request's own rules declare call-frame selectors, so `verdict_for` is fed a real
/// observation set. This value remains reachable for a genuine capture failure and for a request
/// that declares no selectors at all (where nothing can depend on it), and it continues to yield
/// the existing non-verdict rather than an authoritative `Allow`.
pub fn observations_not_captured() -> Observations<'static> {
    Observations::NOT_CAPTURED
}

/// Classifies one already-executed deposit transaction against `rules`. `tx_hash` and `logs`
/// come from the caller's EVM execution; this function does no execution itself —
/// zero network IO, zero side effects (unlike `FilterHandle::screen_tx`, this never touches a
/// `BufferPool`).
/// Identity of one already-executed transaction, as the caller's EVM reported it.
///
/// Grouped rather than passed field by field: the classifier needs the whole identity, and a
/// struct keeps the call sites from drifting out of positional order as the set grows.
#[derive(Debug, Clone, Copy)]
pub struct ExecutedTx {
    pub from: Address,
    pub to: Option<Address>,
    pub tx_hash: B256,
    pub value: U256,
}

pub fn verdict_for(
    req: &DepositTxRequest,
    tx: ExecutedTx,
    logs: &[Log],
    rules: &RuleSet,
    observations: Observations<'_>,
) -> AuditResult {
    let ExecutedTx { from, to, tx_hash, value } = tx;
    // `observations` comes from the caller's real capture. An `Unavailable` value here means the
    // capture genuinely failed (or the request declares no selectors); reporting an empty list
    // instead would make every selector bind `matched = false`, and a rule written to reject the
    // transaction would return `Allow` — "not captured" disguised as "captured, nothing matched".
    let identification = identification_for(rules, observations);
    let input = ScreenInput {
        tx_hash,
        origin: from,
        tx_to: to,
        nonce: 0,
        value,
        block_height: 0,
        logs,
        observations,
    };
    // The audit RPC path holds no FilterConfig, so it uses the default binding budget. This keeps
    // its existing matcher-overflow `Malformed` semantics unchanged.
    match try_evaluate(rules, &input, DEFAULT_MAX_EVENT_BINDINGS_PER_TX) {
        Err(error) => {
            // Every screening failure returns the existing non-verdict channel rather than an
            // authoritative verdict. A matcher budget overflow and an unadjudicable observation
            // set are different incidents, so the reason is reported, but neither may become
            // `Allow`: that would release a transaction a rule was written to reject.
            tracing::warn!(
                target: "xlayer_audit_rpc",
                source_hash = %req.source_hash,
                kind = ?error.kind(),
                %error,
                "screening failed closed; returning malformed result"
            );
            AuditResult::malformed(req.source_hash.clone())
                .with_identification(Some(CallstackIdentification::NotIdentified))
        }
        Ok(MatchOutcome::Allow) => AuditResult {
            tx_hash: format!("{tx_hash:#x}"),
            source_hash: req.source_hash.clone(),
            verdict: Verdict::Allow,
            actions: None,
            callstack_identification: Some(identification),
        },
        Ok(MatchOutcome::Deny) => AuditResult {
            tx_hash: format!("{tx_hash:#x}"),
            source_hash: req.source_hash.clone(),
            verdict: Verdict::Deny,
            actions: None,
            callstack_identification: Some(identification),
        },
        Ok(MatchOutcome::Audit { actions, .. }) => AuditResult {
            tx_hash: format!("{tx_hash:#x}"),
            source_hash: req.source_hash.clone(),
            verdict: Verdict::Audit,
            actions: Some(actions),
            callstack_identification: Some(identification),
        },
    }
}

/// The diagnostic for one adjudication, decided BEFORE evaluation so it describes the
/// observation set rather than the rule outcome's shape.
///
/// `Objected` is attached afterwards by the caller when a selector-bearing request denies, since
/// `MatchOutcome::Deny` carries no rule id and therefore cannot say which dimension objected.
fn identification_for(rules: &RuleSet, observations: Observations<'_>) -> CallstackIdentification {
    if !rules.has_callstack_rules() {
        return CallstackIdentification::NotApplicable;
    }
    match observations {
        Observations::Available { .. } => CallstackIdentification::Clean,
        Observations::Unavailable { .. } => CallstackIdentification::NotIdentified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Bytes, LogData};
    use rcs_filter::rules::{load_rules, RawRule};
    use rcs_filter::test_support::log_builder::erc20_transfer;

    fn no_rules() -> RuleSet {
        load_rules(1, 0, vec![])
    }

    fn sample_request() -> DepositTxRequest {
        super::super::deposit::tests_support_sample_request()
    }

    fn origin() -> Address {
        "0x0404040404040404040404040404040404040404".parse().unwrap()
    }

    fn recipient() -> Address {
        "0x0303030303030303030303030303030303030303".parse().unwrap()
    }

    fn token() -> Address {
        "0x0202020202020202020202020202020202020202".parse().unwrap()
    }

    #[test]
    fn empty_ruleset_always_allows() {
        let req = sample_request();
        let result = verdict_for(
            &req,
            ExecutedTx {
                from: origin(),
                to: Some(recipient()),
                tx_hash: B256::ZERO,
                value: U256::ZERO,
            },
            &[],
            &no_rules(),
            observations_not_captured(),
        );
        assert_eq!(result.verdict, Verdict::Allow);
        assert!(result.actions.is_none());
    }

    /// Deny rule keyed on `{"var":"value"}` (in addition to the triggering ERC20 Transfer
    /// event) — deny only fires above a threshold. This exercises the regression where `value`
    /// was hardcoded to `U256::ZERO`; this transaction (whose real
    /// value is one token, above the half-token threshold) would have wrongly evaluated to
    /// `Allow` instead of `Deny`.
    fn deny_above_threshold_rule() -> RuleSet {
        let json = format!(
            r#"{{
              "id": "deny-large-value-transfer",
              "event_abis": {{
                "transfer": {{
                  "type": "event", "name": "Transfer",
                  "inputs": [
                    {{ "name": "from", "type": "address", "indexed": true }},
                    {{ "name": "to", "type": "address", "indexed": true }},
                    {{ "name": "value", "type": "uint256", "indexed": false }}
                  ],
                  "anonymous": false
                }}
              }},
              "condition": {{
                "and": [
                  {{ "==": [{{ "var": "transfer.address" }}, "{token:#x}"] }},
                  {{ ">": [{{ "var": "value" }}, "500000000000000000"] }}
                ]
              }},
              "action": "deny"
            }}"#,
            token = token()
        );
        let raw: RawRule = serde_json::from_str(&json).expect("valid rule json");
        load_rules(1, 0, vec![raw])
    }

    #[test]
    fn deny_rule_matching_real_value_denies() {
        let req = sample_request();
        let one_token = U256::from(1_000_000_000_000_000_000u128);
        let log = erc20_transfer(token(), origin(), recipient(), one_token);

        let result = verdict_for(
            &req,
            ExecutedTx {
                from: origin(),
                to: Some(recipient()),
                tx_hash: B256::ZERO,
                value: one_token,
            },
            &[log],
            &deny_above_threshold_rule(),
            observations_not_captured(),
        );

        assert_eq!(result.verdict, Verdict::Deny);
        assert!(result.actions.is_none());
    }

    #[test]
    fn deny_rule_below_threshold_value_allows() {
        // Same rule, but with the real value below the deny threshold — regression guard
        // showing the deny rule is genuinely value-sensitive (as opposed to always firing).
        let req = sample_request();
        let below_threshold = U256::from(100_000_000_000_000_000u128); // 0.1 token
        let log = erc20_transfer(token(), origin(), recipient(), below_threshold);

        let result = verdict_for(
            &req,
            ExecutedTx {
                from: origin(),
                to: Some(recipient()),
                tx_hash: B256::ZERO,
                value: below_threshold,
            },
            &[log],
            &deny_above_threshold_rule(),
            observations_not_captured(),
        );

        assert_eq!(result.verdict, Verdict::Allow);
    }

    /// Audit rule (quota) matching the golden ERC20-bridge scenario shape, adapted to this
    /// module's fixtures. Exercises the `Audit` arm, asserting `actions` is populated.
    fn audit_quota_rule() -> RuleSet {
        let json = format!(
            r#"{{
              "id": "audit-erc20-tokenx",
              "event_abis": {{
                "transfer": {{
                  "type": "event", "name": "Transfer",
                  "inputs": [
                    {{ "name": "from", "type": "address", "indexed": true }},
                    {{ "name": "to", "type": "address", "indexed": true }},
                    {{ "name": "value", "type": "uint256", "indexed": false }}
                  ],
                  "anonymous": false
                }}
              }},
              "audit_types": ["quota"],
              "condition": {{ "==": [{{ "var": "transfer.address" }}, "{token:#x}"] }},
              "action": "audit",
              "audit_timeout_action": "allow"
            }}"#,
            token = token()
        );
        let raw: RawRule = serde_json::from_str(&json).expect("valid rule json");
        load_rules(1, 0, vec![raw])
    }

    #[test]
    fn audit_rule_with_actions_populates_actions() {
        let req = sample_request();
        let value = U256::from(2_000_000_000_000_000_000u128);
        let log = erc20_transfer(token(), origin(), recipient(), value);

        let result = verdict_for(
            &req,
            ExecutedTx { from: origin(), to: Some(recipient()), tx_hash: B256::ZERO, value },
            &[log],
            &audit_quota_rule(),
            observations_not_captured(),
        );

        assert_eq!(result.verdict, Verdict::Audit);
        let actions = result.actions.expect("audit verdict must carry actions");
        let quota_items = actions.get("quota").expect("quota audit_type key present");
        assert_eq!(quota_items.len(), 1);
        let item = &quota_items[0];
        assert_eq!(item.name, "transfer");
        assert_eq!(item.address, format!("{:#x}", token()));
        assert_eq!(item.params.get("value").and_then(|v| v.as_str()), Some("2000000000000000000"));
    }

    #[test]
    fn audit_verdict_returns_every_physical_log_in_evm_order() {
        let req = sample_request();
        let first_value = U256::from(1);
        let second_value = U256::from(2);
        let logs = [
            erc20_transfer(token(), origin(), recipient(), first_value),
            erc20_transfer(token(), origin(), recipient(), second_value),
        ];

        let result = verdict_for(
            &req,
            ExecutedTx {
                from: origin(),
                to: Some(recipient()),
                tx_hash: B256::ZERO,
                value: U256::ZERO,
            },
            &logs,
            &audit_quota_rule(),
            observations_not_captured(),
        );

        assert_eq!(result.verdict, Verdict::Audit);
        let actions = result.actions.expect("audit verdict must carry actions");
        let values = actions["quota"]
            .iter()
            .map(|item| item.params["value"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(values, ["1", "2"]);
    }

    #[test]
    fn excessive_complete_bindings_are_reported_as_malformed() {
        // 2 anonymous zero-input events x 101 zero-topic logs = 101 x 101 = 10,201 complete
        // bindings, which genuinely exceeds the 10,000 default event-binding budget that
        // `verdict_for` uses. The budget check is strictly-greater, so a 65-log input (4,225
        // bindings) never trips it. Raise the input, never the asserted verdict.
        let raw: RawRule = serde_json::from_str(
            r#"{"id":"excessive","event_abis":{"a":{"type":"event","name":"A","inputs":[],"anonymous":true},"b":{"type":"event","name":"B","inputs":[],"anonymous":true}},"audit_types":["custom"],"condition":true,"action":"audit"}"#,
        )
        .unwrap();
        let rules = load_rules(1, 0, vec![raw]);
        let logs = (0..101)
            .map(|_| Log { address: token(), data: LogData::new_unchecked(vec![], Bytes::new()) })
            .collect::<Vec<_>>();

        let result = verdict_for(
            &sample_request(),
            ExecutedTx {
                from: origin(),
                to: Some(recipient()),
                tx_hash: B256::ZERO,
                value: U256::ZERO,
            },
            &logs,
            &rules,
            observations_not_captured(),
        );

        assert_eq!(result.verdict, Verdict::Malformed);
        assert!(result.actions.is_none());
    }

    #[test]
    fn exactly_budget_complete_bindings_are_classified_normally() {
        // 100 x 100 = 10,000 == the default budget. The comparison is strictly-greater, so
        // this is ALLOWED. Pins the inclusive edge: an off-by-one changing `>` to `>=` fails
        // here rather than passing silently.
        let raw: RawRule = serde_json::from_str(
            r#"{"id":"excessive","event_abis":{"a":{"type":"event","name":"A","inputs":[],"anonymous":true},"b":{"type":"event","name":"B","inputs":[],"anonymous":true}},"audit_types":["custom"],"condition":true,"action":"audit"}"#,
        )
        .unwrap();
        let rules = load_rules(1, 0, vec![raw]);
        let logs = (0..100)
            .map(|_| Log { address: token(), data: LogData::new_unchecked(vec![], Bytes::new()) })
            .collect::<Vec<_>>();

        let result = verdict_for(
            &sample_request(),
            ExecutedTx {
                from: origin(),
                to: Some(recipient()),
                tx_hash: B256::ZERO,
                value: U256::ZERO,
            },
            &logs,
            &rules,
            observations_not_captured(),
        );

        assert_eq!(result.verdict, Verdict::Audit);
        assert!(result.actions.is_some(), "exactly-at-budget must be classified, not Malformed");
        assert_ne!(result.verdict, Verdict::Malformed);
    }

    #[test]
    fn budget_crossed_only_by_accumulation_across_rules_is_reported_as_malformed() {
        // The complete-binding count accumulates ACROSS candidate rules and is compared once
        // against the budget. A per-rule comparison would pass every other test here, because
        // they all use a single rule. Two rules, each with two anonymous zero-input events over
        // n zero-topic logs, each contribute n^2; the total is 2 * n^2 while neither rule alone
        // is close to the budget.
        //   n = 70  ->  2 * 4,900  =  9,800 <= 10,000 -> allowed    (per-rule: 4,900 each)
        //   n = 71  ->  2 * 5,041  = 10,082  > 10,000 -> Malformed  (per-rule: 5,041 each)
        // The n = 71 half can only fail by accumulating across both rules.
        let two_rules = || -> RuleSet {
            let rule_json = r#"{"id":"ID","event_abis":{"a":{"type":"event","name":"A","inputs":[],"anonymous":true},"b":{"type":"event","name":"B","inputs":[],"anonymous":true}},"audit_types":["custom"],"condition":true,"action":"audit"}"#;
            let raw_one: RawRule =
                serde_json::from_str(&rule_json.replace("ID", "excessive-one")).unwrap();
            let raw_two: RawRule =
                serde_json::from_str(&rule_json.replace("ID", "excessive-two")).unwrap();
            load_rules(1, 0, vec![raw_one, raw_two])
        };

        let logs_of = |n: usize| -> Vec<Log> {
            (0..n)
                .map(|_| Log {
                    address: token(),
                    data: LogData::new_unchecked(vec![], Bytes::new()),
                })
                .collect()
        };

        let allowed = verdict_for(
            &sample_request(),
            ExecutedTx {
                from: origin(),
                to: Some(recipient()),
                tx_hash: B256::ZERO,
                value: U256::ZERO,
            },
            &logs_of(70),
            &two_rules(),
            observations_not_captured(),
        );
        assert_eq!(allowed.verdict, Verdict::Audit);
        assert!(
            allowed.actions.is_some(),
            "9,800 total bindings must be classified, not Malformed"
        );

        let malformed = verdict_for(
            &sample_request(),
            ExecutedTx {
                from: origin(),
                to: Some(recipient()),
                tx_hash: B256::ZERO,
                value: U256::ZERO,
            },
            &logs_of(71),
            &two_rules(),
            observations_not_captured(),
        );
        assert_eq!(malformed.verdict, Verdict::Malformed);
        assert!(
            malformed.actions.is_none(),
            "10,082 total bindings (5,041 per rule) must only exceed the budget by accumulating \
             across both candidate rules"
        );
    }

    #[test]
    fn unknown_result_has_empty_tx_hash_and_no_actions() {
        let result = AuditResult::unknown("0xsrc".to_string());
        assert_eq!(result.verdict, Verdict::Unknown);
        assert_eq!(result.source_hash, "0xsrc");
        assert_eq!(result.tx_hash, "");
        assert!(result.actions.is_none());
    }

    /// Emergency "deny everything except TxBlacklist" rule, expressed against this module's
    /// fixtures. Declares a carrier Transfer event (empty event_abis is rejected); the deny
    /// decision rides on the tx-level `contract_address` condition.
    fn emergency_deny_all_rule() -> RuleSet {
        let json = r#"{
          "id": "emergency-deny-all",
          "event_abis": {
            "transfer": {
              "type": "event", "name": "Transfer",
              "inputs": [
                { "name": "from", "type": "address", "indexed": true },
                { "name": "to", "type": "address", "indexed": true },
                { "name": "value", "type": "uint256", "indexed": false }
              ],
              "anonymous": false
            }
          },
          "condition": { "!=": [ { "var": "contract_address" }, "0xb1ac000000000000000000000000000000000001" ] },
          "action": "deny"
        }"#;
        let raw: RawRule = serde_json::from_str(json).expect("valid rule json");
        load_rules(1, 0, vec![raw])
    }

    #[test]
    fn emergency_rule_denies_no_log_normal_target_via_shared_path() {
        // AC#8 / G2: xlayer_auditTransactions shares try_evaluate, so a no-log tx to a normal
        // business target now returns Deny (it would have returned Allow before the Emergency Deny-All change).
        let req = sample_request();
        let result = verdict_for(
            &req,
            ExecutedTx {
                from: origin(),
                // normal business target, not the TxBlacklist contract
                to: Some(recipient()),
                tx_hash: B256::ZERO,
                value: U256::ZERO,
            },
            &[], // no logs
            &emergency_deny_all_rule(),
            observations_not_captured(),
        );
        assert_eq!(result.verdict, Verdict::Deny);
        assert!(result.actions.is_none());
    }
}
