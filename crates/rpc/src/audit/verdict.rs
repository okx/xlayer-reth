//! Pure per-transaction verdict classification for `xlayer_auditTransactions`.
//!
//! Classifies one already-executed deposit transaction's logs against a rule set using the
//! same [`rcs_filter::matching::try_evaluate`] pure function the look-ahead engine's design is
//! built around. This module does no execution and no network IO — unlike
//! [`rcs_filter::handle::FilterHandle::screen_tx`], it never touches a `BufferPool`.

use std::collections::BTreeMap;

use alloy_primitives::{Address, Log, B256, U256};
use rcs_filter::client::ActionItem;
use rcs_filter::handle::ScreenInput;
use rcs_filter::matching::{try_evaluate, MatchOutcome};
use rcs_filter::rules::RuleSet;
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

/// One transaction's classification result, as returned to the RPC caller.
#[derive(Debug, Clone, Serialize)]
pub struct AuditResult {
    pub tx_hash: String,
    pub source_hash: String,
    pub verdict: Verdict,
    pub actions: Option<BTreeMap<String, Vec<ActionItem>>>,
}

impl AuditResult {
    /// Builds a `Malformed` result for a request that cannot be classified, either because
    /// `build_deposit_tx` failed or because matching exceeded its resource budget. The empty
    /// `tx_hash` consistently indicates that no authoritative screening verdict was produced.
    pub fn malformed(source_hash: String) -> Self {
        Self { tx_hash: String::new(), source_hash, verdict: Verdict::Malformed, actions: None }
    }

    /// Builds an `Unknown` result for a tx that was never executed because an earlier tx in the
    /// same batch stopped the loop with an `Audit` verdict (see `rpc.rs`'s `assemble_results`).
    pub fn unknown(source_hash: String) -> Self {
        Self { tx_hash: String::new(), source_hash, verdict: Verdict::Unknown, actions: None }
    }
}

/// Classifies one already-executed deposit transaction against `rules`. `tx_hash` and `logs`
/// come from the caller's EVM execution; this function does no execution itself —
/// zero network IO, zero side effects (unlike `FilterHandle::screen_tx`, this never touches a
/// `BufferPool`).
pub fn verdict_for(
    req: &DepositTxRequest,
    from: Address,
    to: Option<Address>,
    tx_hash: B256,
    value: U256,
    logs: &[Log],
    rules: &RuleSet,
) -> AuditResult {
    let input =
        ScreenInput { tx_hash, origin: from, tx_to: to, nonce: 0, value, block_height: 0, logs };
    match try_evaluate(rules, &input) {
        Err(error) => {
            tracing::warn!(
                target: "xlayer_audit_rpc",
                source_hash = %req.source_hash,
                %error,
                "matching budget exceeded; returning malformed result"
            );
            AuditResult::malformed(req.source_hash.clone())
        }
        Ok(MatchOutcome::Allow) => AuditResult {
            tx_hash: format!("{tx_hash:#x}"),
            source_hash: req.source_hash.clone(),
            verdict: Verdict::Allow,
            actions: None,
        },
        Ok(MatchOutcome::Deny) => AuditResult {
            tx_hash: format!("{tx_hash:#x}"),
            source_hash: req.source_hash.clone(),
            verdict: Verdict::Deny,
            actions: None,
        },
        Ok(MatchOutcome::Audit { actions, .. }) => AuditResult {
            tx_hash: format!("{tx_hash:#x}"),
            source_hash: req.source_hash.clone(),
            verdict: Verdict::Audit,
            actions: Some(actions),
        },
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
            origin(),
            Some(recipient()),
            B256::ZERO,
            U256::ZERO,
            &[],
            &no_rules(),
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
            origin(),
            Some(recipient()),
            B256::ZERO,
            one_token,
            &[log],
            &deny_above_threshold_rule(),
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
            origin(),
            Some(recipient()),
            B256::ZERO,
            below_threshold,
            &[log],
            &deny_above_threshold_rule(),
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
            origin(),
            Some(recipient()),
            B256::ZERO,
            value,
            &[log],
            &audit_quota_rule(),
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
            origin(),
            Some(recipient()),
            B256::ZERO,
            U256::ZERO,
            &logs,
            &audit_quota_rule(),
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
        let raw: RawRule = serde_json::from_str(
            r#"{"id":"excessive","event_abis":{"a":{"type":"event","name":"A","inputs":[],"anonymous":true},"b":{"type":"event","name":"B","inputs":[],"anonymous":true}},"audit_types":["custom"],"condition":true,"action":"audit"}"#,
        )
        .unwrap();
        let rules = load_rules(1, 0, vec![raw]);
        let logs = (0..65)
            .map(|_| Log { address: token(), data: LogData::new_unchecked(vec![], Bytes::new()) })
            .collect::<Vec<_>>();

        let result = verdict_for(
            &sample_request(),
            origin(),
            Some(recipient()),
            B256::ZERO,
            U256::ZERO,
            &logs,
            &rules,
        );

        assert_eq!(result.verdict, Verdict::Malformed);
        assert!(result.actions.is_none());
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
            origin(),
            Some(recipient()), // normal business target, not the TxBlacklist contract
            B256::ZERO,
            U256::ZERO,
            &[], // no logs
            &emergency_deny_all_rule(),
        );
        assert_eq!(result.verdict, Verdict::Deny);
        assert!(result.actions.is_none());
    }
}
