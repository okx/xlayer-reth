use crate::innertx_inspector::InternalTransaction;
use alloy_primitives::{BlockHash, TxHash, B256};
use eyre::Report;
use moka::sync::Cache;
use once_cell::sync::OnceCell;

// Cache for BlockTable: block_hash -> Vec<TxHash>
static BLOCK_CACHE: OnceCell<Cache<B256, Vec<TxHash>>> = OnceCell::new();

// Cache for TxTable: tx_hash -> Vec<InternalTransaction>
static TX_CACHE: OnceCell<Cache<B256, Vec<InternalTransaction>>> = OnceCell::new();

const MAX_TX_CACHE_SIZE: u64 = 10_000;
const MAX_BLOCK_CACHE_SIZE: u64 = 500;

pub fn initialize_inner_tx_cache() -> Result<(), Report> {
    let block_cache = Cache::builder().max_capacity(MAX_BLOCK_CACHE_SIZE).build();
    let block_set_result = BLOCK_CACHE.set(block_cache);
    if block_set_result.is_err() {
        return Err(Report::msg("block_cache was initialized more than once"));
    }

    let tx_cache = Cache::builder().max_capacity(MAX_TX_CACHE_SIZE).build();
    let tx_set_result = TX_CACHE.set(tx_cache);
    if tx_set_result.is_err() {
        return Err(Report::msg("tx_cache was initialized more than once"));
    }

    Ok(())
}

pub fn write_block_cache(key: B256, tx_hashes: Vec<TxHash>) -> Result<(), Report> {
    let block_cache =
        BLOCK_CACHE.get().ok_or_else(|| Report::msg("BLOCK_CACHE not initialized"))?;

    block_cache.insert(key, tx_hashes);
    Ok(())
}
pub fn write_tx_cache(key: B256, inner_txs: Vec<InternalTransaction>) -> Result<(), Report> {
    let tx_cache = TX_CACHE.get().ok_or_else(|| Report::msg("TX_CACHE not initialized"))?;

    tx_cache.insert(key, inner_txs);
    Ok(())
}

pub fn read_tx_cache(tx_hash: TxHash) -> Result<Vec<InternalTransaction>, Report> {
    let tx_cache = TX_CACHE.get().ok_or_else(|| Report::msg("TX_CACHE not initialized"))?;

    if let Some(inner_txs) = tx_cache.get(&tx_hash) {
        return Ok(inner_txs.clone());
    }

    Ok(Vec::<InternalTransaction>::default())
}

pub fn read_block_cache(block_hash: BlockHash) -> Result<Vec<TxHash>, Report> {
    let block_cache =
        BLOCK_CACHE.get().ok_or_else(|| Report::msg("BLOCK_CACHE not initialized"))?;

    if let Some(tx_hashes) = block_cache.get(&block_hash) {
        return Ok(tx_hashes.clone());
    }

    Ok(Vec::<TxHash>::default())
}
