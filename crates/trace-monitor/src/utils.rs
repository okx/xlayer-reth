use crate::transaction::TransactionProcessId;
use std::borrow::Cow;

/// Fixed chain name
const CHAIN_NAME: &str = "X Layer";

/// Fixed business name
const BUSINESS_NAME: &str = "X Layer";

/// Fixed chain ID
const CHAIN_ID: &str = "196";

/// 32-byte hash (equivalent to B256)
pub type Hash32 = [u8; 32];

/// Format a 32-byte hash as hexadecimal string with 0x prefix
pub fn format_hash_hex(hash: &Hash32) -> String {
    format!("0x{}", hex::encode(hash))
}

/// Convert from B256 (alloy-primitives) or any 32-byte array to Hash32
pub fn from_b256(b256: impl AsRef<[u8; 32]>) -> Hash32 {
    *b256.as_ref()
}

/// Format CSV line with 23 fields.
pub(crate) fn format_csv_line(
    trace: &str,
    process_id: TransactionProcessId,
    current_time: u128,
    block_hash: Option<Hash32>,
    block_number: Option<u64>,
) -> String {
    fn escape_csv(s: &str) -> Cow<'_, str> {
        if s.is_empty() {
            return Cow::Borrowed("");
        }

        if s.contains(',') || s.contains('"') || s.contains('\n') {
            Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
        } else {
            Cow::Borrowed(s)
        }
    }

    // Pre-compute values that need conversion
    let process_str = process_id.as_u64().to_string();
    let current_time_str = current_time.to_string();
    let block_height = block_number.map(|n| n.to_string()).unwrap_or_default();
    let block_hash_str = block_hash.map(|h| format_hash_hex(&h)).unwrap_or_default();

    // Build CSV line efficiently
    format!(
        "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        escape_csv(CHAIN_NAME),
        escape_csv(trace),
        "", // status_str (empty, no need to escape)
        escape_csv(process_id.service_name()),
        escape_csv(BUSINESS_NAME),
        "", // client (empty)
        escape_csv(CHAIN_ID),
        escape_csv(&process_str),
        escape_csv(process_id.as_str()),
        "", // index (empty)
        "", // inner_index (empty)
        escape_csv(&current_time_str),
        "", // referld (empty)
        "", // contract_address (empty)
        escape_csv(&block_height),
        escape_csv(&block_hash_str),
        "", // block_time (empty)
        "", // deposit_confirm_height (empty)
        "", // token_id (empty)
        "", // mev_supplier (empty)
        "", // business_hash (empty)
        "", // transaction_type (empty)
        ""  // ext_json (empty)
    )
}

/// Get current timestamp in milliseconds since UNIX epoch
pub(crate) fn current_timestamp_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
