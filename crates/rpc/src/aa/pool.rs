//! Shared AA-pool / NonceManager helpers used by the `eth_getTransactionCount`
//! overrides.

use alloy_eips::BlockId;
use alloy_primitives::{Address, U256};
use op_revm::precompiles_xlayer::{aa_nonce_slot, NONCE_MANAGER_ADDRESS};
use reth_optimism_txpool::{Eip8130PoolView, Eip8130SeqId};
use reth_rpc_eth_types::EthApiError;
use reth_storage_api::StateProvider;

/// Reads `aa_nonce_slot(address, nonce_key)` from `NONCE_MANAGER_ADDRESS`.
pub fn read_aa_slot<S: StateProvider + ?Sized>(
    state: &S,
    address: Address,
    nonce_key: U256,
) -> Result<U256, EthApiError> {
    let slot = aa_nonce_slot(address, nonce_key);
    Ok(state
        .storage(NONCE_MANAGER_ADDRESS, slot.into())
        .map_err(EthApiError::from)?
        .unwrap_or_default())
}

/// Returns `max(slot_value, highest_consecutive_pool_seq + 1)` for
/// `pending`, and the slot value unchanged for any other tag.
pub fn layer_aa_pool_head<P>(
    pool: &P,
    address: Address,
    nonce_key: U256,
    slot_value: U256,
    block_number: Option<BlockId>,
) -> U256
where
    P: Eip8130PoolView,
{
    if block_number != Some(BlockId::pending()) {
        return slot_value;
    }
    let baseline: u64 = slot_value.try_into().unwrap_or(u64::MAX);
    let seq = Eip8130SeqId::new(address, nonce_key);
    let Some(highest) = pool.highest_consecutive_aa_pending_seq_in_lane(seq, baseline) else {
        return slot_value;
    };
    let next = highest.saturating_add(1);
    slot_value.max(U256::from(next))
}
