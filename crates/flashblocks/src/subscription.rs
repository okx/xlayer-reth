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
use reth_chain_state::{CanonStateNotification, CanonStateSubscriptions};
use reth_optimism_flashblocks::{PendingBlockRx, PendingFlashBlock};
use reth_primitives_traits::{
    NodePrimitives, Recovered, RecoveredBlock, SealedBlock, TransactionMeta,
};
use reth_rpc::eth::pubsub::EthPubSub;
use reth_rpc_convert::{transaction::ConvertReceiptInput, RpcConvert};
use reth_rpc_eth_api::{EthApiTypes, RpcNodeCore, RpcReceipt, RpcTransaction};
use reth_rpc_server_types::result::{internal_rpc_err, invalid_params_rpc_err};
use reth_storage_api::{BlockNumReader, ReceiptProvider};
use reth_tasks::TaskSpawner;
use reth_tracing::tracing::warn;
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

/// Context for enriching transactions and receipts from a block
struct EnrichmentContext<'a, N: NodePrimitives, C> {
    tx: &'a N::SignedTx,
    sender: Address,
    idx: usize,
    tx_hash: alloy_primitives::TxHash,
    sealed_block: &'a SealedBlock<N::Block>,
    tx_converter: &'a C,
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
        params: Option<FlashblockParams>,
    ) -> jsonrpsee::core::SubscriptionResult;
}

/// Optimism-specific Ethereum pubsub handler that extends standard subscriptions with flashblocks support.
#[derive(Clone)]
pub struct FlashblocksPubSub<Eth: EthApiTypes, N: NodePrimitives, Provider> {
    /// Standard eth pubsub handler
    eth_pubsub: EthPubSub<Eth>,
    /// All nested flashblocks fields bundled together
    inner: Arc<FlashblocksPubSubInner<Eth, N, Provider>>,
}

impl<Eth: EthApiTypes, N: NodePrimitives, Provider> FlashblocksPubSub<Eth, N, Provider>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::Provider: BlockNumReader + CanonStateSubscriptions<Primitives = N>,
    Provider: ReceiptProvider<Receipt = N::Receipt>
        + CanonStateSubscriptions<Primitives = N>
        + Clone
        + 'static,
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
        provider: Provider,
    ) -> Self {
        let inner = FlashblocksPubSubInner {
            pending_block_rx,
            subscription_task_spawner,
            tx_converter,
            provider,
        };
        Self { eth_pubsub, inner: Arc::new(inner) }
    }

    /// Converts this `FlashblocksPubSub` into an RPC module.
    pub fn into_rpc(self) -> jsonrpsee::RpcModule<()>
    where
        FlashblocksPubSub<Eth, N, Provider>:
            FlashblocksPubSubApiServer<RpcTransaction<Eth::NetworkTypes>>,
    {
        <FlashblocksPubSub<Eth, N, Provider> as FlashblocksPubSubApiServer<
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

    pub fn new_canonical_state_stream(
        &self,
        filter: FlashblocksFilter,
    ) -> impl Stream<Item = FlashblockItem<N, Eth::RpcConvert>> {
        self.inner.new_canonical_state_stream(filter)
    }

    fn validate_params(
        &self,
        kind: &FlashblockSubscriptionKind,
        params: &Option<FlashblockParams>,
    ) -> Result<(), ErrorObject<'static>> {
        match kind {
            FlashblockSubscriptionKind::Flashblocks => {
                let Some(FlashblockParams::FlashblocksFilter(_)) = params else {
                    return Err(invalid_params_rpc_err("invalid params for flashblocks"));
                };

                Ok(())
            }
            FlashblockSubscriptionKind::Standard(_) => {
                if matches!(params, Some(FlashblockParams::FlashblocksFilter(_))) {
                    return Err(invalid_params_rpc_err(
                        "invalid params, incorrect filter provided for standard eth subscription type",
                    ));
                }
                Ok(())
            }
        }
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

                let fb_stream = self.new_flashblocks_stream(filter.clone());
                let canon_stream = self.new_canonical_state_stream(filter);
                pipe_from_flashblocks_and_canonical_state_stream::<N, Eth, _, _>(
                    accepted_sink,
                    fb_stream,
                    canon_stream,
                )
                .await
            }
            FlashblockSubscriptionKind::Standard(alloy_kind) => {
                let standard_params = match params {
                    Some(FlashblockParams::Standard(p)) => Some(p),
                    None => None,
                    _ => {
                        return Err(invalid_params_rpc_err(
                            "invalid params for standard eth subscription",
                        ))
                    }
                };
                self.eth_pubsub.handle_accepted(accepted_sink, alloy_kind, standard_params).await
            }
        }
    }
}

