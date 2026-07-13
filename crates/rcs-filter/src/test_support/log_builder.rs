//! Builders for synthetic execution logs used by matching / handle tests.

use alloy_primitives::{keccak256, Address, Bytes, Log, LogData, U256};

/// Builds an ERC20 `Transfer(address indexed from, address indexed to, uint256 value)` log
/// emitted by `token`. Topics: `[topic0, from, to]`; data: 32-byte big-endian `value`.
pub fn erc20_transfer(token: Address, from: Address, to: Address, value: U256) -> Log {
    let topic0 = keccak256("Transfer(address,address,uint256)".as_bytes());
    let topics = vec![topic0, from.into_word(), to.into_word()];
    let data = Bytes::copy_from_slice(&value.to_be_bytes::<32>());
    Log { address: token, data: LogData::new_unchecked(topics, data) }
}
