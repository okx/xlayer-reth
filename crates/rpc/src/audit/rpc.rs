//! `xlayer_auditTransactions` JSON-RPC method: builds deposit txs from the request and executes
//! them **one at a time, in order**, against chain-tip state. `Allow` commits full state. `Deny`
//! discards the simulated state and continues. `Audit` discards the simulated state and stops,
//! leaving the tail `Unknown`. Native blacklist interception occurs inside EVM execution before
//! matcher classification; RCS synthetic-ban replay is responsible for making denied force
//! deposits reach that path.
//! A tx whose `source_hash` is in the request's `known_allowed` set skips this classification
//! entirely — it's executed and its state committed unconditionally as `Allow` (see
//! `AuditTransactionsRequest::known_allowed`'s doc comment for why this bypass is necessary).
//!
//! This handler does no side-effecting screening (never touches `FilterHandle::screen_tx` or its
//! `BufferPool`): it runs a throwaway EVM overlay seeded from the current chain-tip state and
//! discards it after producing verdicts.
//!
//! State anchoring and task placement follow the same `Call` helper (`spawn_with_state_at_block`)
//! that `eth_call`/`eth_estimateGas` use, so this handler's EVM execution runs on reth's blocking
//! task pool rather than the async runtime thread.

use std::sync::Arc;

use alloy_consensus::transaction::Recovered;
use alloy_consensus::Transaction;
use alloy_eips::BlockId;
use alloy_evm::{Evm, IntoTxEnv};
use alloy_op_evm::OpEvmFactory;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use rcs_filter::rules::load_rules_from_raw;
use reth_evm::{block::BlockExecutorFactory, ConfigureEvm};
use reth_optimism_primitives::OpTransactionSigned;
use reth_rpc_eth_api::{
    helpers::{Call, LoadState},
    FromEvmError, RpcNodeCore,
};
use reth_storage_api::BlockReaderIdExt;
use revm::{context_interface::result::ResultAndState, DatabaseCommit};

use xlayer_rcs_capture::{DirectDelegatecallConfig, RcsCaptureControl, RcsInspector};

use super::deposit::{build_deposit_tx, DepositTxRequest};
use super::verdict::AuditResult;
use super::verdict::Verdict;

/// Decides whether to commit one classified transaction's full simulated state and whether
/// ordered execution should continue. A blacklist FailedDeposit is produced only by the EVM's
/// native blacklist interceptor before classification, never synthesized here.
fn commit_and_continue(verdict: Verdict) -> (bool, bool) {
    match verdict {
        Verdict::Allow => (true, true),
        Verdict::Deny => (false, true),
        Verdict::Audit => (false, false),
        Verdict::Malformed => (false, true),
        Verdict::Unknown => unreachable!("verdict_for never produces {verdict:?}"),
    }
}

