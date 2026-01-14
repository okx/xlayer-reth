use crate::pubsub::{
    EnrichedTransaction, FlashblockParams, FlashblockStreamEvent, FlashblockSubscriptionKind,
    FlashblocksFilter,
};
use alloy_consensus::{transaction::TxHashRef, BlockHeader as _, Transaction as _, TxReceipt as _};
use alloy_json_rpc::RpcObject;
use alloy_primitives::{Address, TxHash, U256};
use alloy_rpc_types_eth::{Header, TransactionInfo};
use futures::StreamExt;
use jsonrpsee::{
    proc_macros::rpc, server::SubscriptionMessage, types::ErrorObject, PendingSubscriptionSink,
    SubscriptionSink,
};
use moka::policy::EvictionPolicy;
use moka::sync::Cache;
use reth_optimism_flashblocks::{PendingBlockRx, PendingFlashBlock};
use reth_primitives_traits::{
    NodePrimitives, Recovered, RecoveredBlock, SealedBlock, TransactionMeta,
};
use reth_rpc::eth::pubsub::EthPubSub;
use reth_rpc_convert::{transaction::ConvertReceiptInput, RpcConvert};
use reth_rpc_eth_api::{EthApiTypes, RpcNodeCore, RpcReceipt, RpcTransaction};
use reth_rpc_eth_types::utils::calculate_gas_used_and_next_log_index;
use reth_rpc_server_types::result::{internal_rpc_err, invalid_params_rpc_err};
use reth_storage_api::BlockNumReader;
use reth_tasks::TaskSpawner;
use reth_tracing::tracing::warn;
use std::{collections::HashSet, future::ready, sync::Arc};
use tokio_stream::{wrappers::WatchStream, Stream};

const MAX_TXHASH_CACHE_SIZE: u64 = 10_000;

type FlashblockItem<N, C> = FlashblockStreamEvent<
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
pub struct FlashblocksPubSub<Eth: EthApiTypes, N: NodePrimitives> {
    /// Standard eth pubsub handler
    eth_pubsub: EthPubSub<Eth>,
    /// All nested flashblocks fields bundled together
    inner: Arc<FlashblocksPubSubInner<Eth, N>>,
}

