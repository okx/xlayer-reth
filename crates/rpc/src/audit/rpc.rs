//! `xlayer_auditTransactions` JSON-RPC method: builds deposit txs from the request, executes
//! them one at a time against chain-tip state (each commit visible to the next tx in the
//! batch), and classifies each with the pure `verdict_for`.
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

#[derive(Debug, serde::Deserialize)]
pub struct AuditTransactionsRequest {
    pub rules: Vec<RawRule>,
    pub txs: Vec<DepositTxRequest>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditTransactionsResponse {
    pub results: Vec<AuditResult>,
}

#[rpc(server, namespace = "xlayer")]
pub trait XlayerAuditApi {
    /// Classifies each deposit tx in `req.txs` against `req.rules`, without side effects.
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

        // Build every tx up front so a per-item construction failure produces a `Malformed`
        // result without aborting the rest of the batch.
        let mut ordered_txs = Vec::with_capacity(req.txs.len());
        let mut built: Vec<Option<usize>> = Vec::with_capacity(req.txs.len());
        for tx_req in &req.txs {
            match build_deposit_tx(tx_req) {
                Ok(tx) => {
                    built.push(Some(ordered_txs.len()));
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

        // `ordered_txs`/`txs`/`built`/`rules` all move into the closure below: assembling each
        // request's `AuditResult` has to happen there too, since the executed subset's logs
        // (`ordered_txs`-indexed) never leave the blocking task.
        let backend = self.backend.as_ref().clone();
        let block_id = BlockId::hash(latest_header.hash());
        let txs = req.txs;
        let results: Vec<AuditResult> = backend
            .spawn_with_state_at_block(block_id, move |this, mut state_db| {
                let mut evm = this.evm_config().evm_with_env(&mut state_db, evm_env);

                let mut executed_logs = Vec::with_capacity(ordered_txs.len());
                for tx in &ordered_txs {
                    let ResultAndState { result, state } =
                        evm.transact(tx).map_err(T::Error::from_evm_err)?;
                    evm.db_mut().commit(state);
                    executed_logs.push(result.logs().to_vec());
                }

                let mut results = Vec::with_capacity(txs.len());
                for (i, tx_req) in txs.iter().enumerate() {
                    match built[i] {
                        None => results.push(AuditResult::malformed(tx_req.source_hash.clone())),
                        Some(exec_idx) => {
                            let tx = &ordered_txs[exec_idx];
                            let logs = &executed_logs[exec_idx];
                            results.push(super::verdict::verdict_for(
                                tx_req,
                                tx.signer(),
                                tx.to(),
                                tx.tx_hash(),
                                tx.value(),
                                logs,
                                &rules,
                            ));
                        }
                    }
                }
                Ok(results)
            })
            .await
            .map_err(Into::into)?;

        Ok(AuditTransactionsResponse { results })
    }
}
