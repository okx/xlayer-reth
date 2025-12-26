use alloy_consensus::{transaction::TxHashRef, BlockHeader, Transaction, TxReceipt};
use alloy_json_rpc::RpcObject;
use alloy_primitives::Address;
use alloy_rpc_types_eth::{
    pubsub::{Params as AlloyParams, SubscriptionKind as AlloySubscriptionKind},
    Header, TransactionInfo,
};
use futures::StreamExt;
use jsonrpsee::{
    proc_macros::rpc, server::SubscriptionMessage, types::ErrorObject, PendingSubscriptionSink,
    SubscriptionSink,
};
use reth_chain_state::CanonStateSubscriptions;
use reth_optimism_flashblocks::{PendingBlockRx, PendingFlashBlock};
use reth_primitives_traits::{NodePrimitives, Recovered, SealedBlock, TransactionMeta};
use reth_rpc::eth::pubsub::EthPubSub;
use reth_rpc_convert::{transaction::ConvertReceiptInput, RpcConvert};
use reth_rpc_eth_api::pubsub::EthPubSubApiServer;
use reth_rpc_eth_api::{EthApiTypes, RpcNodeCore, RpcReceipt, RpcTransaction};
use reth_rpc_server_types::result::{internal_rpc_err, invalid_params_rpc_err};
use reth_storage_api::BlockNumReader;
use reth_transaction_pool::TransactionPool;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use tokio_stream::{wrappers::WatchStream, Stream};

const FLASHBLOCKS: &str = "flashblocks";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Subscription kind.
pub enum FlashblockSubscriptionKind {
    /// Standard Ethereum subscription.
    Standard(AlloySubscriptionKind),
    /// Flashblocks subscription.
    Flashblocks,
}

impl<'de> Deserialize<'de> for FlashblockSubscriptionKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;

        if let Some(s) = value.as_str() {
            if s == FLASHBLOCKS {
                return Ok(FlashblockSubscriptionKind::Flashblocks);
            }
        }

        match serde_json::from_value::<AlloySubscriptionKind>(value.clone()) {
            Ok(kind) => Ok(FlashblockSubscriptionKind::Standard(kind)),
            Err(_) => Err(serde::de::Error::custom("Invalid subscription kind")),
        }
    }
}

impl Serialize for FlashblockSubscriptionKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            FlashblockSubscriptionKind::Standard(kind) => kind.serialize(serializer),
            FlashblockSubscriptionKind::Flashblocks => serializer.serialize_str(FLASHBLOCKS),
        }
    }
}

impl FlashblockSubscriptionKind {
    /// Returns the inner standard subscription kind, if any.
    pub const fn as_standard(&self) -> Option<&AlloySubscriptionKind> {
        match self {
            Self::Standard(kind) => Some(kind),
            Self::Flashblocks => None,
        }
    }
}

impl From<AlloySubscriptionKind> for FlashblockSubscriptionKind {
    fn from(kind: AlloySubscriptionKind) -> Self {
        Self::Standard(kind)
    }
}

/// Extended params that wraps Alloy's `Params` and adds flashblocks specific variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpParams {
    /// Standard Ethereum subscription params
    Standard(AlloyParams),
    /// Flashblocks stream filter
    FlashBlocksFilter(FlashBlocksFilter),
}

impl<'de> Deserialize<'de> for OpParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;

        if let Ok(filter) = serde_json::from_value::<FlashBlocksFilter>(value.clone()) {
            return Ok(OpParams::FlashBlocksFilter(filter));
        }

        if let Ok(standard_params) = serde_json::from_value::<AlloyParams>(value.clone()) {
            return Ok(OpParams::Standard(standard_params));
        }

        Err(serde::de::Error::custom(
            "Invalid subscription parameters: must be valid FlashBlocksFilter or Filter",
        ))
    }
}

impl Serialize for OpParams {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            OpParams::Standard(params) => params.serialize(serializer),
            OpParams::FlashBlocksFilter(filter) => filter.serialize(serializer),
        }
    }
}

impl OpParams {
    /// Returns the inner `FlashBlocksFilter` if this is a `FlashBlocksFilter` variant.
    pub fn as_flashblocks_filter(&self) -> Option<&FlashBlocksFilter> {
        match self {
            OpParams::FlashBlocksFilter(filter) => Some(filter),
            _ => None,
        }
    }