/// Executes and classifies the built deposits **one at a time, in request order** — the whole of
/// this handler's per-deposit contract in one place: one capture window per deposit, the shared
/// frame code, `verdict_for`, and the ordered commit that makes an earlier deposit's state effects
/// visible to a later one.
///
/// Generic over the EVM rather than inlined into the handler so that the deposit-path tests drive
/// **this** code with a real deposit transaction instead of restating the sequence; a test that
/// reproduces the loop can agree with itself while the handler diverges. The only thing a caller
/// varies is how it built the EVM (including the capture mode), which is the public capture API.
///
/// Returns one slot per built deposit, `None` for every slot after an `Audit` stopped the pass.
/// An execution error aborts capture (parked result included) and propagates, leaving nothing
/// behind.
pub(super) fn classify_ordered_deposits<E>(
    evm: &mut E,
    ordered_txs: &[Recovered<OpTransactionSigned>],
    ordered_tx_reqs: &[DepositTxRequest],
    rules: &rcs_filter::rules::RuleSet,
    capture_config: &Option<DirectDelegatecallConfig>,
    known_allowed: &std::collections::HashSet<String>,
) -> Result<Vec<Option<AuditResult>>, E::Error>
where
    E: Evm,
    E::DB: DatabaseCommit,
    E::Inspector: RcsCaptureControl,
    for<'tx> &'tx Recovered<OpTransactionSigned>: IntoTxEnv<E::Tx>,
{
    let mut executed: Vec<Option<AuditResult>> = (0..ordered_txs.len()).map(|_| None).collect();
    for (exec_idx, tx) in ordered_txs.iter().enumerate() {
        let tx_req = &ordered_tx_reqs[exec_idx];
        // One window per deposit, each beginning with a full reset, so a deposit's observations
        // can never be attributed to another. Ordered state effects are untouched: a prior
        // deposit's committed upgrade is still visible here, so a later deposit resolves the new,
        // real delegate target and its real code hash.
        {
            let inspector = evm.components_mut().1;
            inspector.configure_direct_delegatecalls(capture_config.clone());
            inspector.start_capture();
        }
        let transacted = evm.transact(tx);
        let ResultAndState { result, state } = match transacted {
            Ok(res) => res,
            Err(err) => {
                // An execution error leaves nothing behind, parked result included.
                evm.components_mut().1.abort_capture();
                return Err(err);
            }
        };
        let observations = {
            let inspector = evm.components_mut().1;
            inspector.finish_capture(result.logs());
            inspector.take_direct_delegatecalls()
        };

        if known_allowed.contains(&tx_req.source_hash) {
            evm.db_mut().commit(state);
            record_known_allowed_diagnostic(&tx_req.source_hash, &observations);
            executed[exec_idx] = Some(known_allowed_result(tx.tx_hash(), &tx_req.source_hash));
            continue;
        }

        let logs = result.logs().to_vec();
        let verdict_result = super::verdict::verdict_for(
            tx_req,
            &super::verdict::SimulatedDeposit {
                from: tx.signer(),
                to: tx.to(),
                tx_hash: tx.tx_hash(),
                value: tx.value(),
                logs: &logs,
                observations: &observations,
            },
            rules,
        );
        let (commit, keep_going) = commit_and_continue(verdict_result.verdict);
        if commit {
            evm.db_mut().commit(state);
        }
        executed[exec_idx] = Some(verdict_result);
        if !keep_going {
            break;
        }
    }
    Ok(executed)
}

/// Builds the final, request-ordered `Vec<AuditResult>` from the per-exec-index execution
/// outcomes. `built[i]` maps original request index `i` to an index into `executed` (or `None`
/// if `build_deposit_tx` failed for that item — always `Malformed`, independent of execution).
/// `executed[exec_idx]` is `None` only when the single-pass execution loop stopped (on `Audit`)
/// before reaching that index — those become `Unknown`.
fn assemble_results(
    txs: &[DepositTxRequest],
    built: &[Option<usize>],
    executed: &mut [Option<AuditResult>],
) -> Vec<AuditResult> {
    let mut results = Vec::with_capacity(txs.len());
    for (i, tx_req) in txs.iter().enumerate() {
        match built[i] {
            None => results.push(AuditResult::malformed(tx_req.source_hash.clone())),
            Some(exec_idx) => match executed[exec_idx].take() {
                Some(result) => results.push(result),
                None => results.push(AuditResult::unknown(tx_req.source_hash.clone())),
            },
        }
    }
    results
}

/// Builds the forced-`Allow` result for a tx whose `source_hash` is in the caller-supplied
/// `known_allowed` set — skips rule (re-)classification entirely (see
/// `AuditTransactionsRequest::known_allowed`'s doc comment for why).
fn known_allowed_result(tx_hash: alloy_primitives::B256, source_hash: &str) -> AuditResult {
    AuditResult {
        tx_hash: format!("{tx_hash:#x}"),
        source_hash: source_hash.to_string(),
        verdict: Verdict::Allow,
        actions: None,
    }
}

