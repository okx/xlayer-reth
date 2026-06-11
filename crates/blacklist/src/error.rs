//! FR-7 — pool rejection error contract (cross-client constants).

/// JSON-RPC error code returned when the ingress mempool validator rejects a tx
/// because its top-level `from`/`to` is on the blacklist. [TD §4.2 / §4.7]
pub const POOL_REJECT_CODE: i32 = -32000;

/// Fixed rejection message. Contains **no** dynamic fields (no address) so the
/// response is byte-identical regardless of which address hit. [TD §4.2 / §4.7]
pub const POOL_REJECT_MESSAGE: &str = "xlayer-blacklist: sender or recipient is on the blacklist";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_reject_code_is_minus_32000() {
        assert_eq!(POOL_REJECT_CODE, -32000);
    }

    #[test]
    fn pool_reject_message_is_fixed_and_address_free() {
        assert_eq!(
            POOL_REJECT_MESSAGE,
            "xlayer-blacklist: sender or recipient is on the blacklist"
        );
        // Negative constraint: message must not embed any dynamic 0x address.
        assert!(!POOL_REJECT_MESSAGE.contains("0x"));
    }
}