    /// Returns the inner standard `Params` if this is a `Standard` variant.
    pub fn as_standard(&self) -> Option<&AlloyParams> {
        match self {
            OpParams::Standard(params) => Some(params),
            _ => None,
        }
    }
}

/// Criteria for filtering and enriching flashblock subscription data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct FlashBlocksFilter {
    /// Include new block headers in the stream.
    pub header_info: bool,

    /// SubTxFilter
    pub sub_tx_filter: SubTxFilter,
}

impl FlashBlocksFilter {
    /// Returns `true` if address filtering is enabled.
    pub fn requires_address_filtering(&self) -> bool {
        self.sub_tx_filter.has_address_filter()
    }
}

/// Criteria for filtering and enriching transaction subscription data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct SubTxFilter {
    /// Include extra transaction information
    pub tx_info: bool,

    /// Include transaction receipts.
    pub tx_receipt: bool,

    /// Only include transactions involving these addresses
    pub subscribe_addresses: Vec<Address>,
}

impl SubTxFilter {
    /// Returns `true` if address filtering is enabled.
    pub fn has_address_filter(&self) -> bool {
        !self.subscribe_addresses.is_empty()
    }
}

/// Flashblock data returned to subscribers based on `FlashBlocksFilter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichedFlashblock<H, Tx, R> {
    /// Block header (if `header_info` is true in filter criteria).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<Header<H>>,

    /// Filtered transactions with optional enrichment.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub transactions: Vec<EnrichedTransaction<Tx, R>>,
}

/// Transaction data with optional enrichment based on `FlashBlocksFilter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichedTransaction<Tx, R> {
    /// Transaction hash.
    pub tx_hash: alloy_primitives::TxHash,

    /// Transaction data (if `tx_info` is true in filter criteria).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_data: Option<Tx>,

    /// Transaction receipt (if `tx_receipt` is true in filter criteria).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<R>,
}

/// Flashblocks pubsub RPC interface.
#[rpc(server, namespace = "eth")]
pub trait FlashblocksPubSubApi<T: RpcObject> {
    /// Create an ethereum subscription for the given params
    #[subscription(
        name = "subscribe" => "subscription",
        unsubscribe = "unsubscribe",
        item = alloy_rpc_types::pubsub::SubscriptionResult
    )]
    async fn subscribe(
        &self,
        kind: FlashblockSubscriptionKind,
        params: Option<OpParams>,
    ) -> jsonrpsee::core::SubscriptionResult;
}

/// Optimism-specific Ethereum pubsub handler that extends standard subscriptions with flashblocks support.
pub struct FlashblocksPubSub<Eth: EthApiTypes, N: NodePrimitives> {
    /// Standard eth pubsub handler
    pub(crate) eth_pubsub: EthPubSub<Eth>,
    /// Pending block receiver from flashblocks, if available
    pub(crate) pending_block_rx: PendingBlockRx<N>,
    /// RPC transaction converter
    pub(crate) tx_converter: Eth::RpcConvert,
}

impl<Eth: EthApiTypes, N: NodePrimitives> Clone for FlashblocksPubSub<Eth, N>
where
    Eth: Clone,
    Eth::RpcConvert: Clone,
{
    fn clone(&self) -> Self {
        Self {
            eth_pubsub: self.eth_pubsub.clone(),
            pending_block_rx: self.pending_block_rx.clone(),
            tx_converter: self.tx_converter.clone(),
        }
    }
}

impl<Eth: EthApiTypes, N: NodePrimitives> FlashblocksPubSub<Eth, N> {
    /// Creates a new `FlashblocksPubSub` instance.
    pub fn new(
        eth_pubsub: EthPubSub<Eth>,
        pending_block_rx: PendingBlockRx<N>,
        tx_converter: Eth::RpcConvert,
    ) -> Self {
        Self { eth_pubsub, pending_block_rx, tx_converter }
    }

    /// Converts this `FlashblocksPubSub` into an RPC module.
    pub fn into_rpc(self) -> jsonrpsee::RpcModule<()>
    where
        FlashblocksPubSub<Eth, N>: FlashblocksPubSubApiServer<RpcTransaction<Eth::NetworkTypes>>,
    {
        <FlashblocksPubSub<Eth, N> as FlashblocksPubSubApiServer<
            RpcTransaction<Eth::NetworkTypes>,
        >>::into_rpc(self)
        .remove_context()
    }
}