/// Reports the direct-delegatecall result of an already-approved deposit as a **diagnostic only**.
///
/// This row's verdict and state handling are unchanged: it is allowed and committed, and no
/// verdict reads anything recorded here. The limitation is deliberate and disclosed rather than
/// hidden — an already-approved deposit that executes unapproved implementation code is **not**
/// denied in this revision. Acting on it would need a caller-supplied class label on the request,
/// because this node cannot tell an ordinary already-resolved deposit from a replay prefix, and
/// the two have opposite safe defaults.
///
/// The record is bounded to counts and the fixed reason discriminant. No address, code hash or
/// other observation content is logged.
pub(super) fn record_known_allowed_diagnostic(
    source_hash: &str,
    observations: &rcs_filter::callstack::Observations,
) {
    rcs_filter::metrics::record_callstack_known_allowed_diagnostic();
    match observations {
        rcs_filter::callstack::Observations::Available { items, .. } => {
            if !items.is_empty() {
                tracing::info!(
                    target: "xlayer_audit_rpc",
                    source_hash = %source_hash,
                    observations = items.len(),
                    "already-approved deposit produced direct-delegatecall observations; \
                     reported as a diagnostic only, verdict and state unchanged"
                );
            }
        }
        rcs_filter::callstack::Observations::Unavailable(reason) => {
            tracing::info!(
                target: "xlayer_audit_rpc",
                source_hash = %source_hash,
                reason = reason.label(),
                "already-approved deposit produced no trustworthy direct-delegatecall result; \
                 reported as a diagnostic only, verdict and state unchanged"
            );
        }
    }
}

/// Modelled top-level request keys. Anything else is named and rejected by Phase A.
const AUDIT_REQUEST_KEYS: &[&str] = &["protocol_version", "rules", "txs", "known_allowed"];

/// The default `protocol_version` for a request that omits it, so every existing caller stays
/// byte-compatible.
const fn default_protocol_version() -> u32 {
    1
}

/// Per-transaction direct-delegatecall capture budget for this RPC.
///
/// Threaded in explicitly because this struct carries no other configuration: the value comes
/// from the same `--rcs-filter.*` argument group the builder uses, so the two budgets cannot
/// drift. The audit RPC is deliberately **not** gated by `--rcs-filter.enabled` — it adjudicates
/// the rules carried in the request, not the node's installed snapshot — and only the budget
/// *value* is read from that group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditCaptureConfig {
    pub max_callstack_observations_per_tx: usize,
}

impl Default for AuditCaptureConfig {
    fn default() -> Self {
        Self { max_callstack_observations_per_tx: 10_000 }
    }
}

/// `xlayer_auditTransactions` request.
///
/// `rules` holds each rule's **original JSON bytes**. The elements are completely isomorphic to
/// `GET /rules` rule objects and go through the same per-rule phase, so the rule schema and its
/// validation order apply verbatim and the request can never be trimmed to event-only.
///
/// `Deserialize` is hand-written for the same two reasons as the `GET /rules` envelope:
/// `#[serde(flatten)]` cannot coexist with a raw value, and the visitor must be able to name an
/// unknown or repeated top-level key. It also makes the Phase A order explicit — unknown
/// top-level key first, then `protocol_version` — rather than leaving it to serde's field order.
#[derive(Debug)]
pub struct AuditTransactionsRequest {
    /// Omitted means `1`. Only `1` is accepted; any other value is `-32602`.
    pub protocol_version: u32,
    /// Original bytes per rule. Never `Value`, never a map, never re-serialized.
    pub rules: Vec<Box<serde_json::value::RawValue>>,
    pub txs: Vec<DepositTxRequest>,
    /// `source_hash` values RCS has already resolved to Allow: both original deposits that
    /// RCS's adjudicator approved in an earlier round and synthetic ban deposits prepended for
    /// replay. Executed and committed unconditionally, skipping rule (re-)classification — a
    /// `quota`-type rule match is unconditional on the tx's own logs and has no memory of a
    /// prior decision, so re-classifying it would just produce `audit` again forever.
    ///
    /// **Disclosed limitation of this round:** a deposit listed here that executes unapproved
    /// implementation code is **not denied**. The node cannot distinguish an ordinary
    /// already-approved deposit from a synthetic ban replay prefix — this is a flat list with no
    /// kind label, and the two classes have opposite safe defaults — so its callstack result is
    /// recorded as a bounded diagnostic only and no verdict depends on it. This is the direct
    /// cost of keeping `known_allowed` semantics unchanged without a producer-supplied class
    /// label, and it is a known limitation, not an oversight.
    pub known_allowed: Vec<String>,
}

