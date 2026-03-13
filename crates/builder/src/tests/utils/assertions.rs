use super::signers::builder_signer;
use alloy_network::TransactionResponse;
use alloy_primitives::{Address, TxHash};
use alloy_rpc_types_eth::{Block, BlockTransactionHashes};
use op_alloy_rpc_types::Transaction;

/// Result of builder transaction validation in a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuilderTxInfo {
    /// Number of builder transactions found in the block.
    pub count: usize,
    /// Indices of builder transactions within the block.
    pub indices: Vec<usize>,
}

impl BuilderTxInfo {
    /// Returns true if the block contains at least one builder transaction.
    pub fn has_builder_tx(&self) -> bool {
        self.count > 0
    }
}

pub trait BlockTransactionsExt {
    fn includes(&self, txs: &impl AsTxs) -> bool;
}

/// Extension trait for validating builder transactions in blocks.
pub trait BuilderTxValidation {
    /// Checks if the block contains builder transactions from the configured builder address.
    /// Returns information about builder transactions found in the block.
    fn find_builder_txs(&self) -> BuilderTxInfo;

    /// Returns true if the block contains at least one builder transaction.
    fn has_builder_tx(&self) -> bool {
        self.find_builder_txs().has_builder_tx()
    }

    /// Asserts that the block contains exactly the expected number of builder transactions.
    fn assert_builder_tx_count(&self, expected: usize) {
        let info = self.find_builder_txs();
        assert_eq!(
            info.count, expected,
            "Expected {} builder transaction(s), found {} at indices {:?}",
            expected, info.count, info.indices
        );
    }
}

impl BuilderTxValidation for Block<Transaction> {
    fn find_builder_txs(&self) -> BuilderTxInfo {
        let builder_address = builder_signer().address;
        let mut indices = Vec::new();

        for (idx, tx) in self.transactions.txns().enumerate() {
            if tx.from() == builder_address {
                indices.push(idx);
            }
        }

        BuilderTxInfo { count: indices.len(), indices }
    }
}

impl BlockTransactionsExt for Block<Transaction> {
    fn includes(&self, txs: &impl AsTxs) -> bool {
        txs.as_txs().into_iter().all(|tx| self.transactions.hashes().any(|included| included == tx))
    }
}

impl BlockTransactionsExt for BlockTransactionHashes<'_, Transaction> {
    fn includes(&self, txs: &impl AsTxs) -> bool {
        let mut included_tx_iter = self.clone();
        txs.as_txs().iter().all(|tx| included_tx_iter.any(|included| included == *tx))
    }
}

pub trait AsTxs {
    fn as_txs(&self) -> Vec<TxHash>;
}

impl AsTxs for TxHash {
    fn as_txs(&self) -> Vec<TxHash> {
        vec![*self]
    }
}

impl AsTxs for Vec<TxHash> {
    fn as_txs(&self) -> Vec<TxHash> {
        self.clone()
    }
}

/// Counts transactions with a given `to` address in a block's transaction list.
pub fn count_txs_to(txs: &[Transaction], to: Address) -> usize {
    use alloy_consensus::Transaction as _;
    txs.iter().map(|tx| tx.to()).filter(|t| *t == Some(to)).count()
}
