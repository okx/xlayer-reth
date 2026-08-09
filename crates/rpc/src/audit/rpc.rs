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

use alloy_consensus::Transaction;
use alloy_eips::BlockId;
use alloy_evm::Evm;
use alloy_op_evm::OpEvmFactory;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use rcs_filter::rules::{load_rules, RawRule};
use reth_evm::{block::BlockExecutorFactory, ConfigureEvm};
use reth_rpc_eth_api::{
    helpers::{Call, LoadState},
    FromEvmError, RpcNodeCore,
};
use reth_storage_api::BlockReaderIdExt;
use revm::{context_interface::result::ResultAndState, DatabaseCommit};

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

#[derive(Debug, serde::Deserialize)]
pub struct AuditTransactionsRequest {
    pub rules: Vec<RawRule>,
    pub txs: Vec<DepositTxRequest>,
    /// `source_hash` values RCS has already resolved to Allow: both original deposits that
    /// RCS's adjudicator approved in an earlier round and synthetic ban deposits prepended for
    /// replay. Executed and committed unconditionally, skipping rule (re-)classification — a
    /// `quota`-type rule match is unconditional on the tx's own logs and has no memory of a
    /// prior decision, so re-classifying it would just produce `audit` again forever. See
    /// `docs/superpowers/specs/2026-07-24-audit-transactions-multiround-rpc-design.md` §5.5.
    #[serde(default)]
    pub known_allowed: Vec<String>,
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
}

#[jsonrpsee::core::async_trait]
impl<T> XlayerAuditApiServer for XlayerAuditRpc<T>
where
    T: Call + LoadState + Clone + Send + Sync + 'static,
    // Deposit txs (`Recovered<OpTxEnvelope>`) only convert into a `TxEnv` when `T`'s configured
    // EVM factory is `OpEvmFactory` (that's where `FromRecoveredTx<OpTxEnvelope>` is defined).
    // Pin it down so `evm.transact(tx)` below type-checks for any `T`, not just one concrete node.
    <T as RpcNodeCore>::Evm: ConfigureEvm<
        BlockExecutorFactory: BlockExecutorFactory<EvmFactory = OpEvmFactory>,
    >,
{
    async fn audit_transactions(
        &self,
        req: AuditTransactionsRequest,
    ) -> RpcResult<AuditTransactionsResponse> {
        let rules = load_rules(1, 0, req.rules);
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
        let results: Vec<AuditResult> = backend
            .spawn_with_state_at_block(block_id, move |this, mut state_db| {
                let mut evm = this.evm_config().evm_with_env(&mut state_db, evm_env);

                let mut executed: Vec<Option<AuditResult>> =
                    (0..ordered_txs.len()).map(|_| None).collect();
                for (exec_idx, tx) in ordered_txs.iter().enumerate() {
                    let tx_req = &ordered_tx_reqs[exec_idx];
                    let ResultAndState { result, state } =
                        evm.transact(tx).map_err(T::Error::from_evm_err)?;

                    if known_allowed.contains(&tx_req.source_hash) {
                        evm.db_mut().commit(state);
                        executed[exec_idx] =
                            Some(known_allowed_result(tx.tx_hash(), &tx_req.source_hash));
                        continue;
                    }

                    let logs = result.logs().to_vec();
                    let verdict_result = super::verdict::verdict_for(
                        tx_req,
                        tx.signer(),
                        tx.to(),
                        tx.tx_hash(),
                        tx.value(),
                        &logs,
                        &rules,
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

                Ok(assemble_results(&txs, &built, &mut executed))
            })
            .await
            .map_err(Into::into)?;

        Ok(AuditTransactionsResponse { results })
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
