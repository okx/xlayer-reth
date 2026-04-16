//! AA transaction execution pipeline.
//!
//! Defines the execution steps for EIP-8130 transactions. The pipeline runs
//! within the block executor after validation has passed.
//!
//! ## Execution Order (per spec)
//!
//! 1. Check lock state (reject config changes if locked)
//! 2. Deduct gas from payer
//! 3. Increment nonce in NonceManager
//! 4. Auto-delegate bare EOAs to DEFAULT_ACCOUNT_ADDRESS
//! 5. Process `account_changes` (create + config changes)
//! 6. Populate TX context precompile
//! 7. Execute `calls` (phased atomic batching)
//! 8. Refund unused gas to payer

use alloy_primitives::{Address, Bytes, B256, U256};

use super::{
    predeploys::{
        ACCOUNT_CONFIG_ADDRESS, DEFAULT_ACCOUNT_ADDRESS, NONCE_MANAGER_ADDRESS, REVOKED_VERIFIER,
    },
    storage::{account_state_slot, encode_owner_config, nonce_slot, owner_config_slot},
    tx::TxEip8130,
    types::{ConfigChangeEntry, CreateEntry},
    validation::implicit_eoa_owner_id,
};

/// A single storage write operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageWrite {
    /// Contract address holding the storage.
    pub address: Address,
    /// Storage slot key.
    pub slot: U256,
    /// New value to write.
    pub value: U256,
}

/// Code placement operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodePlacement {
    /// Address to place code at.
    pub address: Address,
    /// Bytecode to place.
    pub code: Bytes,
}

/// A single call to execute during a phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionCall {
    /// Caller address (the sender account).
    pub caller: Address,
    /// Target address.
    pub to: Address,
    /// Calldata.
    pub data: Bytes,
    /// Value to send (always 0 for AA calls, value transfers happen via CALL).
    pub value: U256,
}

/// Values to populate in the TX context precompile.
#[derive(Debug, Clone, Default)]
pub struct TxContextValues {
    /// The sender address.
    pub sender: Address,
    /// The payer address.
    pub payer: Address,
    /// The authenticated owner ID.
    pub owner_id: B256,
    /// The gas limit.
    pub gas_limit: u64,
    /// The maximum cost.
    pub max_cost: U256,
    /// Phased calls: `calls[phase_index][call_index] = (target, data)`.
    pub calls: Vec<Vec<(Address, Bytes)>>,
}

/// Phase execution result.
#[derive(Debug, Clone)]
pub struct PhaseResult {
    /// Whether the phase succeeded.
    pub success: bool,
    /// Gas used by the phase.
    pub gas_used: u64,
}

/// Builds the nonce increment storage write.
pub fn nonce_increment_write(sender: Address, nonce_key: U256, new_sequence: u64) -> StorageWrite {
    let slot = nonce_slot(sender, nonce_key);
    StorageWrite {
        address: NONCE_MANAGER_ADDRESS,
        slot: slot.into(),
        value: U256::from(new_sequence),
    }
}

/// Builds the owner registration storage writes for a create entry.
pub fn owner_registration_writes(account: Address, create: &CreateEntry) -> Vec<StorageWrite> {
    create
        .initial_owners
        .iter()
        .map(|owner| {
            let slot = owner_config_slot(account, owner.owner_id);
            let value = encode_owner_config(owner.verifier, owner.scope);
            StorageWrite { address: ACCOUNT_CONFIG_ADDRESS, slot: slot.into(), value: value.into() }
        })
        .collect()
}

/// Builds the config change storage writes (owner registrations only).
///
/// Sequence bumps are handled separately via [`config_change_sequence`]
/// because the packed `ChangeSequences` slot requires read-modify-write.
pub fn config_change_writes(account: Address, change: &ConfigChangeEntry) -> Vec<StorageWrite> {
    use super::types::{OP_AUTHORIZE_OWNER, OP_REVOKE_OWNER};

    let mut writes = Vec::new();

    let self_owner_id = implicit_eoa_owner_id(account);
    for op in &change.owner_changes {
        let slot = owner_config_slot(account, op.owner_id);
        let value = match op.change_type {
            OP_AUTHORIZE_OWNER => encode_owner_config(op.verifier, op.scope),
            OP_REVOKE_OWNER => {
                if op.owner_id == self_owner_id {
                    encode_owner_config(REVOKED_VERIFIER, 0)
                } else {
                    B256::ZERO
                }
            }
            _ => continue,
        };
        writes.push(StorageWrite {
            address: ACCOUNT_CONFIG_ADDRESS,
            slot: slot.into(),
            value: value.into(),
        });
    }

    writes
}