#[async_trait::async_trait]
impl<Eth: EthApiTypes, N: NodePrimitives, Provider>
    FlashblocksPubSubApiServer<RpcTransaction<Eth::NetworkTypes>>
    for FlashblocksPubSub<Eth, N, Provider>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::Provider: BlockNumReader + CanonStateSubscriptions<Primitives = N>,
    Provider: ReceiptProvider<Receipt = N::Receipt>
        + CanonStateSubscriptions<Primitives = N>
        + Clone
        + 'static,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
{
    async fn subscribe(
        &self,
        pending: PendingSubscriptionSink,
        kind: FlashblockSubscriptionKind,
        params: Option<FlashblockParams>,
    ) -> jsonrpsee::core::SubscriptionResult {
        // Validate and reject with error message if invalid
        if let Err(err) = self.validate_params(&kind, &params) {
            pending.reject(err).await;
            return Ok(());
        }

        let sink = pending.accept().await?;
        let pubsub = self.clone();
        self.inner.subscription_task_spawner.spawn(Box::pin(async move {
            let _ = pubsub.handle_accepted(sink, kind, params).await;
        }));

        Ok(())
    }
}

#[derive(Clone)]
pub struct FlashblocksPubSubInner<Eth: EthApiTypes, N: NodePrimitives, Provider> {
    /// Pending block receiver from flashblocks, if available
    pub(crate) pending_block_rx: PendingBlockRx<N>,
    /// The type that's used to spawn subscription tasks.
    pub(crate) subscription_task_spawner: Box<dyn TaskSpawner>,
    /// RPC transaction converter.
    pub(crate) tx_converter: Eth::RpcConvert,
    /// Blockchain provider for chainstate notifications and fetching receipts.
    pub(crate) provider: Provider,
}

