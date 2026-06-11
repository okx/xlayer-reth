//! FR-1 — ingress mempool validator wrapper.
//!
//! [`XLayerBlacklistTxValidator`] wraps op-reth's `OpTransactionValidator` (any
//! [`TransactionValidator`]) and pre-screens the top-level `to` then `from` against the
//! current blacklist snapshot before delegating. On a hit it rejects with a
//! [`BlacklistRejected`] error (whose `Display` is the fixed [`POOL_REJECT_MESSAGE`]; the
//! RPC layer maps a pool `Invalid(... Other(...))` to JSON-RPC code `-32000`). It is a
//! best-effort filter — the execution gate (FR-2) backstops any snapshot lag.
//!
//! Grounded signatures (verbatim):
//! - `TransactionValidator` trait — reth `crates/transaction-pool/src/validate/mod.rs:170-240`
//!   (`type Transaction: PoolTransaction`, `type Block: Block`, async `validate_transaction`,
//!   default `on_new_head_block(&self, &SealedBlock<Self::Block>)`).
//! - `PoolTransaction: alloy_consensus::Transaction + …` — `traits.rs:1253`; `sender()` at
//!   `:1329`; `to()` via the `alloy_consensus::Transaction` supertrait (`None` for Create).
//! - `PoolTransactionError { is_bad_transaction, as_any }` — `error.rs:16`;
//!   `InvalidPoolTransactionError::Other(Box<dyn PoolTransactionError>)` — `error.rs:285`.
//! - delegation shape mirrors op-reth `OpTransactionValidator` impl `txpool/src/validator.rs:293-318`.

use crate::runtime::BlacklistRuntimeCtx;
use alloy_consensus::Transaction as _;
use reth_primitives_traits::SealedBlock;
use reth_transaction_pool::{
    error::{InvalidPoolTransactionError, PoolTransactionError},
    PoolTransaction, TransactionOrigin, TransactionValidationOutcome, TransactionValidator,
};
use std::any::Any;

/// Pool error for a blacklist-rejected transaction. `Display` == [`POOL_REJECT_MESSAGE`]
/// (no dynamic fields); the RPC layer attaches code `-32000`.
#[derive(Debug, thiserror::Error)]
#[error("xlayer-blacklist: sender or recipient is on the blacklist")]
pub struct BlacklistRejected;

impl PoolTransactionError for BlacklistRejected {
    fn is_bad_transaction(&self) -> bool {
        // Deterministic hard reject — warrants peer penalisation (mirrors op-reth
        // `InvalidCrossTx::CrossChainTxPreInterop`).
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Wraps an inner [`TransactionValidator`] with the chain-level blacklist ingress check.
#[derive(Debug, Clone)]
pub struct XLayerBlacklistTxValidator<V> {
    inner: V,
    ctx: BlacklistRuntimeCtx,
}

impl<V> XLayerBlacklistTxValidator<V> {
    /// Wrap `inner`, sharing the runtime context (snapshot handle + metrics).
    pub fn new(inner: V, ctx: BlacklistRuntimeCtx) -> Self {
        Self { inner, ctx }
    }
}

impl<V> TransactionValidator for XLayerBlacklistTxValidator<V>
where
    V: TransactionValidator,
{
    type Transaction = V::Transaction;
    type Block = V::Block;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        let snapshot = self.ctx.load_snapshot();
        // Empty snapshot (disabled chain / not yet populated) → allow unconditionally.
        if !snapshot.is_empty() {
            // Top-level `to` first (no signature recovery needed; `None` for Create).
            if let Some(to) = transaction.to()
                && snapshot.contains(&to)
            {
                return self.reject(transaction);
            }
            // Then top-level `from`. The pool tx is already recovered, so reading the
            // sender never fails — a from-recovery failure cannot reject here (DM-1.3).
            if snapshot.contains(&transaction.sender()) {
                return self.reject(transaction);
            }
        }
        self.inner.validate_transaction(origin, transaction).await
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock<Self::Block>) {
        // Delegate so the inner validator keeps updating its fork/L1 state.
        self.inner.on_new_head_block(new_tip_block);
    }
}

impl<V> XLayerBlacklistTxValidator<V> {
    fn reject<T: PoolTransaction>(&self, transaction: T) -> TransactionValidationOutcome<T> {
        self.ctx.record_pool_reject();
        TransactionValidationOutcome::Invalid(
            transaction,
            InvalidPoolTransactionError::Other(Box::new(BlacklistRejected)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xlayer_blacklist::POOL_REJECT_MESSAGE;

    #[test]
    fn rejected_message_matches_cross_client_constant() {
        // The thiserror Display must stay byte-identical to the shared constant.
        assert_eq!(BlacklistRejected.to_string(), POOL_REJECT_MESSAGE);
    }

    #[test]
    fn rejected_is_bad_transaction() {
        assert!(BlacklistRejected.is_bad_transaction());
    }
}
