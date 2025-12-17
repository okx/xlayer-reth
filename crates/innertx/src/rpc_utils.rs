use std::collections::HashMap;
use std::sync::Arc;

use alloy_consensus::transaction::TxHashRef;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{
    hex::{FromHex, FromHexError},
    FixedBytes, B256,
};
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
    types::{
        error::{INTERNAL_ERROR_CODE, INVALID_PARAMS_CODE},
        ErrorObjectOwned,
    },
};
use tokio::{
    select,
    task::spawn_blocking,
    time::{interval, sleep_until, Duration, Instant, MissedTickBehavior},
};

use reth_provider::TransactionsProvider;
use reth_rpc::RpcTypes;
use reth_rpc_eth_api::{
    helpers::{block::LoadBlock, EthCall},
    EthApiTypes,
};
use reth_storage_api::BlockIdReader;
use reth_tracing::tracing::debug;

use crate::{
    cache_utils::{read_block_cache, read_tx_cache},
    db_utils::{read_table_block, read_table_tx},
    innertx_inspector::InternalTransaction,
};

const TX_HASH_LENGTH: usize = 66;
const TIMEOUT_DURATION_S: u64 = 3;
const INTERVAL_DELAY_MS: u64 = 10;

fn string_to_b256(hex_str: String) -> Result<B256, FromHexError> {
    let hex = hex_str.strip_prefix("0x").unwrap_or(&hex_str);
    let fb: FixedBytes<32> = FixedBytes::from_hex(hex)?;
    Ok(B256::from(fb))
}

#[rpc(server, namespace = "eth", server_bounds(
    Net: 'static + RpcTypes,                     // Net itself needs no Serde
    <Net as RpcTypes>::TransactionRequest:
        serde::de::DeserializeOwned + serde::Serialize
))]
pub trait XlayerInnerTxExtApi<Net: RpcTypes> {
    #[method(name = "getInternalTransactions")]
    async fn get_internal_transactions(
        &self,
        tx_hash: String,
    ) -> RpcResult<Vec<InternalTransaction>>;

    #[method(name = "getBlockInternalTransactions")]
    async fn get_block_internal_transactions(
        &self,
        block_number: BlockNumberOrTag,
    ) -> RpcResult<HashMap<String, Vec<InternalTransaction>>>;
}

#[derive(Debug)]
pub struct XlayerInnerTxExt<T> {
    pub backend: Arc<T>,
}

