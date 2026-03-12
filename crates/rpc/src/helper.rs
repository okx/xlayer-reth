use alloy_consensus::TxReceipt;
use alloy_primitives::B256;
use alloy_rpc_types_eth::TransactionInfo;
use op_alloy_network::Optimism;

use reth_optimism_primitives::OpPrimitives;
use reth_primitives_traits::{Recovered, SignerRecoverable, TransactionMeta};
use reth_rpc_convert::{transaction::ConvertReceiptInput, RpcConvert, RpcTransaction};
use reth_rpc_eth_api::{EthApiTypes, RpcBlock, RpcReceipt};
use reth_rpc_eth_types::{block::BlockAndReceipts, utils::calculate_gas_used_and_next_log_index};

use xlayer_flashblocks::cache::CachedTxInfo;

/// Converter for `TransactionMeta`
pub(crate) fn build_tx_meta(
    bar: &BlockAndReceipts<OpPrimitives>,
    tx_hash: B256,
    index: u64,
) -> TransactionMeta {
    TransactionMeta {
        tx_hash,
        index,
        block_hash: bar.block.hash(),
        block_number: bar.block.number(),
        base_fee: bar.block.base_fee_per_gas(),
        excess_blob_gas: bar.block.excess_blob_gas(),
        timestamp: bar.block.timestamp(),
    }
}

/// Converter for `TransactionInfo`
pub(crate) fn build_tx_info(
    bar: &BlockAndReceipts<OpPrimitives>,
    tx_hash: B256,
    index: u64,
) -> TransactionInfo {
    TransactionInfo {
        hash: Some(tx_hash),
        index: Some(index),
        block_hash: Some(bar.block.hash()),
        block_number: Some(bar.block.number()),
        base_fee: bar.block.base_fee_per_gas(),
    }
}

/// Converts a `BlockAndReceipts` into an RPC block.
pub(crate) fn to_rpc_block<Eth: EthApiTypes<NetworkTypes = Optimism>>(
    bar: &BlockAndReceipts<OpPrimitives>,
    full: bool,
    converter: &Eth::RpcConvert,
) -> Result<RpcBlock<Optimism>, Eth::Error>
where
    Eth::RpcConvert: RpcConvert<Primitives = OpPrimitives>,
    Eth::Error: From<<Eth::RpcConvert as RpcConvert>::Error>,
{
    Ok(bar.block.clone_into_rpc_block(
        full.into(),
        |tx, tx_info| converter.fill(tx, tx_info),
        |header, size| converter.convert_header(header, size),
    )?)
}

/// Converts all receipts from a `BlockAndReceipts` into RPC receipts.
pub(crate) fn to_block_receipts<Eth: EthApiTypes<NetworkTypes = Optimism>>(
    bar: &BlockAndReceipts<OpPrimitives>,
    converter: &Eth::RpcConvert,
) -> Result<Vec<RpcReceipt<Optimism>>, Eth::Error>
where
    Eth::RpcConvert: RpcConvert<Primitives = OpPrimitives>,
    Eth::Error: From<<Eth::RpcConvert as RpcConvert>::Error>,
{
    let txs = bar.block.body().transactions();
    let senders = bar.block.senders();
    let receipts = bar.receipts.as_ref();

    let mut prev_cumulative_gas = 0u64;
    let mut next_log_index = 0usize;

    let inputs = txs
        .iter()
        .zip(senders.iter())
        .zip(receipts.iter())
        .enumerate()
        .map(|(idx, ((tx, sender), receipt))| {
            let gas_used = receipt.cumulative_gas_used() - prev_cumulative_gas;
            prev_cumulative_gas = receipt.cumulative_gas_used();
            let logs_len = receipt.logs().len();

            let meta = build_tx_meta(bar, tx.tx_hash(), idx as u64);
            let input = ConvertReceiptInput {
                tx: Recovered::new_unchecked(tx, *sender),
                gas_used,
                next_log_index,
                meta,
                receipt: receipt.clone(),
            };

            next_log_index += logs_len;

            input
        })
        .collect::<Vec<_>>();

    Ok(converter.convert_receipts(inputs)?)
}

/// Converts a single `CachedTxInfo` into an RPC receipt, using the full block receipts
/// from `BlockAndReceipts` to correctly calculate gas used and log index offsets.
pub(crate) fn to_rpc_receipt<Eth: EthApiTypes<NetworkTypes = Optimism>>(
    info: &CachedTxInfo<OpPrimitives>,
    bar: &BlockAndReceipts<OpPrimitives>,
    converter: &Eth::RpcConvert,
) -> Result<RpcReceipt<Optimism>, Eth::Error>
where
    Eth::RpcConvert: RpcConvert<Primitives = OpPrimitives>,
    Eth::Error: From<<Eth::RpcConvert as RpcConvert>::Error>,
{
    let (prev_cumulative_gas, next_log_index) =
        calculate_gas_used_and_next_log_index(info.tx_index, bar.receipts.as_ref());

    let meta = build_tx_meta(bar, info.tx.tx_hash(), info.tx_index);
    let recovered = info.tx.try_into_recovered_unchecked()?;
    Ok(converter
        .convert_receipts(vec![ConvertReceiptInput {
            tx: recovered.as_recovered_ref(),
            gas_used: info.receipt.cumulative_gas_used() - prev_cumulative_gas,
            next_log_index,
            meta,
            receipt: info.receipt.clone(),
        }])?
        .pop()
        .unwrap())
}

/// Converts a `CachedTxInfo` and `BlockAndReceipts` into an RPC transaction.
pub(crate) fn to_rpc_transaction<Eth: EthApiTypes<NetworkTypes = Optimism>>(
    info: &CachedTxInfo<OpPrimitives>,
    bar: &BlockAndReceipts<OpPrimitives>,
    converter: &Eth::RpcConvert,
) -> Result<RpcTransaction<Optimism>, Eth::Error>
where
    Eth::RpcConvert: RpcConvert<Primitives = OpPrimitives>,
    Eth::Error: From<<Eth::RpcConvert as RpcConvert>::Error>,
{
    let tx_info = build_tx_info(bar, info.tx.tx_hash(), info.tx_index);
    Ok(converter.fill(info.tx.try_into_recovered_unchecked()?, tx_info)?)
}

/// Converts a `BlockAndReceipts` and transaction index into an RPC transaction.
pub(crate) fn to_rpc_transaction_from_bar_and_index<Eth: EthApiTypes<NetworkTypes = Optimism>>(
    bar: &BlockAndReceipts<OpPrimitives>,
    index: usize,
    converter: &Eth::RpcConvert,
) -> Result<Option<RpcTransaction<Optimism>>, Eth::Error>
where
    Eth::RpcConvert: RpcConvert<Primitives = OpPrimitives>,
    Eth::Error: From<<Eth::RpcConvert as RpcConvert>::Error>,
{
    if let Some((signer, tx)) = bar.block.transactions_with_sender().nth(index) {
        let tx_info = build_tx_info(bar, tx.tx_hash(), index as u64);
        return Ok(Some(converter.fill(tx.clone().with_signer(*signer), tx_info)?));
    }
    Ok(None)
}
