use alloy_primitives::B256;
use xlayer_trace_monitor::{from_b256, get_global_tracer, TransactionProcessId};

pub(crate) fn monitor(block_number: u64, tx_hashes: Vec<B256>) {
    // For X Layer. Log transaction execution end even for failed transactions
    if let Some(tracer) = get_global_tracer() {
        for tx_hash in tx_hashes.iter() {
            tracer.log_transaction(
                from_b256(*tx_hash),
                TransactionProcessId::SeqTxExecutionEnd,
                Some(block_number),
            );
        }
    }
}
