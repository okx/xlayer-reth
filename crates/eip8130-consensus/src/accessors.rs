//! Storage accessors for EIP-8130 system contract state.
//!
//! Provides typed read helpers that compute the correct storage slot and
//! decode the raw `U256` value into the expected Rust type. All functions
//! are generic over `revm::Database`.

use alloy_primitives::{Address, B256, U256};
use revm::database::Database;

use super::{
    predeploys::{ACCOUNT_CONFIG_ADDRESS, NONCE_MANAGER_ADDRESS},
    storage::{
        account_state_slot, encode_owner_config, nonce_slot, owner_config_slot,
        parse_account_state, parse_owner_config, read_sequence,
    },
};

/// Lock state extracted from the `AccountState` storage slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LockState {
    /// Timestamp at which the account becomes unlocked.
    ///
    /// `0` = never locked (default), `type(uint40).max` = permanently locked,
    /// other value = unlock pending at that timestamp.
    pub unlocks_at: u64,
    /// Minimum delay (in seconds) before an unlock request takes effect.
    pub unlock_delay: u16,
}

/// Reads the `owner_config` for `(account, owner_id)` from the AccountConfig contract.
///
/// Returns `(verifier, scope)`. A zero verifier means the owner is not registered.
pub fn read_owner_config<DB: Database>(
    db: &mut DB,
    account: Address,
    owner_id: B256,
) -> Result<(Address, u8), DB::Error> {
    let slot = owner_config_slot(account, owner_id);
    let value = db.storage(ACCOUNT_CONFIG_ADDRESS, slot.into())?;
    Ok(parse_owner_config(B256::from(value.to_be_bytes::<32>())))
}

/// Returns `true` if the owner is authorized (verifier != address(0)).
pub fn is_owner_authorized<DB: Database>(
    db: &mut DB,
    account: Address,
    owner_id: B256,
) -> Result<bool, DB::Error> {
    let (verifier, _) = read_owner_config(db, account, owner_id)?;
    Ok(verifier != Address::ZERO)
}

/// Builds the storage write for registering/updating an owner.
///
/// Returns `(contract_address, slot, value)` for a single SSTORE.
pub fn write_owner_config_op(
    account: Address,
    owner_id: B256,
    verifier: Address,
    scope: u8,
) -> (Address, U256, U256) {
    let slot = owner_config_slot(account, owner_id);
    let value = encode_owner_config(verifier, scope);
    (ACCOUNT_CONFIG_ADDRESS, slot.into(), value.into())
}

/// Reads the current nonce sequence for `(account, nonce_key)` from the NonceManager.
pub fn read_nonce<DB: Database>(
    db: &mut DB,
    account: Address,
    nonce_key: U256,
) -> Result<u64, DB::Error> {
    let slot = nonce_slot(account, nonce_key);
    let value = db.storage(NONCE_MANAGER_ADDRESS, slot.into())?;
    Ok(value.to::<u64>())
}

/// Builds the storage write for incrementing a nonce.
///
/// Returns `(contract_address, slot, new_value)`.
pub fn increment_nonce_op(
    account: Address,
    nonce_key: U256,
    current: u64,
) -> (Address, U256, U256) {
    let slot = nonce_slot(account, nonce_key);
    let next = current.checked_add(1).expect("nonce overflow at u64::MAX");
    (NONCE_MANAGER_ADDRESS, slot.into(), U256::from(next))
}

/// Reads the lock state for an account from the AccountConfig contract.
pub fn read_lock_state<DB: Database>(
    db: &mut DB,
    account: Address,
) -> Result<LockState, DB::Error> {
    let slot = account_state_slot(account);
    let value = db.storage(ACCOUNT_CONFIG_ADDRESS, slot.into())?;
    let state = parse_account_state(value);
    Ok(LockState { unlocks_at: state.unlocks_at, unlock_delay: state.unlock_delay })
}

/// Reads the change sequence for `(account, chain_id)` from AccountConfig.
///
/// `chain_id == 0` reads `multichain`, otherwise `local`.
pub fn read_change_sequence<DB: Database>(
    db: &mut DB,
    account: Address,
    chain_id: u64,
) -> Result<u64, DB::Error> {
    let slot = account_state_slot(account);
    let packed = db.storage(ACCOUNT_CONFIG_ADDRESS, slot.into())?;
    Ok(read_sequence(packed, chain_id == 0))
}
