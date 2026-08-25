//! Heavily influenced by [reth](https://github.com/paradigmxyz/reth/blob/1e965caf5fa176f244a31c0d2662ba1b590938db/crates/optimism/payload/src/builder.rs#L570)
use alloy_primitives::{Address, U256};
use core::time::Duration;
use derive_more::Display;
use op_revm::OpTransactionError;
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};

#[derive(Debug, Display)]
pub enum TxnExecutionResult {
    TransactionDALimitExceeded,
    #[display("BlockDALimitExceeded: total_da_used={_0} tx_da_size={_1} block_da_limit={_2}")]
    BlockDALimitExceeded(u64, u64, u64),
    #[display("TransactionGasLimitExceeded: total_gas_used={_0} tx_gas_limit={_1}")]
    TransactionGasLimitExceeded(u64, u64, u64),
    #[display("GaslessBlockGasLimitExceeded: cumulative={_0} tx_gas_used={_1} limit={_2}")]
    GaslessBlockGasLimitExceeded(u64, u64, u64),
    SequencerTransaction,
    NonceTooLow,
    InteropFailed,
    #[display("InternalError({_0})")]
    InternalError(OpTransactionError),
    EvmError,
    Success,
    Reverted,
    RevertedAndExcluded,
    MaxGasUsageExceeded,
}

#[derive(Default, Debug)]
pub struct ExecutionInfo {
    /// All executed transactions (unrecovered).
    pub executed_transactions: Vec<OpTransactionSigned>,
    /// The recovered senders for the executed transactions.
    pub executed_senders: Vec<Address>,
    /// The transaction receipts
    pub receipts: Vec<OpReceipt>,
    /// All gas used so far
    pub cumulative_gas_used: u64,
    /// Cumulative gas used by gasless transactions in the current block.
    pub cumulative_gasless_gas_used: u64,
    /// Set once a gasless tx is rejected for exceeding the per-block gasless budget.
    pub gasless_budget_exhausted: bool,
    /// Estimated DA size
    pub cumulative_da_bytes_used: u64,
    /// Tracks fees from executed mempool transactions
    pub total_fees: U256,
    /// DA Footprint Scalar for Jovian
    pub da_footprint_scalar: Option<u16>,
    /// Optional blob fields for payload validation
    pub optional_blob_fields: Option<(Option<u64>, Option<u64>)>,
    /// Accumulated active EVM execution and block-build time carried into the
    /// finally-selected payload. Summed across the fallback base and every
    /// flashblock batch that contributes to that payload. Deliberately excludes
    /// flashblock scheduling waits, websocket/p2p propagation, pre-resolve
    /// queuing, engine-tree insertion, and persistence, so it reflects the real
    /// execution cost of producing the block rather than downstream handling.
    pub active_execution_elapsed: Duration,
    /// Number of flashblock batches whose execution is inherited by the final
    /// payload (0 for a fallback-only / `no_tx_pool` payload).
    pub inherited_flashblocks: u64,
}

impl ExecutionInfo {
    /// Create a new instance with allocated slots.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            executed_transactions: Vec::with_capacity(capacity),
            executed_senders: Vec::with_capacity(capacity),
            receipts: Vec::with_capacity(capacity),
            cumulative_gas_used: 0,
            cumulative_gasless_gas_used: 0,
            gasless_budget_exhausted: false,
            cumulative_da_bytes_used: 0,
            total_fees: U256::ZERO,
            da_footprint_scalar: None,
            optional_blob_fields: None,
            active_execution_elapsed: Duration::ZERO,
            inherited_flashblocks: 0,
        }
    }

    /// Adds one active execution/build batch to the accumulated processing time.
    /// Saturating so a pathological duration can never wrap.
    pub fn add_active_execution(&mut self, elapsed: Duration) {
        self.active_execution_elapsed = self.active_execution_elapsed.saturating_add(elapsed);
    }

    /// Records one flashblock batch inherited by the final payload: its active
    /// execution/build time plus one towards the flashblock count.
    pub fn record_flashblock_execution(&mut self, elapsed: Duration) {
        self.add_active_execution(elapsed);
        self.inherited_flashblocks += 1;
    }

    /// Returns true if the transaction would exceed the block limits:
    /// - block gas limit: ensures the transaction still fits into the block.
    /// - tx DA limit: if configured, ensures the tx does not exceed the maximum allowed DA limit
    ///   per tx.
    /// - block DA limit: if configured, ensures the transaction's DA size does not exceed the
    ///   maximum allowed DA limit per block.
    #[allow(clippy::too_many_arguments)]
    pub fn is_tx_over_limits(
        &self,
        tx_da_size: u64,
        block_gas_limit: u64,
        tx_data_limit: Option<u64>,
        block_data_limit: Option<u64>,
        tx_gas_limit: u64,
        da_footprint_gas_scalar: Option<u16>,
        block_da_footprint_limit: Option<u64>,
    ) -> Result<(), TxnExecutionResult> {
        if tx_data_limit.is_some_and(|da_limit| tx_da_size > da_limit) {
            return Err(TxnExecutionResult::TransactionDALimitExceeded);
        }
        let total_da_bytes_used = self.cumulative_da_bytes_used.saturating_add(tx_da_size);
        if block_data_limit.is_some_and(|da_limit| total_da_bytes_used > da_limit) {
            return Err(TxnExecutionResult::BlockDALimitExceeded(
                self.cumulative_da_bytes_used,
                tx_da_size,
                block_data_limit.unwrap_or_default(),
            ));
        }

        // Post Jovian: the tx DA footprint must be less than the block gas limit
        if let Some(da_footprint_gas_scalar) = da_footprint_gas_scalar {
            let tx_da_footprint =
                total_da_bytes_used.saturating_mul(da_footprint_gas_scalar as u64);
            if tx_da_footprint > block_da_footprint_limit.unwrap_or(block_gas_limit) {
                return Err(TxnExecutionResult::BlockDALimitExceeded(
                    total_da_bytes_used,
                    tx_da_size,
                    tx_da_footprint,
                ));
            }
        }

        if self.cumulative_gas_used.saturating_add(tx_gas_limit) > block_gas_limit {
            return Err(TxnExecutionResult::TransactionGasLimitExceeded(
                self.cumulative_gas_used,
                tx_gas_limit,
                block_gas_limit,
            ));
        }
        Ok(())
    }
}

