//! XLayerAA (EIP-8130) RPC extensions for the `eth` namespace.
//!
//! Extends `eth_getTransactionCount` with an optional `nonce_key`
//! parameter so callers can query the 2D nonce channel maintained by
//! the `NonceManager` predeploy.
//!
//! AA receipt enrichment (`payer`, `phaseStatuses`) is handled directly
//! inside the upstream `OpReceiptConverter` (see
//! `op-reth/crates/rpc/src/eth/receipt.rs`) — no downstream wiring
//! needed.

pub mod pool;
pub mod verifiers;

use alloy_eips::BlockId;
use alloy_primitives::{Address, U256};
use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    types::{error::INTERNAL_ERROR_CODE, ErrorObjectOwned},
};
use op_alloy_network::Optimism;
use op_revm::precompiles_xlayer::{aa_nonce_slot, NONCE_MANAGER_ADDRESS};
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_optimism_txpool::Eip8130PoolView;
use reth_rpc_eth_api::{
    helpers::{FullEthApi, SpawnBlocking},
    EthApiServer, EthApiTypes, RpcConvert, RpcNodeCore,
};
use reth_rpc_eth_types::EthApiError;
use reth_storage_api::StateProviderFactory;

use crate::aa::pool::{layer_aa_pool_head, read_aa_slot};

/// Reads the 2D nonce for `(address, nonce_key)` from the Nonce
/// Manager predeploy's storage at the requested block.
///
/// The slot computation (see [`aa_nonce_slot`]) mirrors the handler's
/// own read path in `op_revm::handler` — if this drifts, the pool
/// and handler disagree on which nonce the tx pins against, and the
/// user gets a confusing "known nonce" reject at submission. Tests
/// pin the exact slot formula.
pub fn read_2d_nonce<P: StateProviderFactory>(
    provider: &P,
    address: Address,
    block_id: BlockId,
    nonce_key: U256,
) -> RpcResult<U256> {
    let slot = aa_nonce_slot(address, nonce_key);
    let state = provider
        .state_by_block_id(block_id)
        .map_err(|e| rpc_internal_error(format!("state access error: {e}")))?;

    let value = state
        .storage(NONCE_MANAGER_ADDRESS, slot.into())
        .map_err(|e| rpc_internal_error(format!("storage read error: {e}")))?
        .unwrap_or_default();

    Ok(value)
}

/// Overrides `eth_getTransactionCount` with an optional `nonce_key`
/// parameter for 2D nonce channel queries.
#[rpc(server, namespace = "eth")]
pub trait TransactionCountOverride {
    /// Returns the transaction count (nonce) for an address.
    ///
    /// When `nonce_key` is provided, reads the 2D nonce from the
    /// Nonce Manager predeploy at [`NONCE_MANAGER_ADDRESS`]. When
    /// omitted, returns the standard account nonce.
    #[method(name = "getTransactionCount")]
    async fn get_transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
        nonce_key: Option<U256>,
    ) -> RpcResult<U256>;
}

/// Server implementation of [`TransactionCountOverride`].
#[derive(Debug, Clone)]
pub struct TransactionCountOverrideImpl<N: RpcNodeCore, Rpc: RpcConvert> {
    eth_api: OpEthApi<N, Rpc>,
}

impl<N: RpcNodeCore, Rpc: RpcConvert> TransactionCountOverrideImpl<N, Rpc> {
    pub const fn new(eth_api: OpEthApi<N, Rpc>) -> Self {
        Self { eth_api }
    }
}

