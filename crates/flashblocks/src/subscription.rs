use crate::pubsub::{
    EnrichedFlashblock, EnrichedTransaction, FlashblockParams, FlashblockSubscriptionKind,
    FlashblocksFilter,
};
use alloy_consensus::{transaction::TxHashRef, BlockHeader as _, Transaction as _, TxReceipt as _};
use alloy_json_rpc::RpcObject;
use alloy_primitives::Address;
use alloy_rpc_types_eth::{Header, TransactionInfo};
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
use reth_rpc_eth_api::{EthApiTypes, RpcNodeCore, RpcReceipt, RpcTransaction};
use reth_rpc_server_types::result::{internal_rpc_err, invalid_params_rpc_err};
use reth_storage_api::BlockNumReader;
use reth_tasks::TaskSpawner;
use serde::Serialize;
use std::{future::ready, sync::Arc};
use tokio_stream::{wrappers::WatchStream, Stream};

type FlashblockItem<N, C> = EnrichedFlashblock<
    <N as NodePrimitives>::BlockHeader,
    RpcTransaction<<C as RpcConvert>::Network>,
    RpcReceipt<<C as RpcConvert>::Network>,
>;

type EnrichedTxItem<C> = EnrichedTransaction<
    RpcTransaction<<C as RpcConvert>::Network>,
    RpcReceipt<<C as RpcConvert>::Network>,
>;

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
        params: Option<FlashblockParams>,
    ) -> jsonrpsee::core::SubscriptionResult;
}

/// Optimism-specific Ethereum pubsub handler that extends standard subscriptions with flashblocks support.
#[derive(Clone)]
pub struct FlashblocksPubSub<Eth: EthApiTypes, N: NodePrimitives> {
    /// Standard eth pubsub handler
    eth_pubsub: EthPubSub<Eth>,
    /// All nested flashblocks fields bundled together
    inner: Arc<FlashblocksPubSubInner<Eth, N>>,
}

impl<Eth: EthApiTypes, N: NodePrimitives> FlashblocksPubSub<Eth, N>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::Provider: BlockNumReader + CanonStateSubscriptions<Primitives = N>,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
{
    /// Creates a new, shareable instance.
    ///
    /// Subscription tasks are spawned via [`tokio::task::spawn`]
    pub fn new(
        eth_pubsub: EthPubSub<Eth>,
        pending_block_rx: PendingBlockRx<N>,
        subscription_task_spawner: Box<dyn TaskSpawner>,
        tx_converter: Eth::RpcConvert,
    ) -> Self {
        let inner =
            FlashblocksPubSubInner { pending_block_rx, subscription_task_spawner, tx_converter };
        Self { eth_pubsub, inner: Arc::new(inner) }
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

    pub fn new_flashblocks_stream(
        &self,
        filter: FlashblocksFilter,
    ) -> impl Stream<Item = FlashblockItem<N, Eth::RpcConvert>> {
        self.inner.new_flashblocks_stream(filter)
    }

    async fn handle_accepted(
        &self,
        accepted_sink: SubscriptionSink,
        kind: FlashblockSubscriptionKind,
        params: Option<FlashblockParams>,
    ) -> Result<(), ErrorObject<'static>> {
        match kind {
            FlashblockSubscriptionKind::Flashblocks => {
                let Some(FlashblockParams::FlashblocksFilter(filter)) = params else {
                    return Err(invalid_params_rpc_err("invalid params for flashblocks"));
                };

                if (filter.sub_tx_filter.tx_info || filter.sub_tx_filter.tx_receipt)
                    && filter.sub_tx_filter.subscribe_addresses.is_empty()
                {
                    return Err(invalid_params_rpc_err(
                        "invalid params for flashblocks, subcribe address required when txInfo or txReceipt is enabled",
                    ));
                }

                let stream = self.new_flashblocks_stream(filter);
                pipe_from_stream(accepted_sink, stream).await
            }
            FlashblockSubscriptionKind::Standard(alloy_kind) => {
                let standard_params = match params {
                    Some(FlashblockParams::Standard(p)) => Some(p),
                    Some(FlashblockParams::FlashblocksFilter(_)) => {
                        return Err(invalid_params_rpc_err(
                            "invalid params, incorrect flashblocks filter provided for standard eth subscription type",
                        ));
                    }
                    None => None,
                };
                self.eth_pubsub.handle_accepted(accepted_sink, alloy_kind, standard_params).await
            }
        }
    }
}

