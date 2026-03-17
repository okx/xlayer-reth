use alloy_primitives::{Address, B256, U256};
use tracing::{debug};

/// keccak256("BridgeEvent(uint8,uint32,address,uint32,address,uint256,bytes,uint32)")
pub const BRIDGE_EVENT_SIGNATURE: B256 = B256::new([
    0x50, 0x17, 0x81, 0x20, 0x9a, 0x1f, 0x88, 0x99, 0x32, 0x3b, 0x96, 0xb4, 0xef, 0x08, 0xb1, 0x68,
    0xdf, 0x93, 0xe0, 0xa9, 0x0c, 0x67, 0x3d, 0x1e, 0x4c, 0xce, 0x39, 0x36, 0x6c, 0xb6, 0x2f, 0x9b,
]);

#[derive(Debug, Default, Clone)]
pub struct BridgeInterceptConfig {
    pub enabled: bool,
    pub bridge_contract_address: Address,
    pub target_token_address: Address,
    /// true = intercept all bridge txs for this contract; false = only intercept target_token
    pub wildcard: bool,
}

/// Intercept error type distinguishing two intercept modes.
#[derive(Debug, thiserror::Error)]
pub enum BridgeInterceptError {
    #[error("bridge transaction blocked (wildcard): bridge={bridge_contract}, sender={sender}")]
    WildcardBlock { bridge_contract: Address, sender: Address },

    #[error("bridge transaction blocked: token={token}, amount={amount}, sender={sender}")]
    TargetTokenBlock { token: Address, amount: U256, sender: Address },
}

/// Check whether this transaction should be intercepted.
/// Returns `Err(BridgeInterceptError)` to intercept; `Ok(())` to allow.
/// `ExecutionResult::logs()` returns an empty slice for Revert/Halt, so callers
/// don't need to handle those cases separately.
pub fn intercept_bridge_transaction_if_need(
    logs: &[alloy_primitives::Log],
    tx_sender: Address,
    config: &BridgeInterceptConfig,
) -> Result<(), BridgeInterceptError> {
    if !config.enabled {
        return Ok(());
    }
    for log in logs {
        if log.address != config.bridge_contract_address {
            continue;
        }
        // Specific token mode: parse BridgeEvent and match originAddress
        let event = match parse_bridge_event(log) {
            Ok(e) => e,
            Err(e) => {
                debug!(target: "payload_builder", error = ?e, "ignore other bridge events");
                continue;
            }
        };
        // Wildcard mode: any log from the bridge contract triggers intercept
        if config.wildcard {
            debug!(
                target: "payload_builder",
                bridge_contract = ?config.bridge_contract_address,
                tx_sender = ?tx_sender,
                "Bridge transaction intercepted (wildcard mode)"
            );
            return Err(BridgeInterceptError::WildcardBlock {
                bridge_contract: config.bridge_contract_address,
                sender: tx_sender,
            });
        }
        if event.origin_address == config.target_token_address {
            debug!(
                target: "payload_builder",
                token = ?config.target_token_address,
                amount = ?event.amount,
                tx_sender = ?tx_sender,
                "Bridge tx for target token intercepted"
            );
            return Err(BridgeInterceptError::TargetTokenBlock {
                token: config.target_token_address,
                amount: event.amount,
                sender: tx_sender,
            });
        }
    }
    Ok(())
}

struct BridgeEventData {
    origin_address: Address,
    amount: U256,
}

/// Parse BridgeEvent log data (ABI-encoded, offsets verified against mainnet data).
///
/// event BridgeEvent(
///     uint8  leafType,             // slot 0  (offset   0-31)
///     uint32 originNetwork,        // slot 1  (offset  32-63)
///     address originAddress,       // slot 2  (offset  64-95)  ← bytes[76..96]
///     uint32 destinationNetwork,   // slot 3  (offset  96-127)
///     address destinationAddress,  // slot 4  (offset 128-159)
///     uint256 amount,              // slot 5  (offset 160-191) ← bytes[160..192]
///     bytes metadata,              // slot 6  (offset 192-223, dynamic)
///     uint32 depositCount          // slot 7  (offset 224-255)
/// )
fn parse_bridge_event(log: &alloy_primitives::Log) -> Result<BridgeEventData, ParseError> {
    if log.topics().first() != Some(&BRIDGE_EVENT_SIGNATURE) {
        return Err(ParseError::InvalidSignature);
    }
    let data = log.data.data.as_ref();
    if data.len() < 256 {
        return Err(ParseError::InsufficientData);
    }
    // address is right-aligned in a 32-byte slot: slot2[12..32] = data[76..96]
    let origin_address = Address::from_slice(&data[76..96]);
    let amount = U256::from_be_slice(&data[160..192]);
    Ok(BridgeEventData { origin_address, amount })
}

