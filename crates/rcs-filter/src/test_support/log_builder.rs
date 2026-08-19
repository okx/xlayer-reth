//! Builders for synthetic execution logs used by matching / handle tests.

use alloy_dyn_abi::DynSolValue;
use alloy_primitives::{keccak256, Address, Bytes, Log, LogData, U256};

/// Builds an ERC20 `Transfer(address indexed from, address indexed to, uint256 value)` log
/// emitted by `token`. Topics: `[topic0, from, to]`; data: 32-byte big-endian `value`.
pub fn erc20_transfer(token: Address, from: Address, to: Address, value: U256) -> Log {
    let topic0 = keccak256("Transfer(address,address,uint256)".as_bytes());
    let topics = vec![topic0, from.into_word(), to.into_word()];
    let data = Bytes::copy_from_slice(&value.to_be_bytes::<32>());
    Log { address: token, data: LogData::new_unchecked(topics, data) }
}

/// Builds an ERC1155 `TransferSingle` log with indexed operator/from/to and ABI-encoded
/// `uint256 id` / `uint256 value` in the data body.
pub fn erc1155_transfer_single(
    token: Address,
    operator: Address,
    from: Address,
    to: Address,
    id: U256,
    value: U256,
) -> Log {
    let topic0 = keccak256("TransferSingle(address,address,address,uint256,uint256)".as_bytes());
    let topics = vec![topic0, operator.into_word(), from.into_word(), to.into_word()];
    let body = DynSolValue::Tuple(vec![DynSolValue::Uint(id, 256), DynSolValue::Uint(value, 256)]);
    Log {
        address: token,
        data: LogData::new_unchecked(topics, Bytes::from(body.abi_encode_params())),
    }
}

/// Builds an ERC1155 `TransferBatch` log with indexed operator/from/to and ABI-encoded
/// `uint256[] ids` / `uint256[] values` in the data body.
pub fn erc1155_transfer_batch(
    token: Address,
    operator: Address,
    from: Address,
    to: Address,
    ids: Vec<U256>,
    values: Vec<U256>,
) -> Log {
    let topic0 = keccak256("TransferBatch(address,address,address,uint256[],uint256[])".as_bytes());
    let topics = vec![topic0, operator.into_word(), from.into_word(), to.into_word()];
    let body = DynSolValue::Tuple(vec![
        DynSolValue::Array(ids.into_iter().map(|id| DynSolValue::Uint(id, 256)).collect()),
        DynSolValue::Array(values.into_iter().map(|value| DynSolValue::Uint(value, 256)).collect()),
    ]);
    Log {
        address: token,
        data: LogData::new_unchecked(topics, Bytes::from(body.abi_encode_params())),
    }
}