impl<Eth: EthApiTypes, N: NodePrimitives, Provider> FlashblocksPubSubInner<Eth, N, Provider>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Provider: ReceiptProvider<Receipt = N::Receipt>
        + CanonStateSubscriptions<Primitives = N>
        + Clone
        + 'static,
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

    fn new_canonical_state_stream(
        &self,
        filter: FlashblocksFilter,
    ) -> impl Stream<Item = FlashblockItem<N, Eth::RpcConvert>> {
        self.provider
            .canonical_state_stream()
            .map(move |canon_state| {
                let chain = match canon_state {
                    CanonStateNotification::Commit { new } => new,
                    CanonStateNotification::Reorg { old: _, new } => new,
                };

                let blocks: Vec<_> = chain
                    .blocks_iter()
                    .filter_map(|block| {
                        Self::filter_and_enrich_canonical_block(
                            block,
                            &self.provider,
                            &filter,
                            &self.tx_converter,
                        )
                    })
                    .collect();

                futures::stream::iter(blocks)
            })
            .flatten()
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
                    warn!(target: "xlayer::flashblocks", error = ?e, "Failed to extract header");
                    return None;
                }
            })
        } else {
            None
        };

        let block = pending_block.block();
        let receipts = pending_block.receipts.as_ref();
        let sealed_block = block.sealed_block();

        let transactions =
            Self::collect_transactions(block, filter, receipts, tx_converter, sealed_block);

        if filter.sub_tx_filter.has_address_filter() && transactions.is_empty() {
            return None;
        }

        Some(EnrichedFlashblock { header, transactions })
    }

    fn filter_and_enrich_canonical_block(
        block: &RecoveredBlock<N::Block>,
        provider: &Provider,
        filter: &FlashblocksFilter,
        tx_converter: &Eth::RpcConvert,
    ) -> Option<FlashblockItem<N, Eth::RpcConvert>> {
        let header = if filter.header_info {
            let sealed_header = block.clone_sealed_header();
            Some(Header::from_consensus(sealed_header.into(), None, None))
        } else {
            None
        };

        let sealed_block = block.sealed_block();

        let receipts_result =
            provider.receipts_by_block(alloy_eips::BlockHashOrNumber::Hash(sealed_block.hash()));

        let receipts_vec = match receipts_result {
            Ok(Some(receipts)) => receipts,
            Ok(None) => {
                warn!(
                    target: "xlayer::flashblocks",
                    block_number = sealed_block.number(),
                    block_hash = ?sealed_block.hash(),
                    "No receipts found in storage for canonical block"
                );
                vec![]
            }
            Err(e) => {
                warn!(
                    target: "xlayer::flashblocks",
                    block_number = sealed_block.number(),
                    block_hash = ?sealed_block.hash(),
                    error = ?e,
                    "Failed to fetch receipts from provider"
                );
                vec![]
            }
        };

        let transactions =
            Self::collect_transactions(block, filter, &receipts_vec, tx_converter, sealed_block);

        if filter.sub_tx_filter.has_address_filter() && transactions.is_empty() {
            return None;
        }

        Some(EnrichedFlashblock { header, transactions })
    }

    fn collect_transactions(
        block: &RecoveredBlock<N::Block>,
        filter: &FlashblocksFilter,
        receipts: &[N::Receipt],
        tx_converter: &Eth::RpcConvert,
        sealed_block: &SealedBlock<N::Block>,
    ) -> Vec<EnrichedTxItem<Eth::RpcConvert>> {
        block
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

                let ctx = EnrichmentContext {
                    tx,
                    sender: *sender,
                    idx,
                    tx_hash,
                    sealed_block,
                    tx_converter,
                };

                let tx_data = Self::enrich_transaction_data(filter, &ctx);
                let tx_receipt = Self::enrich_receipt(filter, receipt, receipts, &ctx);

                Some(EnrichedTransaction { tx_hash, tx_data, receipt: tx_receipt })
            })
            .collect()
    }

    /// Enrich transaction data if requested in filter
    fn enrich_transaction_data(
        filter: &FlashblocksFilter,
        ctx: &EnrichmentContext<'_, N, Eth::RpcConvert>,
    ) -> Option<RpcTransaction<<Eth::RpcConvert as RpcConvert>::Network>> {
        if !filter.sub_tx_filter.tx_info {
            return None;
        }

        let recovered =
            reth_primitives_traits::Recovered::new_unchecked(ctx.tx.clone(), ctx.sender);

        let rpc_tx = ctx
            .tx_converter
            .fill(
                recovered,
                TransactionInfo {
                    hash: Some(ctx.tx_hash),
                    index: Some(ctx.idx as u64),
                    block_hash: Some(ctx.sealed_block.hash()),
                    block_number: Some(ctx.sealed_block.header().number()),
                    base_fee: ctx.sealed_block.header().base_fee_per_gas(),
                },
            )
            .ok()?;

        Some(rpc_tx)
    }

    /// Enrich receipt data if requested in filter
    fn enrich_receipt(
        filter: &FlashblocksFilter,
        receipt: &N::Receipt,
        receipts: &[N::Receipt],
        ctx: &EnrichmentContext<'_, N, Eth::RpcConvert>,
    ) -> Option<RpcReceipt<<Eth::RpcConvert as RpcConvert>::Network>> {
        if !filter.sub_tx_filter.tx_receipt {
            return None;
        }

        let gas_used = receipt.cumulative_gas_used();

        let next_log_index = receipts.iter().take(ctx.idx).map(|r| r.logs().len()).sum::<usize>();

        let receipt_input = ConvertReceiptInput {
            receipt: receipt.clone(),
            tx: Recovered::new_unchecked(ctx.tx, ctx.sender),
            gas_used,
            next_log_index,
            meta: TransactionMeta {
                tx_hash: ctx.tx_hash,
                index: ctx.idx as u64,
                block_hash: ctx.sealed_block.hash(),
                block_number: ctx.sealed_block.header().number(),
                base_fee: ctx.sealed_block.header().base_fee_per_gas(),
                excess_blob_gas: ctx.sealed_block.header().excess_blob_gas(),
                timestamp: ctx.sealed_block.header().timestamp(),
            },
        };

        let rpc_receipts = ctx
            .tx_converter
            .convert_receipts_with_block(vec![receipt_input], ctx.sealed_block)
            .ok()?;

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
async fn pipe_from_flashblocks_and_canonical_state_stream<N, Eth, FbSt, CanonSt>(
    sink: SubscriptionSink,
    mut fb_stream: FbSt,
    mut canon_stream: CanonSt,
) -> Result<(), ErrorObject<'static>>
where
    N: NodePrimitives,
    N::BlockHeader: alloy_consensus::BlockHeader,
    Eth: EthApiTypes + RpcNodeCore<Primitives = N> + 'static,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
    FbSt: Stream<Item = FlashblockItem<N, Eth::RpcConvert>> + Unpin,
    CanonSt: Stream<Item = FlashblockItem<N, Eth::RpcConvert>> + Unpin,
{
    let mut last_sent_height = 0;
    loop {
        tokio::select! {
            _ = sink.closed() => {
                // connection dropped
                break Ok(())
            },
            maybe_fb_item = fb_stream.next() => {
                let item = match maybe_fb_item {
                    Some(item) => item,
                    None => {
                        // stream ended
                        break Ok(())
                    },
                };

                let block_num = item.block_number();
                if block_num < last_sent_height {
                    // Flashblocks stream is lagging, skip
                    continue
                }

                last_sent_height = block_num;
                let msg = SubscriptionMessage::new(
                    sink.method_name(),
                    sink.subscription_id(),
                    &item
                ).map_err(SubscriptionSerializeError::new)?;

                if sink.send(msg).await.is_err() {
                    break Ok(());
                }
            }
            maybe_canon_item = canon_stream.next() => {
                let item = match maybe_canon_item {
                    Some(item) => item,
                    None => {
                        // stream ended
                        break Ok(())
                    },
                };

                let block_num = item.block_number();
                if block_num <= last_sent_height {
                    // Fallback - same height is not allowed. Canonical stream is lagging, skip
                    continue
                }

                last_sent_height = block_num;
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
