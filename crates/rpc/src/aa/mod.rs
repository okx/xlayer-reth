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
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use op_alloy_network::Optimism;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_optimism_txpool::Eip8130PoolView;
use reth_rpc_eth_api::{
    helpers::{FullEthApi, SpawnBlocking},
    EthApiServer, EthApiTypes, RpcConvert, RpcNodeCore,
};
use reth_rpc_eth_types::EthApiError;
use reth_storage_api::StateProviderFactory;

use crate::aa::pool::{layer_aa_pool_head, read_aa_slot};

/// Lane-pending overlay used by the AA `eth_getTransactionCount`
/// overrides.
///
/// Encapsulates the pool lookup so the RPC handlers don't need to
/// reach for `RpcNodeCore::pool()` themselves — concrete eth APIs
/// (`OpEthApi`, `FlashblocksEthApiExt`) get this trait via the blanket
/// impl, and the call sites stay free of `Eip8130PoolView` bounds.
///
/// The slot read against `NonceManager` is deliberately kept outside
/// the trait so callers can run it on the blocking IO executor and
/// then hand the resulting `slot_value` to this synchronous overlay.
pub trait AaNonceOverlay {
    /// Returns `slot_value` for non-pending block tags. For `pending`,
    /// returns `max(slot_value, highest_consecutive_pending_seq + 1)`
    /// so a wallet polling for its next AA sequence inside the
    /// admission-but-not-yet-included window sees the live answer
    /// instead of a stale state read.
    fn aa_nonce_with_pending_overlay(
        &self,
        address: Address,
        nonce_key: U256,
        slot_value: U256,
        block_number: Option<BlockId>,
    ) -> U256;
}

impl<T> AaNonceOverlay for T
where
    T: RpcNodeCore,
    T::Pool: Eip8130PoolView,
{
    fn aa_nonce_with_pending_overlay(
        &self,
        address: Address,
        nonce_key: U256,
        slot_value: U256,
        block_number: Option<BlockId>,
    ) -> U256 {
        layer_aa_pool_head(self.pool(), address, nonce_key, slot_value, block_number)
    }
}

/// Overrides `eth_getTransactionCount` with an optional `nonce_key`
/// parameter for 2D nonce channel queries.
#[rpc(server, namespace = "eth")]
pub trait TransactionCountOverride {
    /// Returns the transaction count (nonce) for an address.
    ///
    /// When `nonce_key` is provided, reads the 2D nonce from the
    /// Nonce Manager predeploy. When omitted, returns the standard
    /// account nonce.
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
        + AaNonceOverlay
        + Send
        + Sync
        + 'static,
{
    async fn get_transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
        nonce_key: Option<U256>,
    ) -> RpcResult<U256> {
        match nonce_key {
            // No `nonce_key` → defer to the upstream handler so the
            // `pending` tag still surfaces the standard txpool-resident
            // nonce (max(state nonce, highest_consecutive_pool_nonce + 1)).
            None => EthApiServer::transaction_count(&self.eth_api, address, block_number).await,
            // 2D nonce read against NonceManager predeploy storage.
            // The slot read runs on the blocking IO executor (matches
            // the upstream `LoadState::transaction_count` path); the
            // pending-tag overlay is then applied by `AaNonceOverlay`.
            Some(key) => {
                let block_id = block_number.unwrap_or_default();
                self.eth_api
                    .spawn_blocking_io_fut(move |this| async move {
                        let state = this
                            .provider()
                            .state_by_block_id(block_id)
                            .map_err(EthApiError::from)?;
                        let slot_value = read_aa_slot(state.as_ref(), address, key)?;
                        Ok(this.aa_nonce_with_pending_overlay(
                            address,
                            key,
                            slot_value,
                            block_number,
                        ))
                    })
                    .await
                    .map_err(|e| -> jsonrpsee_types::error::ErrorObject<'static> { e.into() })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address, U256};
    use op_revm::precompiles_xlayer::{aa_nonce_slot, NONCE_MANAGER_ADDRESS};
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

    /// `read_aa_slot` is the live read path used by the RPC override
    /// (it runs inside `spawn_blocking_io_fut`); pin it against a
    /// provider-backed state read so a refactor of the slot lookup
    /// surfaces here rather than as an RPC-level mismatch.
    fn read_via_provider(provider: &MockEthProvider, owner: Address, nonce_key: U256) -> U256 {
        let state = provider.state_by_block_id(BlockId::default()).unwrap();
        read_aa_slot(state.as_ref(), owner, nonce_key).unwrap()
    }

    #[test]
    fn read_aa_slot_reads_nonce_manager_storage() {
        let provider = MockEthProvider::default();
        let owner = address!("0x1111111111111111111111111111111111111111");
        let nonce_key = U256::from(42u64);
        let slot = aa_nonce_slot(owner, nonce_key);
        provider.add_account(
            NONCE_MANAGER_ADDRESS,
            ExtendedAccount::new(0, U256::ZERO).extend_storage([(slot.into(), U256::from(99u64))]),
        );

        assert_eq!(read_via_provider(&provider, owner, nonce_key), U256::from(99u64));
    }

    #[test]
    fn read_aa_slot_returns_zero_for_unset_slot() {
        let provider = MockEthProvider::default();
        let owner = Address::repeat_byte(0xab);
        let nonce_key = U256::from(7u64);
        assert_eq!(read_via_provider(&provider, owner, nonce_key), U256::ZERO);
    }

    #[test]
    fn read_aa_slot_isolates_keys() {
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

        assert_eq!(read_via_provider(&provider, owner, key_a), U256::from(11u64));
        assert_eq!(read_via_provider(&provider, owner, key_b), U256::from(22u64));
    }
}
