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
    /// No params
    None,
}

impl<'de> Deserialize<'de> for OpParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;

        if value.is_null() {
            return Ok(OpParams::None);
        }

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
            OpParams::None => serializer.serialize_none(),
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlashBlocksFilter {
    /// Include new block headers in the stream.
    #[serde(default)]
    pub header_info: bool,

    /// SubTxFilter
    #[serde(default)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubTxFilter {
    /// Include extra transaction information
    #[serde(default)]
    pub tx_info: bool,

    /// Include transaction receipts.
    #[serde(default)]
    pub tx_receipt: bool,

    /// Only include transactions involving these addresses
    #[serde(default)]
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
        kind: OpSubscriptionKind,
        params: Option<OpParams>,
    ) -> jsonrpsee::core::SubscriptionResult;
}

/// Optimism-specific Ethereum pubsub handler that extends standard subscriptions with flashblocks support.
pub struct FlashblocksPubSub<Eth: EthApiTypes, N: NodePrimitives> {
    /// Standard eth pubsub handler
    eth_pubsub: EthPubSub<Eth>,
    /// Pending block receiver from flashblocks, if available
    pending_block_rx: PendingBlockRx<N>,
    /// Direct reference to eth API for RPC conversion
    eth_api: Eth,
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
            eth_api: self.eth_api.clone(),
        }
    }
}

    /// Creates a new `FlashblocksPubSub` instance.
    pub fn new(
        eth_pubsub: EthPubSub<Eth>,
        pending_block_rx: PendingBlockRx<N>,
        eth_api: Eth,
    ) -> Self {
        Self { eth_pubsub, pending_block_rx, eth_api }
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

#[async_trait::async_trait]
impl<Eth: EthApiTypes, N: NodePrimitives>
    FlashblocksPubSubApiServer<RpcTransaction<Eth::NetworkTypes>> for FlashblocksPubSub<Eth, N>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::Provider: BlockNumReader + CanonStateSubscriptions<Primitives = N>,
    Eth::Pool: TransactionPool,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
{
    async fn subscribe(
        &self,
        pending: PendingSubscriptionSink,
        kind: FlashblockSubscriptionKind,
        params: Option<OpParams>,
    ) -> jsonrpsee::core::SubscriptionResult {
        // Extract FlashBlocksFilter from params if present
        let filter =
            params.as_ref().and_then(|p| p.as_flashblocks_filter()).cloned().unwrap_or_default();

        match kind {
            FlashblockSubscriptionKind::Flashblocks => {
                if (filter.sub_tx_filter.tx_info || filter.sub_tx_filter.tx_receipt)
                    && filter.sub_tx_filter.subscribe_addresses.is_empty()
                {
                    let err = invalid_params_rpc_err(
                    "subscribeAddresses is required when txInfo or txReceipt is enabled. Provide at least one address to monitor.",
                );
                    pending.accept().await?;
                    return Err(jsonrpsee::core::SubscriptionError::from(err));
                }

                return self
                    .filter_flashblocks_stream(pending, &self.pending_block_rx, &filter)
                    .await;
            }
            FlashblockSubscriptionKind::Standard(alloy_kind) => {
                // If it is a non-flashblocks subscription, forward it to the original subscribe implementation
                let standard_params = params.and_then(|p| p.as_standard().cloned());

                self.eth_pubsub.subscribe(pending, alloy_kind, standard_params).await?;
                Ok(())
            }
        }
    }
}

impl<Eth: EthApiTypes, N: NodePrimitives> FlashblocksPubSub<Eth, N>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::Provider: BlockNumReader + CanonStateSubscriptions<Primitives = N>,
    Eth::Pool: TransactionPool,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
{
    async fn filter_flashblocks_stream(
        &self,
        pending: PendingSubscriptionSink,
        pending_block_rx: &PendingBlockRx<N>,
        filter: &FlashBlocksFilter,
    ) -> jsonrpsee::core::SubscriptionResult {
        let sink = pending.accept().await?;
        let pending_block_rx = pending_block_rx.clone();
        let filter = filter.clone();
        let api = self.eth_api.clone();

        let flashblocks_stream =
            WatchStream::new(pending_block_rx).filter_map(move |pending_block_opt| {
                let filter = filter.clone();
                let api = api.clone();
                async move {
                    pending_block_opt.and_then(|pending_block| {
                        Self::filter_and_enrich_flashblock(&pending_block, &filter, &api)
                    })
                }
            });
        let pinned_stream = Box::pin(flashblocks_stream);

        tokio::spawn(async move {
            let _ = pipe_from_stream(sink, pinned_stream).await;
        });

        Ok(())
    }

    /// Filter and enrich a flashblock based on the provided filter criteria.
    fn filter_and_enrich_flashblock(
        pending_block: &PendingFlashBlock<N>,
        filter: &FlashBlocksFilter,
        api: &Eth,
    ) -> Option<
        EnrichedFlashblock<
            N::BlockHeader,
            RpcTransaction<<Eth::RpcConvert as RpcConvert>::Network>,
            RpcReceipt<<Eth::RpcConvert as RpcConvert>::Network>,
        >,
    > {
        let header = if filter.header_info {
            Some(match extract_header_from_pending_block(pending_block) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("Failed to extract header: {:?}", e);
                    return None;
                }
            })
        } else {
            None
        };

        let block = pending_block.block();
        let receipts = pending_block.receipts.as_ref();
        let sealed_block = block.sealed_block();
        let tx_converter = api.tx_resp_builder();

        let transactions: Vec<
            EnrichedTransaction<
                RpcTransaction<<Eth::RpcConvert as RpcConvert>::Network>,
                RpcReceipt<<Eth::RpcConvert as RpcConvert>::Network>,
            >,
        > = block
            .transactions_with_sender()
            .enumerate()
            .filter_map(|(idx, (sender, tx))| {
                if filter.requires_address_filtering() {
                    let matches_filter = Self::is_address_in_transaction(
                        *sender,
                        tx,
                        receipts.get(idx),
                        &filter.sub_tx_filter.subscribe_addresses,
                    );
                    if !matches_filter {
                        return None;
                    }
                }

                let receipt = receipts.get(idx)?;
                let tx_hash = *tx.tx_hash();

                let tx_data = Self::enrich_transaction_data(
                    filter,
                    tx,
                    *sender,
                    idx,
                    tx_hash,
                    &sealed_block,
                    &tx_converter,
                );

                let tx_receipt = Self::enrich_receipt(
                    filter,
                    receipt,
                    tx,
                    *sender,
                    idx,
                    tx_hash,
                    receipts,
                    &sealed_block,
                    &tx_converter,
                );

                Some(EnrichedTransaction { tx_hash, tx_data, receipt: tx_receipt })
            })
            .collect();

        if filter.sub_tx_filter.has_address_filter() && transactions.is_empty() {
            return None;
        }

        Some(EnrichedFlashblock { header, transactions })
    }

    /// Enrich transaction data if requested in filter
    fn enrich_transaction_data(
        filter: &FlashBlocksFilter,
        tx: &N::SignedTx,
        sender: Address,
        idx: usize,
        tx_hash: alloy_primitives::TxHash,
        sealed_block: &SealedBlock<N::Block>,
        rpc_convert: &Eth::RpcConvert,
    ) -> Option<RpcTransaction<<Eth::RpcConvert as RpcConvert>::Network>> {
        if !filter.sub_tx_filter.tx_info {
            return None;
        }

        let recovered = reth_primitives_traits::Recovered::new_unchecked(tx.clone(), sender);

        let rpc_tx = rpc_convert
            .fill(
                recovered,
                TransactionInfo {
                    hash: Some(tx_hash),
                    index: Some(idx as u64),
                    block_hash: Some(sealed_block.hash()),
                    block_number: Some(sealed_block.header().number()),
                    base_fee: sealed_block.header().base_fee_per_gas(),
                },
            )
            .ok()?;

        Some(rpc_tx)
    }

    /// Enrich receipt data if requested in filter
    fn enrich_receipt(
        filter: &FlashBlocksFilter,
        receipt: &N::Receipt,
        tx: &N::SignedTx,
        sender: Address,
        idx: usize,
        tx_hash: alloy_primitives::TxHash,
        receipts: &[N::Receipt],
        sealed_block: &SealedBlock<N::Block>,
        tx_converter: &Eth::RpcConvert,
    ) -> Option<RpcReceipt<<Eth::RpcConvert as RpcConvert>::Network>> {
        if !filter.sub_tx_filter.tx_receipt {
            return None;
        }

        let gas_used = receipt.cumulative_gas_used();

        let next_log_index = receipts.iter().take(idx).map(|r| r.logs().len()).sum::<usize>();

        let receipt_input = ConvertReceiptInput {
            receipt: receipt.clone(),
            tx: Recovered::new_unchecked(tx, sender),
            gas_used,
            next_log_index,
            meta: TransactionMeta {
                tx_hash,
                index: idx as u64,
                block_hash: sealed_block.hash(),
                block_number: sealed_block.header().number(),
                base_fee: sealed_block.header().base_fee_per_gas(),
                excess_blob_gas: sealed_block.header().excess_blob_gas(),
                timestamp: sealed_block.header().timestamp(),
            },
        };

        let rpc_receipts =
            tx_converter.convert_receipts_with_block(vec![receipt_input], sealed_block).ok()?;

        rpc_receipts.first().cloned()
    }

    fn is_address_in_transaction(
        sender: Address,
        tx: &N::SignedTx,
        receipt: Option<&N::Receipt>,
        addresses: &[Address],
    ) -> bool {
        // Check sender
        if addresses.contains(&sender) {
            return true;
        }

        // Check recipient
        if let Some(to) = tx.to() {
            if addresses.contains(&to) {
                return true;
            }
        }

        // Check log addresses
        if let Some(receipt) = receipt {
            for log in receipt.logs() {
                if addresses.contains(&log.address) {
                    return true;
                }
            }
        }

        false
    }
}