impl<'de> serde::Deserialize<'de> for AuditTransactionsRequest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RequestVisitor;

        impl<'de> serde::de::Visitor<'de> for RequestVisitor {
            type Value = AuditTransactionsRequest;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an xlayer_auditTransactions request object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut tracker = rcs_filter::rules::raw::EnvelopeKeyTracker::new();
                let mut protocol_version: Option<u32> = None;
                let mut rules: Option<Vec<Box<serde_json::value::RawValue>>> = None;
                let mut txs: Option<Vec<DepositTxRequest>> = None;
                let mut known_allowed: Option<Vec<String>> = None;

                while let Some(key) = map.next_key::<String>()? {
                    let repeated = tracker.already_seen(&key);
                    tracker.record(&key, AUDIT_REQUEST_KEYS);
                    // A repeated key's value is consumed but discarded, so the first occurrence
                    // is never silently overwritten before the duplicate is reported.
                    if repeated {
                        map.next_value::<serde::de::IgnoredAny>()?;
                        continue;
                    }
                    match key.as_str() {
                        "protocol_version" => protocol_version = Some(map.next_value()?),
                        "rules" => rules = Some(map.next_value()?),
                        "txs" => txs = Some(map.next_value()?),
                        "known_allowed" => known_allowed = Some(map.next_value()?),
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }

                // Phase A, step 1: an unknown (then a repeated) top-level key, named.
                if let Some(problem) = tracker.problem() {
                    return Err(serde::de::Error::custom(problem));
                }

                // Phase A, step 2: the version, naming the received value and the supported set.
                let protocol_version = protocol_version.unwrap_or_else(default_protocol_version);
                if !rcs_filter::config::is_supported_protocol(protocol_version) {
                    return Err(serde::de::Error::custom(
                        rcs_filter::rules::raw::unsupported_protocol_reason(protocol_version),
                    ));
                }

                Ok(AuditTransactionsRequest {
                    protocol_version,
                    rules: rules.ok_or_else(|| serde::de::Error::missing_field("rules"))?,
                    txs: txs.ok_or_else(|| serde::de::Error::missing_field("txs"))?,
                    known_allowed: known_allowed.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_map(RequestVisitor)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditTransactionsResponse {
    pub results: Vec<AuditResult>,
}

#[rpc(server, namespace = "xlayer")]
pub trait XlayerAuditApi {
    /// Classifies each deposit tx in `req.txs` against `req.rules`, without side effects —
    /// except for txs whose `source_hash` is in `req.known_allowed`, which are force-classified
    /// `allow` without running `req.rules` against them at all (see
    /// `AuditTransactionsRequest::known_allowed`).
    ///
    /// **Only meaningful for deposits not yet recorded in `TxBlacklist`.** If a deposit passed
    /// here has *already* been blacklisted on-chain, the underlying EVM (see the L1 force-tx
    /// intercept design) skips its payload before this handler ever sees any logs from it — the
    /// resulting verdict will misleadingly read `allow` (no logs to match against), not a
    /// reflection of the prior blacklist decision. This does not affect on-chain safety (the
    /// blacklisted deposit's payload is already guaranteed to be skipped independently of
    /// anything this RPC reports), but callers re-querying a previously-decided deposit for
    /// audit/debugging purposes should not treat its verdict here as authoritative — check
    /// `TxBlacklist` directly instead.
    #[method(name = "auditTransactions")]
    async fn audit_transactions(
        &self,
        req: AuditTransactionsRequest,
    ) -> RpcResult<AuditTransactionsResponse>;
}

/// Implementation struct, following the existing `XlayerRpcExt<T>` pattern
/// (`crates/rpc/src/default.rs`).
#[derive(Debug)]
pub struct XlayerAuditRpc<T> {
    pub backend: Arc<T>,
    /// Per-transaction direct-delegatecall capture budget, injected rather than hard-coded.
    pub capture: AuditCaptureConfig,
}

#[jsonrpsee::core::async_trait]
impl<T> XlayerAuditApiServer for XlayerAuditRpc<T>
where
    T: Call + LoadState + Clone + Send + Sync + 'static,
    // Deposit txs (`Recovered<OpTxEnvelope>`) only convert into a `TxEnv` when `T`'s configured
    // EVM factory is `OpEvmFactory` (that's where `FromRecoveredTx<OpTxEnvelope>` is defined).
    // Pin it down so `evm.transact(tx)` below type-checks for any `T`, not just one concrete node.
    <T as RpcNodeCore>::Evm:
        ConfigureEvm<BlockExecutorFactory: BlockExecutorFactory<EvmFactory = OpEvmFactory>>,
{
    async fn audit_transactions(
        &self,
        req: AuditTransactionsRequest,
    ) -> RpcResult<AuditTransactionsResponse> {
        // Phase B: the per-rule phase, from the request's preserved rule bytes. The request does
        // not accept a `content_version`; honouring one would imply a persistence this path does
        // not have, so the snapshot identity uses 0 for it.
        let rules = load_rules_from_raw(req.protocol_version, 0, &req.rules);
        if !rules.rejected.is_empty() {
            // Fail the whole request rather than silently classifying against a rule set
            // that's missing the rule(s) the caller actually asked for — a caller testing a
            // specific rule must not get a misleadingly "clean" verdict back.
            return Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32602,
                format!("{} rule(s) failed validation and were rejected", rules.rejected.len()),
                Some(
                    rules
                        .rejected
                        .iter()
                        .map(|r| serde_json::json!({"id": r.id, "reason": r.reason}))
                        .collect::<Vec<_>>(),
                ),
            ));
        }

        let known_allowed: std::collections::HashSet<String> =
            req.known_allowed.iter().cloned().collect();

        // Build every tx up front so a per-item construction failure produces a `Malformed`
        // result without aborting the rest of the batch.
        let mut ordered_txs = Vec::with_capacity(req.txs.len());
        let mut ordered_tx_reqs: Vec<DepositTxRequest> = Vec::with_capacity(req.txs.len());
        let mut built: Vec<Option<usize>> = Vec::with_capacity(req.txs.len());
        for tx_req in &req.txs {
            match build_deposit_tx(tx_req) {
                Ok(tx) => {
                    built.push(Some(ordered_txs.len()));
                    ordered_tx_reqs.push(tx_req.clone());
                    ordered_txs.push(tx);
                }
                Err(_err) => {
                    built.push(None);
                }
            }
        }

        // Fetch the header FIRST and pin state to that exact block hash (rather than
        // independently resolving "latest" twice). If a new block landed between two
        // independent "latest" lookups, state could come from block N while the EVM env is
        // built for block N+1 — a mismatched pairing. Deriving the state's `BlockId` from the
        // header's own hash guarantees both reference the identical block.
        let latest_header = self
            .backend
            .provider()
            .latest_header()
            .map_err(|e| {
                jsonrpsee::types::ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>)
            })?
            .ok_or_else(|| {
                jsonrpsee::types::ErrorObjectOwned::owned(-32000, "no latest header", None::<()>)
            })?;
        let evm_env = self.backend.evm_config().evm_env(&latest_header).map_err(|e| {
            jsonrpsee::types::ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>)
        })?;

        // `ordered_txs`/`ordered_tx_reqs`/`txs`/`built`/`rules` all move into the closure below:
        // assembling each request's `AuditResult` has to happen there too, since the executed
        // subset's logs (`ordered_txs`-indexed) never leave the blocking task.
        let backend = self.backend.as_ref().clone();
        let block_id = BlockId::hash(latest_header.hash());
        let txs = req.txs;
        // The request's own snapshot identity. `content_version` is 0 because a request carries no
        // persisted version and honouring one would imply a persistence this path does not have;
        // it matches how the request's rules are compiled.
        // Copied out of `self` before the closure below takes ownership of what it needs.
        let capture_budget = self.capture.max_callstack_observations_per_tx;
        let capture_config = rules.declares_any_alias().then(|| DirectDelegatecallConfig {
            aliases: rules.alias_addresses().into_iter().collect(),
            stamp: rules.snapshot_stamp(),
            max_observations: capture_budget,
        });
        let results: Vec<AuditResult> = backend
            .spawn_with_state_at_block(block_id, move |this, mut state_db| {
                // Built WITH the shared capture inspector, in the mode that drives the
                // direct-delegatecall channel only: this path screens the execution result's own
                // logs, so it needs no native log stream, while the frame logic that identifies a
                // direct delegatecall is the SAME code the block builder runs. That shared code is
                // what makes "both paths agree on the same frames" a testable property.
                //
                // When no rule selects an address, no configuration is installed below and every
                // frame hook returns before any frame work, so the response is unchanged.
                let inspector = RcsInspector::new_callstack_only(capture_budget);
                let mut evm =
                    this.evm_config().evm_with_env_and_inspector(&mut state_db, evm_env, inspector);

                let mut executed = classify_ordered_deposits(
                    &mut evm,
                    &ordered_txs,
                    &ordered_tx_reqs,
                    &rules,
                    &capture_config,
                    &known_allowed,
                )
                .map_err(T::Error::from_evm_err)?;

                Ok(assemble_results(&txs, &built, &mut executed))
            })
            .await
            .map_err(Into::into)?;

        Ok(AuditTransactionsResponse { results })
    }
}

