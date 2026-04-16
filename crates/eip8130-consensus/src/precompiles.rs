//! Precompile handlers for EIP-8130 NonceManager and TxContext.
//!
//! These precompiles are read-only and provide access to AA transaction
//! metadata and 2D nonce state.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::{sol_data, SolCall, SolType};
use revm::database::Database;

use super::{
    abi::{INonceManager, ITxContext},
    accessors::read_nonce,
    execution::TxContextValues,
};

type CallTupleArray = sol_data::Array<sol_data::Array<(sol_data::Address, sol_data::Bytes)>>;

/// Gas cost for TxContext precompile calls (pure memory read).
pub const TX_CONTEXT_GAS: u64 = 100;

/// Gas cost for NonceManager precompile calls (storage read).
pub const NONCE_MANAGER_GAS: u64 = 2_100;

/// Handles a call to the NonceManager precompile.
///
/// Decodes `getNonce(address, uint256)` and returns the current nonce.
pub fn handle_nonce_manager<DB: Database>(
    db: &mut DB,
    input: &[u8],
) -> Result<(u64, Bytes), PrecompileError> {
    let call = INonceManager::getNonceCall::abi_decode(input)
        .map_err(|_| PrecompileError::InvalidInput)?;

    let nonce_key = U256::from(call.nonceKey);
    let nonce =
        read_nonce(db, call.account, nonce_key).map_err(|_| PrecompileError::DatabaseError)?;

    let encoded = <sol_data::Uint<64>>::abi_encode(&nonce);
    Ok((NONCE_MANAGER_GAS, Bytes::from(encoded)))
}

/// Handles a call to the TxContext precompile.
///
/// Routes to the appropriate getter based on the function selector.
pub fn handle_tx_context(
    ctx: &TxContextValues,
    input: &[u8],
) -> Result<(u64, Bytes), PrecompileError> {
    if input.len() < 4 {
        return Err(PrecompileError::InvalidInput);
    }

    let selector = [input[0], input[1], input[2], input[3]];

    let output = match selector {
        _ if selector == ITxContext::getSenderCall::SELECTOR => {
            <sol_data::Address>::abi_encode(&ctx.sender)
        }
        _ if selector == ITxContext::getPayerCall::SELECTOR => {
            <sol_data::Address>::abi_encode(&ctx.payer)
        }
        _ if selector == ITxContext::getOwnerIdCall::SELECTOR => {
            <sol_data::FixedBytes<32>>::abi_encode(&ctx.owner_id)
        }
        _ if selector == ITxContext::getMaxCostCall::SELECTOR => {
            <sol_data::Uint<256>>::abi_encode(&ctx.max_cost)
        }
        _ if selector == ITxContext::getGasLimitCall::SELECTOR => {
            <sol_data::Uint<256>>::abi_encode(&U256::from(ctx.gas_limit))
        }
        _ if selector == ITxContext::getCallsCall::SELECTOR => {
            <CallTupleArray>::abi_encode(&ctx.calls)
        }
        _ => return Err(PrecompileError::UnknownSelector),
    };

    Ok((TX_CONTEXT_GAS, Bytes::from(output)))
}

/// Errors that can occur in precompile handling.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrecompileError {
    /// Input data could not be decoded.
    #[error("invalid precompile input")]
    InvalidInput,
    /// Database read failed.
    #[error("database error in precompile")]
    DatabaseError,
    /// Unknown function selector.
    #[error("unknown function selector")]
    UnknownSelector,
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256};

    use super::*;

    #[test]
    fn tx_context_get_sender() {
        let ctx = TxContextValues {
            sender: Address::repeat_byte(0xAA),
            payer: Address::repeat_byte(0xBB),
            owner_id: B256::repeat_byte(0xCC),
            gas_limit: 1_000_000,
            max_cost: U256::from(1_000_000_000u64),
            calls: Vec::new(),
        };

        let mut input = Vec::new();
        input.extend_from_slice(&ITxContext::getSenderCall::SELECTOR);
        let (gas, output) = handle_tx_context(&ctx, &input).unwrap();

        assert_eq!(gas, TX_CONTEXT_GAS);
        let decoded: Address = <sol_data::Address>::abi_decode(&output).unwrap();
        assert_eq!(decoded, Address::repeat_byte(0xAA));
    }

    #[test]
    fn tx_context_get_payer() {
        let ctx = TxContextValues { payer: Address::repeat_byte(0xBB), ..Default::default() };

        let mut input = Vec::new();
        input.extend_from_slice(&ITxContext::getPayerCall::SELECTOR);
        let (_, output) = handle_tx_context(&ctx, &input).unwrap();

        let decoded: Address = <sol_data::Address>::abi_decode(&output).unwrap();
        assert_eq!(decoded, Address::repeat_byte(0xBB));
    }

    #[test]
    fn tx_context_get_gas_limit() {
        let ctx = TxContextValues { gas_limit: 42_000, ..Default::default() };

        let mut input = Vec::new();
        input.extend_from_slice(&ITxContext::getGasLimitCall::SELECTOR);
        let (_, output) = handle_tx_context(&ctx, &input).unwrap();

        let decoded: U256 = <sol_data::Uint<256>>::abi_decode(&output).unwrap();
        assert_eq!(decoded, U256::from(42_000));
    }

    #[test]
    fn tx_context_invalid_selector() {
        let ctx = TxContextValues::default();
        let input = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(matches!(handle_tx_context(&ctx, &input), Err(PrecompileError::UnknownSelector)));
    }

    #[test]
    fn tx_context_short_input() {
        let ctx = TxContextValues::default();
        let input = [0xFF, 0xFF];
        assert!(matches!(handle_tx_context(&ctx, &input), Err(PrecompileError::InvalidInput)));
    }
}