#[derive(Debug)]
enum ParseError {
    InvalidSignature,
    InsufficientData,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, LogData};

    fn make_bridge_log(bridge_contract: Address, data_hex: &str) -> alloy_primitives::Log {
        let data_bytes = alloy_primitives::hex::decode(data_hex).expect("valid hex");
        alloy_primitives::Log {
            address: bridge_contract,
            data: LogData::new(vec![BRIDGE_EVENT_SIGNATURE], data_bytes.into())
                .expect("valid log data"),
        }
    }

    const BRIDGE: Address = address!("2a3dd3eb832af982ec71669e178424b10dca2ede");
    const TOKEN: Address = address!("75231f58b43240c9718dd58b4967c5114342a86c");
    const OTHER: Address = address!("1111111111111111111111111111111111111111");
    const SENDER: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    // Real mainnet BridgeEvent data (256 bytes / 512 hex chars each)
    const DATA1: &str = "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
75231f58b43240c9718dd58b4967c5114342a86c\
0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000bf7624b8a72797fe35ba1505587fc8a39705740c\
000000000000000000000000000000000000000000000000008e1bc9bf040000\
00000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000001c97";

    const DATA2: &str = "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
75231f58b43240c9718dd58b4967c5114342a86c\
0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000151ed65c4451661313848d07a615dec1f0d4ad25\
0000000000000000000000000000000000000000000000000000000000000001\
00000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000001caa";

    const DATA3: &str = "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