impl<Eth: EthApiTypes, N: NodePrimitives> FlashblocksPubSub<Eth, N>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::Provider: BlockNumReader,
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
        max_subscribed_addresses: usize,
    ) -> Self {
        let inner = FlashblocksPubSubInner {
            pending_block_rx,
            subscription_task_spawner,
            tx_converter,
            max_subscribed_addresses,
        };
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

                let fb_stream = self.new_flashblocks_stream(filter.clone());
                pipe_from_flashblocks_stream::<N, Eth, _>(accepted_sink, fb_stream).await
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
impl<Eth: EthApiTypes, N: NodePrimitives>
    FlashblocksPubSubApiServer<RpcTransaction<Eth::NetworkTypes>> for FlashblocksPubSub<Eth, N>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::Provider: BlockNumReader,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
{
    async fn subscribe(
        &self,
        pending: PendingSubscriptionSink,
        kind: FlashblockSubscriptionKind,
        params: Option<FlashblockParams>,
    ) -> jsonrpsee::core::SubscriptionResult {
        if let Some(params) = &params {
            if let Err(err) = params.validate(self.inner.max_subscribed_addresses) {
                pending.reject(err).await;
                return Ok(());
            }
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
pub struct FlashblocksPubSubInner<Eth: EthApiTypes, N: NodePrimitives> {
    /// Pending block receiver from flashblocks, if available
    pub(crate) pending_block_rx: PendingBlockRx<N>,
    /// The type that's used to spawn subscription tasks.
    pub(crate) subscription_task_spawner: Box<dyn TaskSpawner>,
    /// RPC transaction converter.
    pub(crate) tx_converter: Eth::RpcConvert,
    /// Maximum number of subscribed addresses.
    pub(crate) max_subscribed_addresses: usize,
}

impl<Eth: EthApiTypes, N: NodePrimitives> FlashblocksPubSubInner<Eth, N>
where
    Eth: RpcNodeCore<Primitives = N> + 'static,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
{
    fn new_flashblocks_stream(
        &self,
        filter: FlashblocksFilter,
    ) -> impl Stream<Item = FlashblockItem<N, Eth::RpcConvert>> {
        let tx_converter = self.tx_converter.clone();
        let txhash_cache = Cache::builder()
            .max_capacity(MAX_TXHASH_CACHE_SIZE)
            .eviction_policy(EvictionPolicy::lru())
            .build();

        WatchStream::new(self.pending_block_rx.clone())
            .filter_map(move |pending_block_opt| {
                ready(pending_block_opt.map(|pending_block| {
                    futures::stream::iter(Self::flashblock_to_stream_events(
                        &pending_block,
                        &filter,
                        &tx_converter,
                        &txhash_cache,
                    ))
                }))
            })
            .flatten()
    }

    /// Convert a flashblock into a stream of events (header + transaction messages)
    fn flashblock_to_stream_events(
        pending_block: &PendingFlashBlock<N>,
        filter: &FlashblocksFilter,
        tx_converter: &Eth::RpcConvert,
        txhash_cache: &Cache<TxHash, ()>,
    ) -> Vec<FlashblockItem<N, Eth::RpcConvert>> {
        let block = pending_block.block();
        let receipts = pending_block.receipts.as_ref();
        let sealed_block = block.sealed_block();
        let block_number = sealed_block.header().number();

        let mut events = Vec::new();

        if filter.header_info {
            match extract_header_from_pending_block(pending_block) {
                Ok(header) => {
                    events.push(FlashblockStreamEvent::Header { block_number, header });
                }
                Err(e) => {
                    warn!(target: "xlayer::flashblocks", error = ?e, "Failed to extract header");
                }
            }
        }

        events.extend(
            Self::collect_transactions(
                block,
                filter,
                receipts,
                tx_converter,
                sealed_block,
                txhash_cache,
            )
            .into_iter()
            .map(|transaction| FlashblockStreamEvent::Transaction { block_number, transaction }),
        );

        events
    }

    fn collect_transactions(
        block: &RecoveredBlock<N::Block>,
        filter: &FlashblocksFilter,
        receipts: &[N::Receipt],
        tx_converter: &Eth::RpcConvert,
        sealed_block: &SealedBlock<N::Block>,
        txhash_cache: &Cache<TxHash, ()>,
    ) -> Vec<EnrichedTxItem<Eth::RpcConvert>> {
        block
            .transactions_with_sender()
            .enumerate()
            .filter_map(|(idx, (sender, tx))| {
                let tx_hash = *tx.tx_hash();
                if txhash_cache.get(&tx_hash).is_some() {
                    return None;
                }
                let Some(receipt) = receipts.get(idx) else {
                    warn!(target: "xlayer::flashblocks", "failed to collect transaction idx {idx}, missing receipt");
                    return None;
                };

                if filter.requires_address_filtering() {
                    let matches_filter = Self::is_address_in_transaction(
                        *sender,
                        tx,
                        Some(receipt),
                        &filter.sub_tx_filter.subscribe_addresses,
                    );
                    if !matches_filter {
                        return None;
                    }
                }
                txhash_cache.insert(tx_hash, ());

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

        let recovered = Recovered::new_unchecked(ctx.tx.clone(), ctx.sender);

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

        let (gas_used, next_log_index) =
            calculate_gas_used_and_next_log_index(ctx.idx as u64, receipts);

        let receipt_input = ConvertReceiptInput {
            receipt: receipt.clone(),
            tx: Recovered::new_unchecked(ctx.tx, ctx.sender),
            gas_used: receipt.cumulative_gas_used() - gas_used,
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
        addresses: &HashSet<Address>,
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
async fn pipe_from_flashblocks_stream<N, Eth, FbSt>(
    sink: SubscriptionSink,
    mut fb_stream: FbSt,
) -> Result<(), ErrorObject<'static>>
where
    N: NodePrimitives,
    N::BlockHeader: alloy_consensus::BlockHeader,
    Eth: EthApiTypes + RpcNodeCore<Primitives = N> + 'static,
    Eth::RpcConvert: RpcConvert<Primitives = N> + Clone,
    FbSt: Stream<Item = FlashblockItem<N, Eth::RpcConvert>> + Unpin,
{
    loop {
        tokio::select! {
            _ = sink.closed() => {
                // connection dropped
                break Ok(())
            },
            maybe_fb_item = fb_stream.next() => {
                let Some(item) = maybe_fb_item else {
                    // stream ended
                    break Ok(());
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
    Ok(Header::from_consensus(
        block.clone_sealed_header().into(),
        None,
        Some(U256::from(block.rlp_length())),
    ))
}
