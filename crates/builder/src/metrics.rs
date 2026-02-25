use alloy_primitives::{hex, Address};
use metrics::IntoF64;
use reth_metrics::{
    metrics::{gauge, Counter, Gauge, Histogram},
    Metrics,
};

use crate::{
    args::OpRbuilderArgs,
    flashtestations::attestation::{compute_workload_id_from_parsed, parse_report_body},
};

/// op-rbuilder metrics
#[derive(Metrics, Clone)]
#[metrics(scope = "op_rbuilder")]
pub struct OpRBuilderMetrics {
    /// Block built success
    pub block_built_success: Counter,
    /// Block synced success
    pub block_synced_success: Counter,
    /// Number of flashblocks added to block (Total per block)
    pub flashblock_count: Histogram,
    /// Number of messages sent
    pub messages_sent_count: Counter,
    /// Histogram of the time taken to build a block
    pub total_block_built_duration: Histogram,
    /// Latest time taken to build a block
    pub total_block_built_gauge: Gauge,
    /// Histogram of the time taken to build a Flashblock
    pub flashblock_build_duration: Histogram,
    /// Histogram of the time taken to sync a Flashblock
    pub flashblock_sync_duration: Histogram,
    /// Flashblock UTF8 payload byte size histogram
    pub flashblock_byte_size_histogram: Histogram,
    /// Histogram of transactions in a Flashblock
    pub flashblock_num_tx_histogram: Histogram,
    /// Number of invalid blocks
    pub invalid_built_blocks_count: Counter,
    /// Number of invalid synced blocks
    pub invalid_synced_blocks_count: Counter,
    /// Histogram of fetching transactions from the pool duration
    pub transaction_pool_fetch_duration: Histogram,
    /// Latest time taken to fetch tx from the pool
    pub transaction_pool_fetch_gauge: Gauge,
    /// Histogram of state root calculation duration
    pub state_root_calculation_duration: Histogram,
    /// Latest state root calculation duration
    pub state_root_calculation_gauge: Gauge,
    /// Histogram of sequencer transaction execution duration
    pub sequencer_tx_duration: Histogram,
    /// Latest sequencer transaction execution duration
    pub sequencer_tx_gauge: Gauge,
    /// Histogram of state merge transitions duration
    pub state_transition_merge_duration: Histogram,
    /// Latest state merge transitions duration
    pub state_transition_merge_gauge: Gauge,
    /// Histogram of the duration of payload simulation of all transactions
    pub payload_transaction_simulation_duration: Histogram,
    /// Latest payload simulation of all transactions duration
    pub payload_transaction_simulation_gauge: Gauge,
    /// Number of transaction considered for inclusion in the block
    pub payload_num_tx_considered: Histogram,
    /// Latest number of transactions considered for inclusion in the block
    pub payload_num_tx_considered_gauge: Gauge,
    /// Payload byte size histogram
    pub payload_byte_size: Histogram,
    /// Latest Payload byte size
    pub payload_byte_size_gauge: Gauge,
    /// Histogram of transactions in the payload
    pub payload_num_tx: Histogram,
    /// Latest number of transactions in the payload
    pub payload_num_tx_gauge: Gauge,
    /// Histogram of transactions in the payload that were successfully simulated
    pub payload_num_tx_simulated: Histogram,
    /// Latest number of transactions in the payload that were successfully simulated
    pub payload_num_tx_simulated_gauge: Gauge,
    /// Histogram of transactions in the payload that were successfully simulated
    pub payload_num_tx_simulated_success: Histogram,
    /// Latest number of transactions in the payload that were successfully simulated
    pub payload_num_tx_simulated_success_gauge: Gauge,
    /// Histogram of transactions in the payload that failed simulation
    pub payload_num_tx_simulated_fail: Histogram,
    /// Latest number of transactions in the payload that failed simulation
    pub payload_num_tx_simulated_fail_gauge: Gauge,
    /// Histogram of gas used by successful transactions
    pub successful_tx_gas_used: Histogram,
    /// Histogram of gas used by reverted transactions
    pub reverted_tx_gas_used: Histogram,
    /// Gas used by reverted transactions in the latest block
    pub payload_reverted_tx_gas_used: Gauge,
    /// Histogram of tx simulation duration
    pub tx_simulation_duration: Histogram,
    /// Byte size of transactions
    pub tx_byte_size: Histogram,
    /// How much less flashblocks we issue to be on time with block construction
    pub reduced_flashblocks_number: Histogram,
    /// How much less flashblocks we issued in reality, comparing to calculated number for block
    pub missing_flashblocks_count: Histogram,
    /// How much time we have deducted from block building time
    pub flashblocks_time_drift: Histogram,
    /// Time offset we used for first flashblock
    pub first_flashblock_time_offset: Histogram,
    /// Number of requests sent to the eth_sendBundle endpoint
    pub bundle_requests: Counter,
    /// Number of valid bundles received at the eth_sendBundle endpoint
    pub valid_bundles: Counter,
    /// Number of bundles that failed to execute
    pub failed_bundles: Counter,
    /// Histogram of eth_sendBundle request duration
    pub bundle_receive_duration: Histogram,
}