75231f58b43240c9718dd58b4967c5114342a86c\
0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000696314d0f50d0dfb3f6c8de9f33d9e546b1dfbed\
000000000000000000000000000000000000000000000000c12dc63fa9700000\
00000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000001c90";

    #[test]
    fn test_parse_mainnet_data1() {
        let log = make_bridge_log(BRIDGE, DATA1);
        let event = parse_bridge_event(&log).expect("should parse");
        assert_eq!(event.origin_address, TOKEN);
        assert_eq!(event.amount, U256::from(40_000_000_000_000_000u64));
    }

    #[test]
    fn test_parse_mainnet_data2() {
        let log = make_bridge_log(BRIDGE, DATA2);
        let event = parse_bridge_event(&log).expect("should parse");
        assert_eq!(event.origin_address, TOKEN);
        assert_eq!(event.amount, U256::from(1u64));
    }

    #[test]
    fn test_parse_mainnet_data3() {
        let log = make_bridge_log(BRIDGE, DATA3);
        let event = parse_bridge_event(&log).expect("should parse");
        assert_eq!(event.origin_address, TOKEN);
        assert_eq!(event.amount, U256::from(13_920_000_000_000_000_000u128));
    }

    #[test]
    fn test_wildcard_intercept() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: true,
        };
        let log = make_bridge_log(BRIDGE, DATA1);
        let result = intercept_bridge_transaction_if_need(&[log], SENDER, &config);
        assert!(matches!(result, Err(BridgeInterceptError::WildcardBlock { .. })));
    }

    #[test]
    fn test_specific_token_intercept() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: false,
        };
        let log = make_bridge_log(BRIDGE, DATA1);
        let result = intercept_bridge_transaction_if_need(&[log], SENDER, &config);
        assert!(matches!(result, Err(BridgeInterceptError::TargetTokenBlock { .. })));
    }

    #[test]
    fn test_non_target_token_allowed() {
        // Use a different token as target — DATA1's originAddress is TOKEN, so it won't match OTHER
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: OTHER,
            wildcard: false,
        };
        let log = make_bridge_log(BRIDGE, DATA1);
        let result = intercept_bridge_transaction_if_need(&[log], SENDER, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_non_bridge_contract_ignored() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: true,
        };
        // Log from a different contract address
        let mut log = make_bridge_log(BRIDGE, DATA1);
        log.address = OTHER;
        let result = intercept_bridge_transaction_if_need(&[log], SENDER, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_disabled_config_allows() {
        let config = BridgeInterceptConfig {
            enabled: false,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: true,
        };
        let log = make_bridge_log(BRIDGE, DATA1);
        let result = intercept_bridge_transaction_if_need(&[log], SENDER, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_empty_logs_allowed() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: true,
        };
        let result = intercept_bridge_transaction_if_need(&[], SENDER, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_invalid_signature() {
        let log = alloy_primitives::Log {
            address: BRIDGE,
            data: LogData::new(vec![B256::ZERO], vec![0u8; 256].into()).expect("valid log data"),
        };
        assert!(matches!(parse_bridge_event(&log), Err(ParseError::InvalidSignature)));
    }

    #[test]
    fn test_parse_insufficient_data() {
        let log = alloy_primitives::Log {
            address: BRIDGE,
            data: LogData::new(vec![BRIDGE_EVENT_SIGNATURE], vec![0u8; 100].into())
                .expect("valid log data"),
        };
        assert!(matches!(parse_bridge_event(&log), Err(ParseError::InsufficientData)));
    }

    #[test]
    fn test_parse_bridge_event_empty_topics() {
        let log = alloy_primitives::Log {
            address: BRIDGE,
            data: LogData::new(vec![], vec![0u8; 256].into()).expect("valid log data"),
        };
        assert!(matches!(parse_bridge_event(&log), Err(ParseError::InvalidSignature)));
    }

    #[test]
    fn test_intercept_multiple_logs() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: false,
        };
        // First log from a different contract (ignored), second is the valid target log
        let mut other_log = make_bridge_log(BRIDGE, DATA1);
        other_log.address = OTHER;
        let target_log = make_bridge_log(BRIDGE, DATA1);
        let result =
            intercept_bridge_transaction_if_need(&[other_log, target_log], SENDER, &config);
        assert!(matches!(result, Err(BridgeInterceptError::TargetTokenBlock { .. })));
    }

    #[test]
    fn test_intercept_wildcard_ignores_other_contracts() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: true,
        };
        // All logs are from OTHER, not BRIDGE – wildcard must not fire
        let mut log1 = make_bridge_log(BRIDGE, DATA1);
        log1.address = OTHER;
        let mut log2 = make_bridge_log(BRIDGE, DATA2);
        log2.address = OTHER;
        let result = intercept_bridge_transaction_if_need(&[log1, log2], SENDER, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_intercept_invalid_event_in_logs() {
        let config = BridgeInterceptConfig {
            enabled: true,
            bridge_contract_address: BRIDGE,
            target_token_address: TOKEN,
            wildcard: false,
        };
        // First log: wrong signature → parse error → skipped via continue
        let invalid_log = alloy_primitives::Log {
            address: BRIDGE,
            data: LogData::new(vec![B256::ZERO], vec![0u8; 256].into()).expect("valid log data"),
        };
        // Second log: valid and matches target token
        let valid_log = make_bridge_log(BRIDGE, DATA1);
        let result =
            intercept_bridge_transaction_if_need(&[invalid_log, valid_log], SENDER, &config);
        assert!(matches!(result, Err(BridgeInterceptError::TargetTokenBlock { .. })));
    }

    #[test]
    fn test_bridge_event_signature_constant() {
        // keccak256("BridgeEvent(uint8,uint32,address,uint32,address,uint256,bytes,uint32)")
        let expected = [
            0x50u8, 0x17, 0x81, 0x20, 0x9a, 0x1f, 0x88, 0x99, 0x32, 0x3b, 0x96, 0xb4, 0xef, 0x08,
            0xb1, 0x68, 0xdf, 0x93, 0xe0, 0xa9, 0x0c, 0x67, 0x3d, 0x1e, 0x4c, 0xce, 0x39, 0x36,
            0x6c, 0xb6, 0x2f, 0x9b,
        ];
        assert_eq!(BRIDGE_EVENT_SIGNATURE.0, expected);
    }
}