#[cfg(test)]
mod request_contract_tests {
    use super::{AuditCaptureConfig, AuditTransactionsRequest};
    use rcs_filter::rules::load_rules_from_raw;

    const PROXY1: &str = "0x1111111111111111111111111111111111111111";
    const PROXY2: &str = "0x2222222222222222222222222222222222222222";

    /// A syntactically valid deposit tx entry, so `txs` is never the reason a request fails.
    const TX: &str = r#"{"source_hash":"0xa","from":"0x0404040404040404040404040404040404040404","mint":"0","value":"0","gas_limit":100000,"is_system_transaction":false,"data":"0x"}"#;

    /// Decodes a request from **exact bytes**, exactly as the JSON-RPC layer does. A decode
    /// failure is what the layer turns into `-32602`.
    fn decode(body: &str) -> Result<AuditTransactionsRequest, String> {
        serde_json::from_str(body).map_err(|e| e.to_string())
    }

    /// An omitted `protocol_version` means 1, so every existing caller is
    /// byte-compatible and unaffected.
    #[test]
    fn an_omitted_protocol_version_defaults_to_one() {
        let req = decode(&format!(r#"{{"rules":[],"txs":[{TX}]}}"#)).expect("decodes");
        assert_eq!(req.protocol_version, 1);
        assert!(req.known_allowed.is_empty(), "known_allowed defaults to empty");
    }

    #[test]
    fn protocol_version_one_is_accepted_explicitly() {
        let req = decode(&format!(r#"{{"protocol_version":1,"rules":[],"txs":[{TX}]}}"#))
            .expect("decodes");
        assert_eq!(req.protocol_version, 1);
    }

    /// Any value other than 1, including 2, is rejected naming the value AND the
    /// supported set. The v2 selector contract is not part of this protocol.
    #[test]
    fn any_protocol_version_other_than_one_is_rejected_naming_value_and_supported_set() {
        for version in [0u32, 2, 3, 99] {
            let error =
                decode(&format!(r#"{{"protocol_version":{version},"rules":[],"txs":[{TX}]}}"#))
                    .expect_err("must be rejected");
            assert!(error.contains(&version.to_string()), "{error}");
            assert!(error.contains('1'), "must name the supported set: {error}");
        }
    }

    /// An unknown top-level key is cited AHEAD of the version and of any rule problem,
    /// proving the Phase A order is real and not an artifact of decode order.
    #[test]
    fn t53_unknown_top_level_key_is_cited_ahead_of_version_and_rule_problems() {
        let malformed_rule =
            r#"{"id":"m","event_abis":{},"callstack":7,"condition":true,"action":"deny"}"#;
        let error = decode(&format!(
            r#"{{"surprise":1,"protocol_version":2,"rules":[{malformed_rule}],"txs":[{TX}]}}"#
        ))
        .expect_err("must be rejected");
        assert!(error.contains("surprise"), "must cite the unknown key: {error}");
        assert!(!error.contains("unsupported protocol_version"), "version came first: {error}");
        assert!(!error.contains("callstack"), "rule error came first: {error}");
    }

    /// With no unknown top-level key, the version is cited, not the rule.
    #[test]
    fn t54_version_is_cited_when_there_is_no_unknown_top_level_key() {
        let malformed_rule =
            r#"{"id":"m","event_abis":{},"callstack":7,"condition":true,"action":"deny"}"#;
        let error =
            decode(&format!(r#"{{"protocol_version":2,"rules":[{malformed_rule}],"txs":[{TX}]}}"#))
                .expect_err("must be rejected");
        assert!(error.contains("unsupported protocol_version"), "{error}");
        assert!(!error.contains("callstack"), "the rule must not be cited first: {error}");
    }

    /// The verdict is not a function of input key order.
    #[test]
    fn t56_reordering_top_level_keys_does_not_change_the_verdict() {
        let a = format!(r#"{{"surprise":1,"protocol_version":2,"rules":[],"txs":[{TX}]}}"#);
        let b = format!(r#"{{"txs":[{TX}],"rules":[],"protocol_version":2,"surprise":1}}"#);
        assert_eq!(
            decode(&a).expect_err("rejected"),
            decode(&b).expect_err("rejected"),
            "Phase A must not depend on key order"
        );
    }

    #[test]
    fn a_repeated_top_level_key_is_named() {
        let error = decode(&format!(r#"{{"rules":[],"rules":[],"txs":[{TX}]}}"#))
            .expect_err("must be rejected");
        assert!(error.contains("rules") && error.contains("more than once"), "{error}");
    }

    #[test]
    fn missing_required_top_level_fields_still_fail_decoding() {
        assert!(decode(r#"{"rules":[]}"#).is_err(), "txs is required");
        assert!(decode(&format!(r#"{{"txs":[{TX}]}}"#)).is_err(), "rules is required");
    }

    /// At THIS entry point too: a duplicate alias inside a request rule is rejected, which is
    /// only possible because the request preserves each rule's original bytes.
    #[test]
    fn a_duplicate_alias_in_a_request_rule_is_rejected_through_this_entry_point() {
        let rule = format!(
            r#"{{"id":"dup","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY1}","p":"{PROXY2}"}}}},"condition":true,"action":"deny"}}"#
        );
        let req = decode(&format!(r#"{{"rules":[{rule}],"txs":[{TX}]}}"#)).expect("decodes");
        // The bytes survived the envelope decode.
        assert_eq!(req.rules[0].get().matches(r#""p""#).count(), 2);
        // And the per-rule phase rejects it, so the handler returns -32602 with nothing executed.
        let set = load_rules_from_raw(req.protocol_version, 0, &req.rules);
        assert!(set.rules.is_empty());
        assert_eq!(set.rejected.len(), 1);
        assert!(set.rejected[0].reason.contains("more than once"));
    }

    /// At THIS entry point too: every rejected rule is reported, each with its id and reason.
    #[test]
    fn t55_all_rejected_request_rules_are_reported_together() {
        let v0 = r#"{"id":"r-v0","event_abis":{},"callStack":{},"condition":true,"action":"deny"}"#;
        let v4 = r#"{"id":"r-v4","event_abis":{},"callstack":{"direct_delegatecalls":{"p":"0x111111111111111111111111111111111111111A"}},"condition":true,"action":"deny"}"#;
        let req = decode(&format!(r#"{{"rules":[{v0},{v4}],"txs":[{TX}]}}"#)).expect("decodes");
        let set = load_rules_from_raw(req.protocol_version, 0, &req.rules);
        assert_eq!(set.rejected.len(), 2, "{:?}", set.rejected);
        let ids: Vec<&str> = set.rejected.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"r-v0") && ids.contains(&"r-v4"), "{ids:?}");
    }

    /// A valid direct-delegatecall rule is accepted on this path, so the request can carry the
    /// new schema rather than being limited to event-only rules.
    #[test]
    fn a_valid_direct_delegatecall_request_rule_compiles_on_this_path() {
        let rule = format!(
            r#"{{"id":"ok","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY1}"}}}},"condition":true,"action":"deny"}}"#
        );
        let req = decode(&format!(r#"{{"rules":[{rule}],"txs":[{TX}]}}"#)).expect("decodes");
        let set = load_rules_from_raw(req.protocol_version, 0, &req.rules);
        assert!(set.rejected.is_empty(), "{:?}", set.rejected);
        assert_eq!(set.rules.len(), 1);
        assert!(set.declares_any_alias());
    }

    #[test]
    fn known_allowed_is_read_when_present() {
        let req = decode(&format!(r#"{{"rules":[],"txs":[{TX}],"known_allowed":["0xa","0xb"]}}"#))
            .expect("decodes");
        assert_eq!(req.known_allowed, vec!["0xa".to_string(), "0xb".to_string()]);
    }

    #[test]
    fn the_capture_budget_default_matches_the_filter_default() {
        assert_eq!(AuditCaptureConfig::default().max_callstack_observations_per_tx, 10_000);
    }
}

#[cfg(test)]
mod algorithm_tests {
    use super::super::verdict::Verdict;
    use super::commit_and_continue;

    #[test]
    fn allow_commits_and_continues() {
        assert_eq!(commit_and_continue(Verdict::Allow), (true, true));
    }

    #[test]
    fn deny_discards_and_continues() {
        assert_eq!(commit_and_continue(Verdict::Deny), (false, true));
    }

    #[test]
    fn audit_discards_and_stops() {
        assert_eq!(commit_and_continue(Verdict::Audit), (false, false));
    }

    #[test]
    fn malformed_discards_and_continues() {
        assert_eq!(commit_and_continue(Verdict::Malformed), (false, true));
    }

    use super::super::deposit::DepositTxRequest;
    use super::super::verdict::AuditResult;
    use super::assemble_results;

    fn req(source_hash: &str) -> DepositTxRequest {
        DepositTxRequest {
            source_hash: source_hash.to_string(),
            from: "0x0404040404040404040404040404040404040404".to_string(),
            to: None,
            mint: "0".to_string(),
            value: "0".to_string(),
            gas_limit: 100_000,
            is_system_transaction: false,
            data: "0x".to_string(),
        }
    }

    #[test]
    fn malformed_entry_stays_malformed_regardless_of_executed_state() {
        let txs = vec![req("0xa")];
        let built = vec![None]; // build_deposit_tx failed for this one
        let mut executed: Vec<Option<AuditResult>> = vec![];

        let results = assemble_results(&txs, &built, &mut executed);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verdict, Verdict::Malformed);
        assert_eq!(results[0].source_hash, "0xa");
    }

    #[test]
    fn executed_entry_is_used_as_is() {
        let txs = vec![req("0xa")];
        let built = vec![Some(0)];
        let mut executed = vec![Some(AuditResult {
            tx_hash: "0xhash".to_string(),
            source_hash: "0xa".to_string(),
            verdict: Verdict::Allow,
            actions: None,
        })];

        let results = assemble_results(&txs, &built, &mut executed);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verdict, Verdict::Allow);
        assert_eq!(results[0].tx_hash, "0xhash");
    }

    #[test]
    fn never_executed_entry_becomes_unknown() {
        // Simulates: this tx was built successfully (built[0] = Some(0)) but the execution loop
        // stopped before reaching it (an earlier tx in the batch hit Audit) — executed[0] stays
        // None. This is the exact bug-fix scenario: previously this tx would have been executed
        // and (wrongly) classified anyway.
        let txs = vec![req("0xb")];
        let built = vec![Some(0)];
        let mut executed: Vec<Option<AuditResult>> = vec![None];

        let results = assemble_results(&txs, &built, &mut executed);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verdict, Verdict::Unknown);
        assert_eq!(results[0].source_hash, "0xb");
    }

    #[test]
    fn mixed_batch_preserves_original_order() {
        let txs = vec![req("0xa"), req("0xb"), req("0xc")];
        let built = vec![Some(0), None, Some(1)];
        let mut executed = vec![
            Some(AuditResult {
                tx_hash: "0xhashA".to_string(),
                source_hash: "0xa".to_string(),
                verdict: Verdict::Allow,
                actions: None,
            }),
            None, // 0xc was built (built[2] = Some(1)) but never executed
        ];

        let results = assemble_results(&txs, &built, &mut executed);

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].source_hash, "0xa");
        assert_eq!(results[0].verdict, Verdict::Allow);
        assert_eq!(results[1].source_hash, "0xb");
        assert_eq!(results[1].verdict, Verdict::Malformed);
        assert_eq!(results[2].source_hash, "0xc");
        assert_eq!(results[2].verdict, Verdict::Unknown);
    }

    use super::known_allowed_result;
    use alloy_primitives::B256;

    #[test]
    fn known_allowed_result_is_allow_with_no_actions() {
        let result = known_allowed_result(B256::ZERO, "0xsrc");
        assert_eq!(result.verdict, Verdict::Allow);
        assert_eq!(result.source_hash, "0xsrc");
        assert_eq!(result.tx_hash, format!("{:#x}", B256::ZERO));
        assert!(result.actions.is_none());
    }

    #[test]
    fn known_allowed_membership_check_matches_by_source_hash() {
        let known_allowed: std::collections::HashSet<String> =
            ["0xsrc1".to_string(), "0xsrc2".to_string()].into_iter().collect();
        assert!(known_allowed.contains("0xsrc1"));
        assert!(!known_allowed.contains("0xsrc3"));
    }
}
