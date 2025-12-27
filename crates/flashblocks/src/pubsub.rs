use alloy_consensus::BlockHeader;
use alloy_primitives::{Address, TxHash};
use alloy_rpc_types_eth::{
    pubsub::{Params as AlloyParams, SubscriptionKind as AlloySubscriptionKind},
    Header,
};
use serde::{Deserialize, Serialize};

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
    pub subscribe_addresses: Vec<Address>,

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

/// Flashblock data returned to subscribers based on `FlashblocksFilter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichedFlashblock<H, Tx, R> {
    /// Block header (if `header_info` is true in filter criteria).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<Header<H>>,

    /// Transactions with optional enrichment, based on the tx filter critera.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub transactions: Vec<EnrichedTransaction<Tx, R>>,
}

impl<H, Tx, R> EnrichedFlashblock<H, Tx, R>
where
    H: BlockHeader,
{
    pub fn block_number(&self) -> u64 {
        self.header.as_ref().map(|h| h.number()).unwrap_or(0)
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