#[async_trait]
impl<T, Net> XlayerInnerTxExtApiServer<Net> for XlayerInnerTxExt<T>
where
    T: EthCall + EthApiTypes<NetworkTypes = Net> + LoadBlock + Send + Sync + 'static,
    Net: RpcTypes + Send + Sync + 'static,
{
    async fn get_internal_transactions(
        &self,
        tx_hash: String,
    ) -> RpcResult<Vec<InternalTransaction>> {
        if tx_hash.len() != TX_HASH_LENGTH || !tx_hash.starts_with("0x") {
            return Err(ErrorObjectOwned::owned(
                INVALID_PARAMS_CODE,
                "Invalid transaction hash format",
                None::<()>,
            ));
        }

        let hash = string_to_b256(tx_hash.clone()).map_err(|_| {
            ErrorObjectOwned::owned(INVALID_PARAMS_CODE, "Invalid transaction hash", None::<()>)
        })?;

        if self.backend.local_pending_block().await.is_ok() {
            let read = read_tx_cache(hash);
            debug!(target: "xlayer::innertx::rpc_utils", "read {} from tx cache", hash.to_string());
            match read {
                Ok(result) if !result.is_empty() => {
                    return Ok(result);
                }
                Ok(_) => {
                    // Cache miss, fall through to DB reads for committed transactions
                }
                Err(_) => {
                    return Err(ErrorObjectOwned::owned(
                        INTERNAL_ERROR_CODE,
                        "Internal error reading transaction data",
                        None::<()>,
                    ))
                }
            }
        }

        match self.backend.provider().transaction_by_hash(hash) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(ErrorObjectOwned::owned(-32000, "Transaction not found", None::<()>));
            }
            Err(_) => return Err(ErrorObjectOwned::owned(-32603, "Internal error", None::<()>)),
        }

        let deadline = Instant::now() + Duration::from_secs(TIMEOUT_DURATION_S);
        let mut tick = interval(Duration::from_millis(INTERVAL_DELAY_MS));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            let read = spawn_blocking(move || read_table_tx(hash))
                .await
                .map_err(|_| ())
                .and_then(|r| r.map_err(|_| ()));

            match read {
                Ok(result) if !result.is_empty() => return Ok(result),
                Ok(_) => {}
                Err(_) => {
                    return Err(ErrorObjectOwned::owned(
                        -32603,
                        "Internal error reading transaction data",
                        None::<()>,
                    ))
                }
            }

            select! {
                _ = tick.tick() => {},
                _ = sleep_until(deadline) => {
                    return Err(ErrorObjectOwned::owned(
                        -32000,
                        "Timeout waiting for transaction data",
                        None::<()>,
                    ))
                }
            }
        }
    }

    async fn get_block_internal_transactions(
        &self,
        block_number: BlockNumberOrTag,
    ) -> RpcResult<HashMap<String, Vec<InternalTransaction>>> {
        if block_number == BlockNumberOrTag::Pending {
            let Some(pending_block) = self.backend.local_pending_block().await.map_err(|_| {
                ErrorObjectOwned::owned(
                    INTERNAL_ERROR_CODE,
                    "Internal error getting pending block",
                    None::<()>,
                )
            })?
            else {
                return Err(ErrorObjectOwned::owned(-32000, "Block not found", None::<()>));
            };

            let block_hash = pending_block.block.hash();
            if let Ok(txns) = read_block_cache(block_hash) {
                debug!(target: "xlayer::innertx::rpc_utils", "read {} from block cache", block_hash.to_string());
                let block_txs = if !txns.is_empty() {
                    txns
                } else {
                    // fall back to getting tx_hashes from pending block
                    pending_block
                        .block
                        .transactions_recovered()
                        .map(|tx| *tx.tx_hash())
                        .collect::<Vec<_>>()
                };

                // Read internal transactions from cache
                let mut result = HashMap::<String, Vec<InternalTransaction>>::default();
                for tx_hash in block_txs {
                    let internal_txs_result = read_tx_cache(tx_hash);

                    match internal_txs_result {
                        Ok(internal_txs) => {
                            result.insert(tx_hash.to_string(), internal_txs);
                        }
                        Err(_) => {
                            return Err(ErrorObjectOwned::owned(
                                INTERNAL_ERROR_CODE,
                                "Internal error reading transaction data",
                                None::<()>,
                            ));
                        }
                    }
                }
                return Ok(result);
            }
        }

        let hash: FixedBytes<32> = match self
            .backend
            .provider()
            .block_hash_for_id(block_number.into())
        {
            Ok(Some(hash)) => hash,
            Ok(None) => return Err(ErrorObjectOwned::owned(-32000, "Block not found", None::<()>)),
            Err(_) => return Err(ErrorObjectOwned::owned(-32603, "Internal error", None::<()>)),
        };

        let deadline = Instant::now() + Duration::from_secs(TIMEOUT_DURATION_S);
        let mut tick = interval(Duration::from_millis(INTERVAL_DELAY_MS));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        let block_txs = loop {
            let read = spawn_blocking(move || read_table_block(hash))
                .await
                .map_err(|_| ())
                .and_then(|r| r.map_err(|_| ()));

            match read {
                Ok(result) if !result.is_empty() => break result,
                Ok(_) => {}
                Err(_) => {
                    return Err(ErrorObjectOwned::owned(
                        -32603,
                        "Internal error reading block data",
                        None::<()>,
                    ))
                }
            }

            select! {
                _ = tick.tick() => {},
                _ = sleep_until(deadline) => {
                    return Err(ErrorObjectOwned::owned(
                        -32000,
                        "Timeout waiting for block data",
                        None::<()>,
                    ))
                }
            }
        };

        let mut result = HashMap::<String, Vec<InternalTransaction>>::default();

        for tx_hash in block_txs {
            let internal_txs_result = read_table_tx(tx_hash);
            if internal_txs_result.is_err() {
                return Err(ErrorObjectOwned::owned(
                    -32603,
                    "Internal error reading transaction data",
                    None::<()>,
                ));
            }

            result.insert(tx_hash.to_string(), internal_txs_result.unwrap());
        }

        Ok(result)
    }
}