/// Computes execution throughput in gas per second for a completed payload.
///
/// Returns `None` when either input is zero, so callers never surface `NaN`,
/// infinity, or a fabricated throughput: a block that used no gas has no
/// meaningful throughput, and a zero elapsed time would divide by zero. The
/// returned value is always finite by construction.
pub fn gas_throughput_per_sec(gas_used: u64, processing_elapsed: Duration) -> Option<f64> {
    let seconds = processing_elapsed.as_secs_f64();
    if gas_used == 0 || seconds <= 0.0 {
        return None;
    }
    Some(gas_used as f64 / seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gas_throughput_normal_case_is_finite() {
        // 21_000 gas over 1ms => 21_000_000 gas/s.
        let tp = gas_throughput_per_sec(21_000, Duration::from_millis(1)).unwrap();
        assert!(tp.is_finite());
        assert!((tp - 21_000_000.0).abs() < 1.0);
    }

    #[test]
    fn gas_throughput_zero_gas_is_none() {
        // A block that used no gas has no throughput; must not be NaN/0-div.
        assert_eq!(gas_throughput_per_sec(0, Duration::from_millis(5)), None);
    }

    #[test]
    fn gas_throughput_zero_elapsed_is_none() {
        // Zero elapsed must never produce infinity.
        assert_eq!(gas_throughput_per_sec(1_000_000, Duration::ZERO), None);
    }

    #[test]
    fn gas_throughput_both_zero_is_none() {
        assert_eq!(gas_throughput_per_sec(0, Duration::ZERO), None);
    }

    #[test]
    fn gas_throughput_large_values_stay_finite() {
        let tp = gas_throughput_per_sec(u64::MAX, Duration::from_nanos(1)).unwrap();
        assert!(tp.is_finite() && tp > 0.0);
    }

    #[test]
    fn add_active_execution_accumulates() {
        let mut info = ExecutionInfo::with_capacity(0);
        assert_eq!(info.active_execution_elapsed, Duration::ZERO);
        info.add_active_execution(Duration::from_millis(3));
        info.add_active_execution(Duration::from_millis(7));
        assert_eq!(info.active_execution_elapsed, Duration::from_millis(10));
        // Fallback contribution alone does not count as a flashblock.
        assert_eq!(info.inherited_flashblocks, 0);
    }

    #[test]
    fn record_flashblock_execution_counts_batches() {
        let mut info = ExecutionInfo::with_capacity(0);
        info.record_flashblock_execution(Duration::from_millis(2));
        info.record_flashblock_execution(Duration::from_millis(4));
        assert_eq!(info.active_execution_elapsed, Duration::from_millis(6));
        assert_eq!(info.inherited_flashblocks, 2);
    }

    #[test]
    fn add_active_execution_saturates() {
        let mut info = ExecutionInfo::with_capacity(0);
        info.add_active_execution(Duration::MAX);
        info.add_active_execution(Duration::from_secs(1));
        // Saturating add keeps it at MAX rather than wrapping/panicking.
        assert_eq!(info.active_execution_elapsed, Duration::MAX);
    }
}
