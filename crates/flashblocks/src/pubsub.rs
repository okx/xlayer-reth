use alloy_primitives::{Address, TxHash};
use alloy_rpc_types_eth::{
    pubsub::{Params as AlloyParams, SubscriptionKind as AlloySubscriptionKind},
    Header,
};
use jsonrpsee::types::ErrorObject;
use reth_rpc_server_types::result::invalid_params_rpc_err;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const FLASHBLOCKS: &str = "flashblocks";

/// Subscription kind inclusive of flashblocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum FlashblockSubscriptionKind {
    /// Flashblocks subscription.
    #[serde(
        deserialize_with = "deserialize_flashblocks",
        serialize_with = "serialize_flashblocks"
    )]
    Flashblocks,
    /// Standard Ethereum subscription.
    Standard(AlloySubscriptionKind),
}

/// Helper to deserialize the unit variant from the string "flashblocks".
fn deserialize_flashblocks<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = <&str>::deserialize(deserializer)?;
    if s == FLASHBLOCKS {
        Ok(())
    } else {
        Err(serde::de::Error::custom(format!("expected '{FLASHBLOCKS}', got '{s}'")))
    }
}

/// Helper to serialize the unit variant as the string "flashblocks".
fn serialize_flashblocks<S>(serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(FLASHBLOCKS)
}

/// Extended params that wraps Alloy's `Params` and adds flashblocks specific variants.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum FlashblockParams {
    /// Flashblocks stream filter.
    FlashblocksFilter(FlashblocksFilter),
    /// Standard Ethereum subscription params.
    Standard(AlloyParams),
}

impl FlashblockParams {
    /// Validates the flashblock params.
    pub fn validate(&self, max_subscribed_addresses: usize) -> Result<(), ErrorObject<'static>> {
        if let FlashblockParams::FlashblocksFilter(filter) = self {
            if filter.sub_tx_filter.subscribe_addresses.len() > max_subscribed_addresses {
                return Err(invalid_params_rpc_err("too many subscribe addresses"));
            }
        }
        Ok(())
    }
}

/// Criteria for filtering and enriching flashblock subscription data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct FlashblocksFilter {
    /// Flag to subscribe to new block headers in the stream.
    pub header_info: bool,

    /// Tx criterias to subscribe to new transactions in the stream.
    pub sub_tx_filter: SubTxFilter,
}

impl FlashblocksFilter {
    /// Returns `true` if address filtering is enabled.
    pub fn requires_address_filtering(&self) -> bool {
        self.sub_tx_filter.has_address_filter()
    }
}

/// Criteria for filtering and enriching transaction subscription data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct SubTxFilter {
    /// Subscribed transactions involving these addresses.
    pub subscribe_addresses: HashSet<Address>,

    /// Flag to include full transaction information.
    pub tx_info: bool,

    /// Flag to include transaction receipts.
    pub tx_receipt: bool,
}

impl SubTxFilter {
    /// Returns `true` if address filtering is enabled.
    pub fn has_address_filter(&self) -> bool {
        !self.subscribe_addresses.is_empty()
    }
}

/// Streaming flashblock event which is either a header or transaction message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FlashblockStreamEvent<H, Tx, R> {
    /// Block header event
    Header {
        #[serde(skip_serializing)]
        block_number: u64,
        header: Header<H>,
    },
    /// Individual transaction event
    Transaction {
        #[serde(skip_serializing)]
        block_number: u64,
        transaction: EnrichedTransaction<Tx, R>,
    },
}

impl<H, Tx, R> FlashblockStreamEvent<H, Tx, R> {
    /// Get the block number for this event
    pub fn block_number(&self) -> u64 {
        match self {
            FlashblockStreamEvent::Header { block_number, .. } => *block_number,
            FlashblockStreamEvent::Transaction { block_number, .. } => *block_number,
        }
    }
}

/// Transaction data with optional enrichment based on `FlashblocksFilter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichedTransaction<Tx, R> {
    /// Transaction hash.
    pub tx_hash: TxHash,

    /// Transaction data (if `tx_info` is true in filter criteria).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_data: Option<Tx>,

    /// Transaction receipt (if `tx_receipt` is true in filter criteria).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<R>,
}
