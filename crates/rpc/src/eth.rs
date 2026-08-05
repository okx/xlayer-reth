//! Flashblocks-aware `eth_getTransactionCount` override.
//!
//! Adds a `"flashblocks"` pseudo-tag to `eth_getTransactionCount` that returns
//! the account nonce as executed by the most recent flashblock, WITHOUT the
//! mempool overlay that the standard `"pending"` tag applies. Every other block
//! parameter is delegated unchanged to the upstream `EthApiServer` default.

use alloy_consensus::BlockHeader;
use alloy_eips::BlockId;
use alloy_primitives::{Address, U256};
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
};
use op_alloy_network::Optimism;
use op_revm::transaction::OpTxTr;
use reth_chain_state::BlockState;
use reth_evm::TxEnvFor;
use reth_optimism_evm::OpTxEnv;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_rpc_eth_api::{EthApiServer, FromEvmError, RpcConvert, RpcNodeCore};
use reth_rpc_server_types::result::{internal_rpc_err, ToRpcResult};
use reth_storage_api::{StateProvider, StateProviderFactory};
use tracing::warn;

/// Block tag accepted by flashblocks-aware RPC methods: either a standard
/// [`BlockId`], or the flashblocks-only pseudo-tag that returns the nonce/state
/// as executed by flashblocks so far, without overlaying mempool-pending txs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashblocksBlockId {
    /// Any standard block id/tag (`latest`, `pending`, number, hash, ...).
    Standard(BlockId),
    /// Executed-by-flashblocks state, no mempool overlay.
    Flashblocks,
}

/// The literal block-parameter string that selects the flashblocks-executed state.
const FLASHBLOCKS_TAG: &str = "flashblocks";

impl<'de> serde::Deserialize<'de> for FlashblocksBlockId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Peek the raw JSON value: the exact string `"flashblocks"` selects the
        // flashblocks pseudo-tag; anything else defers to the standard `BlockId`
        // deserializer (which rejects unknown strings as invalid params).
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.as_str() == Some(FLASHBLOCKS_TAG) {
            return Ok(Self::Flashblocks);
        }
        BlockId::deserialize(value).map(Self::Standard).map_err(serde::de::Error::custom)
    }
}

/// Self-declared override of `eth_getTransactionCount` that understands the
/// `"flashblocks"` block tag. Registered via `add_or_replace_if_module_configured`
/// so it replaces the default dispatch entry for the method.
#[rpc(server, namespace = "eth")]
pub trait FlashblocksEthApiOverride {
    /// Returns the number of transactions sent from `address` at the given block
    /// parameter, honouring the flashblocks-only `"flashblocks"` tag.
    #[method(name = "getTransactionCount")]
    async fn transaction_count(
        &self,
        address: Address,
        block_number: Option<FlashblocksBlockId>,
    ) -> RpcResult<U256>;
}

/// Flashblocks-aware `eth_getTransactionCount` implementation, wrapping the
/// concrete [`OpEthApi`] so it can both read flashblock state and delegate to the
/// upstream default for standard tags.
pub struct FlashblocksEthApiExt<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
{
    eth_api: OpEthApi<N, Rpc>,
}

impl<N, Rpc> FlashblocksEthApiExt<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
{
    /// Creates a new override wrapping the given [`OpEthApi`].
    pub fn new(eth_api: OpEthApi<N, Rpc>) -> Self {
        Self { eth_api }
    }
}

#[async_trait]
impl<N, Rpc> FlashblocksEthApiOverrideServer for FlashblocksEthApiExt<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert<
        Network = Optimism,
        Primitives = N::Primitives,
        Error = OpEthApiError,
        Evm = N::Evm,
    >,
    OpEthApiError: FromEvmError<N::Evm>,
    TxEnvFor<N::Evm>: OpTxTr + OpTxEnv,
    OpEthApi<N, Rpc>: RpcNodeCore<Primitives = OpPrimitives> + Send + Sync + 'static,
{
    async fn transaction_count(
        &self,
        address: Address,
        block_number: Option<FlashblocksBlockId>,
    ) -> RpcResult<U256> {
        match block_number {
            Some(FlashblocksBlockId::Flashblocks) => {
                // `pending_flashblock()` returns an `eyre::Result`, which does not
                // implement `ToRpcResult`; map it to an internal RPC error.
                let pending = self
                    .eth_api
                    .pending_flashblock()
                    .await
                    .map_err(|err| internal_rpc_err(err.to_string()))?;
                match pending {
                    // INV-1: return the flashblocks-executed nonce before any pool overlay.
                    Some(pending) => {
                        let latest_historical = self
                            .eth_api
                            .provider()
                            .history_by_block_hash(pending.block().parent_hash())
                            .to_rpc_result()?;
                        let state =
                            BlockState::from(pending.pending).state_provider(latest_historical);
                        let nonce =
                            state.account_nonce(&address).to_rpc_result()?.unwrap_or_default();
                        Ok(U256::from(nonce))
                    }
                    // INV-3: fall back to latest (never pending), with exactly one warn log.
                    None => {
                        warn!(
                            target: "rpc::eth",
                            ?address,
                            "flashblocks tag requested but no pending flashblock available, falling back to latest"
                        );
                        EthApiServer::transaction_count(
                            &self.eth_api,
                            address,
                            Some(BlockId::latest()),
                        )
                        .await
                    }
                }
            }
            // INV-2: delegate every standard block parameter unchanged.
            Some(FlashblocksBlockId::Standard(id)) => {
                EthApiServer::transaction_count(&self.eth_api, address, Some(id)).await
            }
            None => EthApiServer::transaction_count(&self.eth_api, address, None).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FlashblocksBlockId, FLASHBLOCKS_TAG};
    use alloy_eips::BlockId;
    use serde_json::json;

    #[test]
    fn deserializes_flashblocks_tag_to_flashblocks_variant() {
        let parsed: FlashblocksBlockId = serde_json::from_value(json!(FLASHBLOCKS_TAG)).unwrap();
        assert_eq!(parsed, FlashblocksBlockId::Flashblocks);
    }

    #[test]
    fn deserializes_standard_string_tags_to_standard_variant() {
        for tag in ["latest", "earliest", "pending", "safe", "finalized"] {
            let parsed: FlashblocksBlockId = serde_json::from_value(json!(tag)).unwrap();
            assert!(
                matches!(parsed, FlashblocksBlockId::Standard(_)),
                "tag {tag} should deserialize to Standard"
            );
        }
    }

    #[test]
    fn deserializes_hex_block_number_to_standard_variant() {
        let parsed: FlashblocksBlockId = serde_json::from_value(json!("0x1a")).unwrap();
        assert_eq!(parsed, FlashblocksBlockId::Standard(BlockId::from(0x1au64)));
    }

    #[test]
    fn deserializes_block_hash_object_to_standard_variant() {
        let hash = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let parsed: FlashblocksBlockId =
            serde_json::from_value(json!({ "blockHash": hash })).unwrap();
        assert!(matches!(parsed, FlashblocksBlockId::Standard(_)));
    }

    #[test]
    fn rejects_unsupported_and_wrong_case_strings() {
        // Unsupported string, wrong-case tag, and a bare (non-hex-string) number
        // must all fail deserialization -> JSON-RPC "invalid params".
        assert!(serde_json::from_value::<FlashblocksBlockId>(json!("foobar")).is_err());
        assert!(serde_json::from_value::<FlashblocksBlockId>(json!("Flashblocks")).is_err());
        assert!(serde_json::from_value::<FlashblocksBlockId>(json!(123)).is_err());
    }
}