/// Pipes all stream items to the subscription sink.
async fn pipe_from_stream<T, St>(
    sink: SubscriptionSink,
    stream: Pin<Box<St>>,
) -> Result<(), ErrorObject<'static>>
where
    St: Stream<Item = T> + ?Sized,
    T: Serialize,
{
    let mut stream = stream;
    loop {
        tokio::select! {
            _ = sink.closed() => {
                // connection dropped
                break Ok(())
            }
            maybe_item = StreamExt::next(&mut stream) => {
                let item = match maybe_item {
                    Some(item) => item,
                    None => {
                        // stream ended
                        break Ok(())
                    }
                };
                let msg = SubscriptionMessage::new(
                    sink.method_name(),
                    sink.subscription_id(),
                    &item,
                )
                .map_err(|e| internal_rpc_err(format!("Failed to serialize item: {e}")))?;

                if sink.send(msg).await.is_err() {
                    break Ok(());
                }
            }
        }
    }
}

/// Extract Header from PendingFlashBlock
fn extract_header_from_pending_block<N: NodePrimitives>(
    pending_block: &PendingFlashBlock<N>,
) -> Result<Header<N::BlockHeader>, ErrorObject<'static>> {
    let block = pending_block.block();
    let sealed_header = block.clone_sealed_header();

    Ok(Header::from_consensus(sealed_header.into(), None, None))
}