impl OpRBuilderMetrics {
    pub fn set_payload_builder_metrics(
        &self,
        payload_transaction_simulation_time: impl IntoF64 + Copy,
        num_txs_considered: impl IntoF64 + Copy,
        num_txs_simulated: impl IntoF64 + Copy,
        num_txs_simulated_success: impl IntoF64 + Copy,
        num_txs_simulated_fail: impl IntoF64 + Copy,
        reverted_gas_used: impl IntoF64,
    ) {
        self.payload_transaction_simulation_duration.record(payload_transaction_simulation_time);
        self.payload_transaction_simulation_gauge.set(payload_transaction_simulation_time);
        self.payload_num_tx_considered.record(num_txs_considered);
        self.payload_num_tx_considered_gauge.set(num_txs_considered);
        self.payload_num_tx_simulated.record(num_txs_simulated);
        self.payload_num_tx_simulated_gauge.set(num_txs_simulated);
        self.payload_num_tx_simulated_success.record(num_txs_simulated_success);
        self.payload_num_tx_simulated_success_gauge.set(num_txs_simulated_success);
        self.payload_num_tx_simulated_fail.record(num_txs_simulated_fail);
        self.payload_num_tx_simulated_fail_gauge.set(num_txs_simulated_fail);
        self.payload_reverted_tx_gas_used.set(reverted_gas_used);
    }
}

/// Set gauge metrics for some flags so we can inspect which ones are set
/// and which ones aren't.
pub fn record_flag_gauge_metrics(builder_args: &OpRbuilderArgs) {
    gauge!("op_rbuilder_flags_flashblocks_enabled").set(builder_args.flashblocks.enabled as i32);
    gauge!("op_rbuilder_flags_flashtestations_enabled")
        .set(builder_args.flashtestations.flashtestations_enabled as i32);
    gauge!("op_rbuilder_flags_enable_revert_protection")
        .set(builder_args.enable_revert_protection as i32);
}

/// Record TEE workload ID and measurement metrics
/// Parses the quote, computes workload ID, and records workload_id, mr_td (TEE measurement), and rt_mr0 (runtime measurement register 0)
/// These identify the trusted execution environment configuration provided by GCP
pub fn record_tee_metrics(raw_quote: &[u8], tee_address: &Address) -> eyre::Result<()> {
    let parsed_quote = parse_report_body(raw_quote)?;
    let workload_id = compute_workload_id_from_parsed(&parsed_quote);

    let workload_id_hex = hex::encode(workload_id);
    let mr_td_hex = hex::encode(parsed_quote.mr_td);
    let rt_mr0_hex = hex::encode(parsed_quote.rt_mr0);

    let tee_address_static: &'static str = Box::leak(tee_address.to_string().into_boxed_str());
    let workload_id_static: &'static str = Box::leak(workload_id_hex.into_boxed_str());
    let mr_td_static: &'static str = Box::leak(mr_td_hex.into_boxed_str());
    let rt_mr0_static: &'static str = Box::leak(rt_mr0_hex.into_boxed_str());

    // Record TEE address
    let tee_address_labels: [(&str, &str); 1] = [("tee_address", tee_address_static)];
    gauge!("op_rbuilder_tee_address", &tee_address_labels).set(1);

    // Record workload ID
    let workload_labels: [(&str, &str); 1] = [("workload_id", workload_id_static)];
    gauge!("op_rbuilder_tee_workload_id", &workload_labels).set(1);

    // Record MRTD (TEE measurement)
    let mr_td_labels: [(&str, &str); 1] = [("mr_td", mr_td_static)];
    gauge!("op_rbuilder_tee_mr_td", &mr_td_labels).set(1);

    // Record RTMR0 (runtime measurement register 0)
    let rt_mr0_labels: [(&str, &str); 1] = [("rt_mr0", rt_mr0_static)];
    gauge!("op_rbuilder_tee_rt_mr0", &rt_mr0_labels).set(1);

    Ok(())
}
