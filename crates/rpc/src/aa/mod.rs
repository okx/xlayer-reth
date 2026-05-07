//! XLayerAA (EIP-8130) RPC extensions for the `eth` namespace.
//!
//! Extends `eth_getTransactionCount` with an optional `nonce_key`
//! parameter so callers can query the 2D nonce channel maintained by
//! the `NonceManager` predeploy. Without this, AA clients have no
//! cheap way to discover the next nonce for a `(sender, nonce_key)`
//! pair.
//!
//! AA receipt enrichment (`payer`, `phaseStatuses`) is handled directly
//! inside the upstream `OpReceiptConverter` (see
//! `op-reth/crates/rpc/src/eth/receipt.rs`) — no downstream wiring
//! needed.

pub mod verifiers;

use alloy_eips::BlockId;
use alloy_primitives::{Address, U256};
use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    types::{error::INTERNAL_ERROR_CODE, ErrorObjectOwned},
};
use op_revm::precompiles_xlayer::{aa_nonce_slot, NONCE_MANAGER_ADDRESS};
use reth_storage_api::StateProviderFactory;

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
///
/// **Naming.** We intentionally keep the method name
/// `eth_getTransactionCount` (the exact standard one) rather than
/// introducing a new namespace. Reth's JSON-RPC registry routes
/// duplicate method names to the last-registered handler — so
/// attaching this override after the stock eth module means a
/// client calling `eth_getTransactionCount(addr, block)` continues
/// to work unchanged, while a client that passes a third
/// `nonce_key` argument transparently opts into the AA path.
///
/// Matches Base's surface: `nonce_key` is a camelCased U256 at
/// position 2 (after address and block).
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
pub struct TransactionCountOverrideImpl<Provider> {
    provider: Provider,
}

impl<Provider> TransactionCountOverrideImpl<Provider> {
    /// Creates a new override wrapping the given state provider.
    pub const fn new(provider: Provider) -> Self {
        Self { provider }
    }
}

#[async_trait::async_trait]
impl<Provider> TransactionCountOverrideServer for TransactionCountOverrideImpl<Provider>
where
    Provider: StateProviderFactory + Send + Sync + 'static,
{
    async fn get_transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
        nonce_key: Option<U256>,
    ) -> RpcResult<U256> {
        let block_id = block_number.unwrap_or_default();
        match nonce_key {
            Some(key) => read_2d_nonce(&self.provider, address, block_id, key),
            None => {
                // Standard account-nonce path; delegates to the
                // same state provider the stock eth module uses.
                // Keeping this path here (rather than forwarding to
                // the inner eth handler) means the override is
                // self-contained and doesn't need an inner Arc of
                // the eth module for the degenerate case.
                let state = self
                    .provider
                    .state_by_block_id(block_id)
                    .map_err(|e| rpc_internal_error(format!("state access error: {e}")))?;
                let nonce = state
                    .basic_account(&address)
                    .map_err(|e| rpc_internal_error(format!("account read error: {e}")))?
                    .map(|a| a.nonce)
                    .unwrap_or_default();
                Ok(U256::from(nonce))
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
            ExtendedAccount::new(0, U256::ZERO)
                .extend_storage([(slot.into(), U256::from(99u64))]),
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

    #[tokio::test]
    async fn get_transaction_count_without_nonce_key_uses_account_nonce() {
        let provider = MockEthProvider::default();
        let address = Address::repeat_byte(0x22);
        provider.add_account(address, ExtendedAccount::new(7, U256::ZERO));

        let api = TransactionCountOverrideImpl::new(provider);
        let nonce = TransactionCountOverrideServer::get_transaction_count(&api, address, None, None)
            .await
            .unwrap();
        assert_eq!(nonce, U256::from(7u64));
    }

    #[tokio::test]
    async fn get_transaction_count_unknown_address_returns_zero() {
        let provider = MockEthProvider::default();
        let api = TransactionCountOverrideImpl::new(provider);
        let nonce = TransactionCountOverrideServer::get_transaction_count(
            &api,
            Address::repeat_byte(0x99),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(nonce, U256::ZERO);
    }

    #[tokio::test]
    async fn get_transaction_count_with_nonce_key_uses_2d_nonce() {
        let provider = MockEthProvider::default();
        let address = Address::repeat_byte(0x33);
        let nonce_key = U256::from(5u64);
        let slot = aa_nonce_slot(address, nonce_key);
        // Account-nonce field intentionally non-zero — the AA path must
        // ignore it and read NonceManager storage instead.
        provider.add_account(address, ExtendedAccount::new(1, U256::ZERO));
        provider.add_account(
            NONCE_MANAGER_ADDRESS,
            ExtendedAccount::new(0, U256::ZERO)
                .extend_storage([(slot.into(), U256::from(21u64))]),
        );

        let api = TransactionCountOverrideImpl::new(provider);
        let nonce = TransactionCountOverrideServer::get_transaction_count(
            &api,
            address,
            Some(BlockId::default()),
            Some(nonce_key),
        )
        .await
        .unwrap();
        assert_eq!(nonce, U256::from(21u64));
    }
}
