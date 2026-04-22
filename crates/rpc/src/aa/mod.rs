//! XLayerAA (EIP-8130) RPC extensions for the `eth` namespace.
//!
//! Two building blocks live here:
//!
//! 1. [`TransactionCountOverride`] — extends `eth_getTransactionCount`
//!    with an optional `nonce_key` parameter so callers can query
//!    the 2D nonce channel maintained by the `NonceManager`
//!    predeploy. Without this, AA clients have no cheap way to
//!    discover the next nonce for a `(sender, nonce_key)` pair.
//!
//! 2. [`receipt::build_eip8130_fields`] — pure helper that derives
//!    [`xlayer_rpc_types::Eip8130ReceiptFields`] from an AA tx
//!    envelope + receipt logs. The receipt-converter wire-up that
//!    actually threads these fields into `eth_getTransactionReceipt`
//!    JSON lands alongside the M5 node-level `XLayerNode` swap;
//!    until then the helper stays callable as a standalone so
//!    future converter work and AA-aware test harnesses share one
//!    derivation path.
//!
//! Both mirror Base's shape exactly so existing AA tooling works
//! on XLayer unchanged.

pub mod receipt;

use alloy_eips::BlockId;
use alloy_primitives::{Address, U256};
use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    types::{error::INTERNAL_ERROR_CODE, ErrorObjectOwned},
};
use reth_storage_api::StateProviderFactory;
use xlayer_revm::{aa_nonce_slot, NONCE_MANAGER_ADDRESS};

/// Reads the 2D nonce for `(address, nonce_key)` from the Nonce
/// Manager predeploy's storage at the requested block.
///
/// The slot computation (see [`aa_nonce_slot`]) mirrors the handler's
/// own read path in `xlayer-revm/src/handler.rs` — if this drifts,
/// the pool and handler disagree on which nonce the tx pins against,
/// and the user gets a confusing "known nonce" reject at submission.
/// Tests pin the exact slot formula.
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
    use alloy_primitives::{address, U256};

    // Reth's MockEthProvider lives behind the `test-utils` feature,
    // which isn't reachable from our default dev-deps set — the
    // tests here exercise the pure-logic slot helper directly.
    // End-to-end provider tests land alongside the bin/node
    // integration wiring (P3d).

    /// Pins the 2D-nonce slot formula against a known vector so a
    /// silent refactor of `aa_nonce_slot` in xlayer-revm breaks here
    /// rather than being caught by a rare integration test.
    #[test]
    fn nonce_slot_matches_known_vector() {
        let owner = address!("0x1111111111111111111111111111111111111111");
        let key = U256::from(42u64);
        let slot = aa_nonce_slot(owner, key);
        // Recompute inline using the documented formula to guard
        // against the helper drifting.
        use alloy_primitives::keccak256;
        use alloy_sol_types::SolValue;
        let inner = keccak256((owner, xlayer_revm::NONCE_BASE_SLOT).abi_encode());
        let outer = keccak256((key, inner).abi_encode());
        assert_eq!(slot, U256::from_be_bytes(outer.0));
    }
}