/// Returns the sequence update parameters for a config change.
pub fn config_change_sequence(account: Address, change: &ConfigChangeEntry) -> SequenceUpdateInfo {
    SequenceUpdateInfo {
        slot: account_state_slot(account).into(),
        is_multichain: change.chain_id == 0,
        new_value: change.sequence + 1,
    }
}

/// Pre-computed info for a read-modify-write sequence update.
#[derive(Debug)]
pub struct SequenceUpdateInfo {
    /// Storage slot for `_accountState[account]`.
    pub slot: U256,
    /// Whether this updates the multichain (chain_id 0) or local field.
    pub is_multichain: bool,
    /// New sequence value to write.
    pub new_value: u64,
}

/// Builds the auto-delegation code: `0xef0100 || DEFAULT_ACCOUNT_ADDRESS`.
pub fn auto_delegation_code() -> Bytes {
    let mut code = Vec::with_capacity(23);
    code.extend_from_slice(&[0xef, 0x01, 0x00]);
    code.extend_from_slice(DEFAULT_ACCOUNT_ADDRESS.as_slice());
    Bytes::from(code)
}

/// Converts a `TxEip8130` into phased execution calls.
pub fn build_execution_calls(tx: &TxEip8130, sender: Address) -> Vec<Vec<ExecutionCall>> {
    tx.calls
        .iter()
        .map(|phase| {
            phase
                .iter()
                .map(|call| ExecutionCall {
                    caller: sender,
                    to: call.to,
                    data: call.data.clone(),
                    value: U256::ZERO,
                })
                .collect()
        })
        .collect()
}

/// Computes the execution-only max gas cost.
pub fn max_execution_gas_cost(tx: &TxEip8130) -> U256 {
    U256::from(tx.gas_limit) * U256::from(tx.max_fee_per_gas)
}

/// Computes the gas refund for unused gas.
pub fn gas_refund(gas_limit: u64, gas_used: u64, effective_gas_price: u128) -> U256 {
    let unused = gas_limit.saturating_sub(gas_used);
    U256::from(unused) * U256::from(effective_gas_price)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_delegation_code_format() {
        let code = auto_delegation_code();
        assert_eq!(code.len(), 23);
        assert_eq!(&code[..3], &[0xef, 0x01, 0x00]);
        assert_eq!(&code[3..], DEFAULT_ACCOUNT_ADDRESS.as_slice());
    }

    #[test]
    fn max_execution_gas_cost_calculation() {
        let tx = TxEip8130 { gas_limit: 100_000, max_fee_per_gas: 10, ..Default::default() };
        assert_eq!(max_execution_gas_cost(&tx), U256::from(1_000_000));
    }

    #[test]
    fn gas_refund_calculation() {
        assert_eq!(gas_refund(100_000, 60_000, 10), U256::from(400_000));
    }

    #[test]
    fn gas_refund_no_unused() {
        assert_eq!(gas_refund(100_000, 100_000, 10), U256::ZERO);
    }

    #[test]
    fn build_calls_from_empty_tx() {
        let tx = TxEip8130::default();
        let calls = build_execution_calls(&tx, Address::repeat_byte(0x01));
        assert!(calls.is_empty());
    }

    #[test]
    fn nonce_increment_write_correct() {
        let sender = Address::repeat_byte(0x01);
        let nonce_key = U256::from(42);
        let write = nonce_increment_write(sender, nonce_key, 5);
        assert_eq!(write.address, NONCE_MANAGER_ADDRESS);
        assert_eq!(write.value, U256::from(5));
    }
}
