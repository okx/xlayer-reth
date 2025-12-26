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
use crate::pubsub::{FlashblocksPubSubApiServer, FlashblocksPubSub, FlashblockSubscriptionKind, OpParams, FlashBlocksFilter, EnrichedFlashblock, EnrichedTransaction};

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
        let tx_converter = self.tx_converter.clone();

        let flashblocks_stream =
            WatchStream::new(pending_block_rx).filter_map(move |pending_block_opt| {
                let filter = filter.clone();
                let tx_converter = tx_converter.clone();
                async move {
                    pending_block_opt.and_then(|pending_block| {
                        Self::filter_and_enrich_flashblock(&pending_block, &filter, &tx_converter)
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
        tx_converter: &Eth::RpcConvert,
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
