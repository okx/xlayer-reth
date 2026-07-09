//! Golden fixtures — verbatim from contract §4. Both teams reuse these exact constants.

use std::str::FromStr;

use alloy_primitives::{Address, B256, U256};

/// ERC20 bridge (transfer `from`).
pub const BRIDGE_ERC20: &str = "0x0101010101010101010101010101010101010101";
/// ERC20 token contract (`log.address`).
pub const TOKEN_X: &str = "0x0202020202020202020202020202020202020202";
/// Withdrawal recipient (`to`).
pub const RECIPIENT: &str = "0x0303030303030303030303030303030303030303";
/// Claiming EOA (`tx.origin`) for scenarios a/c/d.
pub const ORIGIN: &str = "0x0404040404040404040404040404040404040404";
/// `tx.to` claim/relayer contract.
pub const CLAIM_CONTRACT: &str = "0x0505050505050505050505050505050505050505";
/// Compromised/blacklisted contract (scenario b).
pub const BLACKLISTED_FROM: &str = "0x0606060606060606060606060606060606060606";
/// Scenario b originating EOA.
pub const ORIGIN_B: &str = "0x0707070707070707070707070707070707070707";

/// Fixed tx hashes (contract §4).
pub const TX_A: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const TX_B: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const TX_C: &str = "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
pub const TX_D: &str = "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

/// One token = 18-digit precision integer string (contract §4).
pub const ONE_TOKEN: &str = "1000000000000000000";
/// Three tokens (scenario c requested amount).
pub const THREE_TOKENS: &str = "3000000000000000000";
/// Two tokens (scenario d amount).
pub const TWO_TOKENS: &str = "2000000000000000000";

/// Scenario a rule (`action=audit`, quota, ERC20 Transfer, `contract_address=null`).
pub const RULE_SCENARIO_A: &str = r#"{
  "id": "risk-check-erc20-tokenx-bridge",
  "contract_address": null,
  "origin": null,
  "event_abis": {
    "transfer": {
      "type": "event", "name": "Transfer",
      "inputs": [
        { "name": "from", "type": "address", "indexed": true },
        { "name": "to", "type": "address", "indexed": true },
        { "name": "value", "type": "uint256", "indexed": false }
      ],
      "anonymous": false
    }
  },
  "audit_types": ["quota"],
  "condition": {
    "and": [
      { "==": [{ "var": "transfer.address" }, "0x0202020202020202020202020202020202020202"] },
      { "==": [{ "var": "transfer.from" }, "0x0101010101010101010101010101010101010101"] }
    ]
  },
  "action": "audit",
  "audit_timeout_action": "allow"
}"#;

/// Scenario b rule (`action=deny`, blacklist `in`).
pub const RULE_SCENARIO_B: &str = r#"{
  "id": "blk-compromised-contract",
  "contract_address": null,
  "origin": null,
  "event_abis": {
    "transfer": {
      "type": "event", "name": "Transfer",
      "inputs": [
        { "name": "from", "type": "address", "indexed": true },
        { "name": "to", "type": "address", "indexed": true },
        { "name": "value", "type": "uint256", "indexed": false }
      ],
      "anonymous": false
    }
  },
  "condition": { "in": [{ "var": "transfer.from" }, ["0x0606060606060606060606060606060606060606"]] },
  "action": "deny"
}"#;

/// Parses a fixed address constant (panics on malformed input — test-only).
pub fn addr(s: &str) -> Address {
    Address::from_str(s).expect("valid fixed address")
}

pub fn bridge_erc20() -> Address {
    addr(BRIDGE_ERC20)
}
pub fn token_x() -> Address {
    addr(TOKEN_X)
}
pub fn recipient() -> Address {
    addr(RECIPIENT)
}
pub fn origin() -> Address {
    addr(ORIGIN)
}
pub fn claim_contract() -> Address {
    addr(CLAIM_CONTRACT)
}
pub fn blacklisted_from() -> Address {
    addr(BLACKLISTED_FROM)
}
pub fn origin_b() -> Address {
    addr(ORIGIN_B)
}

pub fn tx_a() -> B256 {
    B256::from_str(TX_A).expect("valid tx hash")
}
pub fn tx_b() -> B256 {
    B256::from_str(TX_B).expect("valid tx hash")
}
pub fn tx_c() -> B256 {
    B256::from_str(TX_C).expect("valid tx hash")
}
pub fn tx_d() -> B256 {
    B256::from_str(TX_D).expect("valid tx hash")
}

pub fn one_token() -> U256 {
    U256::from_str(ONE_TOKEN).expect("valid amount")
}