#[async_trait::async_trait]
impl<Eth: EthApiTypes, N: NodePrimitives>
    FlashblocksPubSubApiServer<RpcTransaction<Eth::NetworkTypes>> for FlashblocksPubSub<Eth, N>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::Provider: BlockNumReader + CanonStateSubscriptions<Primitives = N>,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
{
    async fn subscribe(
        &self,
        pending: PendingSubscriptionSink,
        kind: FlashblockSubscriptionKind,
        params: Option<FlashblockParams>,
    ) -> jsonrpsee::core::SubscriptionResult {
        let sink = pending.accept().await?;
        let pubsub = self.clone();
        self.inner.subscription_task_spawner.spawn(Box::pin(async move {
            let _ = pubsub.handle_accepted(sink, kind, params).await;
        }));

        Ok(())
    }
}

#[derive(Clone)]
pub struct FlashblocksPubSubInner<Eth: EthApiTypes, N: NodePrimitives> {
    /// Pending block receiver from flashblocks, if available
    pub(crate) pending_block_rx: PendingBlockRx<N>,
    /// The type that's used to spawn subscription tasks.
    pub(crate) subscription_task_spawner: Box<dyn TaskSpawner>,
    /// RPC transaction converter
    pub(crate) tx_converter: Eth::RpcConvert,
}

impl<Eth: EthApiTypes, N: NodePrimitives> FlashblocksPubSubInner<Eth, N>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::Provider: BlockNumReader + CanonStateSubscriptions<Primitives = N>,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
{
    fn new_flashblocks_stream(
        &self,
        filter: FlashblocksFilter,
    ) -> impl Stream<Item = FlashblockItem<N, Eth::RpcConvert>> {
        let tx_converter = self.tx_converter.clone();
        WatchStream::new(self.pending_block_rx.clone()).filter_map(move |pending_block_opt| {
            ready(pending_block_opt.and_then(|pending_block| {
                Self::filter_and_enrich_flashblock(&pending_block, &filter, &tx_converter)
            }))
        })
    }

    /// Filter and enrich a flashblock based on the provided filter criteria.
    fn filter_and_enrich_flashblock(
        pending_block: &PendingFlashBlock<N>,
        filter: &FlashblocksFilter,
        tx_converter: &Eth::RpcConvert,
    ) -> Option<FlashblockItem<N, Eth::RpcConvert>> {
        let header = if filter.header_info {
            Some(match extract_header_from_pending_block(pending_block) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("Failed to extract header: {e:?}");
                    return None;
                }
            })
        } else {
            None
        };

        let block = pending_block.block();
        let receipts = pending_block.receipts.as_ref();
        let sealed_block = block.sealed_block();

        let transactions: Vec<EnrichedTxItem<Eth::RpcConvert>> = block
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
                    sealed_block,
                    tx_converter,
                );

                let tx_receipt = Self::enrich_receipt(
                    filter,
                    receipt,
                    tx,
                    *sender,
                    idx,
                    tx_hash,
                    receipts,
                    sealed_block,
                    tx_converter,
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
        filter: &FlashblocksFilter,
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
        filter: &FlashblocksFilter,
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
        if let Some(to) = tx.to()
            && addresses.contains(&to)
        {
            return true;
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

/// Helper to convert a serde error into an [`ErrorObject`]
#[derive(Debug)]
pub struct SubscriptionSerializeError(serde_json::Error);

impl SubscriptionSerializeError {
    const fn new(err: serde_json::Error) -> Self {
        Self(err)
    }
}

impl std::fmt::Display for SubscriptionSerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Failed to serialize subscription item: {}", self.0)
    }
}

impl From<SubscriptionSerializeError> for ErrorObject<'static> {
    fn from(value: SubscriptionSerializeError) -> Self {
        internal_rpc_err(value.to_string())
    }
}

/// Pipes all stream items to the subscription sink.
async fn pipe_from_stream<T, St>(
    sink: SubscriptionSink,
    mut stream: St,
) -> Result<(), ErrorObject<'static>>
where
    St: Stream<Item = T> + Unpin,
    T: Serialize,
{
    loop {
        tokio::select! {
            _ = sink.closed() => {
                // connection dropped
                break Ok(())
            },
            maybe_item = stream.next() => {
                let item = match maybe_item {
                    Some(item) => item,
                    None => {
                        // stream ended
                        break  Ok(())
                    },
                };
                let msg = SubscriptionMessage::new(
                    sink.method_name(),
                    sink.subscription_id(),
                    &item
                ).map_err(SubscriptionSerializeError::new)?;

                if sink.send(msg).await.is_err() {
                    break Ok(());
                }
            }
        }
    }
}

/// Extract `Header` from `PendingFlashBlock`
fn extract_header_from_pending_block<N: NodePrimitives>(
    pending_block: &PendingFlashBlock<N>,
) -> Result<Header<N::BlockHeader>, ErrorObject<'static>> {
    let block = pending_block.block();
    let sealed_header = block.clone_sealed_header();

    Ok(Header::from_consensus(sealed_header.into(), None, None))
}