#[async_trait::async_trait]
impl<N, Rpc> TransactionCountOverrideServer for TransactionCountOverrideImpl<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: FullEthApi<NetworkTypes = Optimism, Error = OpEthApiError>
        + EthApiTypes<RpcConvert = Rpc>
        + RpcNodeCore
        + SpawnBlocking
        + Send
        + Sync
        + 'static,
    <OpEthApi<N, Rpc> as RpcNodeCore>::Pool: Eip8130PoolView,
{
    async fn get_transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
        nonce_key: Option<U256>,
    ) -> RpcResult<U256> {
        match nonce_key {
            // No `nonce_key` → defer to the upstream handler so the
            // `pending` block tag still surfaces txpool-resident nonces
            // (returns max(state nonce, highest_consecutive_pool_nonce + 1)).
            None => EthApiServer::transaction_count(&self.eth_api, address, block_number).await,
            // 2D nonce read against NonceManager predeploy storage. For the
            // `pending` tag we additionally layer the AA pool's per-lane
            // consecutive head, mirroring how the 1D path layers
            // `get_highest_consecutive_transaction_by_sender`.
            Some(key) => {
                let block_id = block_number.unwrap_or_default();
                self.eth_api
                    .spawn_blocking_io_fut(move |this| async move {
                        let state = this
                            .provider()
                            .state_by_block_id(block_id)
                            .map_err(EthApiError::from)?;
                        let slot_value = read_aa_slot(state.as_ref(), address, key)?;
                        Ok(layer_aa_pool_head(this.pool(), address, key, slot_value, block_number))
                    })
                    .await
                    .map_err(|e| -> jsonrpsee_types::error::ErrorObject<'static> { e.into() })
            }
        }
    }
}

fn rpc_internal_error(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(INTERNAL_ERROR_CODE, msg, None::<()>)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address, U256};
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};

    /// Pins the 2D-nonce slot formula against a known vector so a
    /// silent refactor of `aa_nonce_slot` in op_revm breaks here
    /// rather than being caught by a rare integration test.
    #[test]
    fn nonce_slot_matches_known_vector() {
        let owner = address!("0x1111111111111111111111111111111111111111");
        let key = U256::from(42u64);
        let slot = aa_nonce_slot(owner, key);
        use alloy_primitives::keccak256;
        use alloy_sol_types::SolValue;
        let inner = keccak256((owner, op_revm::precompiles_xlayer::NONCE_BASE_SLOT).abi_encode());
        let outer = keccak256((key, inner).abi_encode());
        assert_eq!(slot, U256::from_be_bytes(outer.0));
    }

    #[test]
    fn read_2d_nonce_reads_nonce_manager_storage() {
        let provider = MockEthProvider::default();
        let owner = address!("0x1111111111111111111111111111111111111111");
        let nonce_key = U256::from(42u64);
        let slot = aa_nonce_slot(owner, nonce_key);
        provider.add_account(
            NONCE_MANAGER_ADDRESS,
            ExtendedAccount::new(0, U256::ZERO).extend_storage([(slot.into(), U256::from(99u64))]),
        );

        let nonce = read_2d_nonce(&provider, owner, BlockId::default(), nonce_key).unwrap();
        assert_eq!(nonce, U256::from(99u64));
    }

    #[test]
    fn read_2d_nonce_returns_zero_for_unset_slot() {
        let provider = MockEthProvider::default();
        let owner = Address::repeat_byte(0xab);
        let nonce_key = U256::from(7u64);

        let nonce = read_2d_nonce(&provider, owner, BlockId::default(), nonce_key).unwrap();
        assert_eq!(nonce, U256::ZERO);
    }

    #[test]
    fn read_2d_nonce_isolates_keys() {
        let provider = MockEthProvider::default();
        let owner = Address::repeat_byte(0xcd);
        let key_a = U256::from(1u64);
        let key_b = U256::from(2u64);
        provider.add_account(
            NONCE_MANAGER_ADDRESS,
            ExtendedAccount::new(0, U256::ZERO).extend_storage([
                (aa_nonce_slot(owner, key_a).into(), U256::from(11u64)),
                (aa_nonce_slot(owner, key_b).into(), U256::from(22u64)),
            ]),
        );

        let a = read_2d_nonce(&provider, owner, BlockId::default(), key_a).unwrap();
        let b = read_2d_nonce(&provider, owner, BlockId::default(), key_b).unwrap();
        assert_eq!(a, U256::from(11u64));
        assert_eq!(b, U256::from(22u64));
    }
}
