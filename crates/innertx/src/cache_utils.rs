use alloy_primitives::{BlockHash, TxHash};
use eyre::Report;
use lru_cache::LruCache;
use once_cell::sync::OnceCell;

use crate::innertx_inspector::InternalTransaction;
use std::sync::Mutex;

// Cache for BlockTable: block_hash -> Vec<TxHash>
static BLOCK_CACHE: OnceCell<Mutex<LruCache<Vec<u8>, Vec<TxHash>>>> = OnceCell::new();

// Cache for TxTable: tx_hash -> Vec<InternalTransaction>
static TX_CACHE: OnceCell<Mutex<LruCache<Vec<u8>, Vec<InternalTransaction>>>> = OnceCell::new();

const MAX_TX_CACHE_SIZE: usize = 10_000;
const MAX_BLOCK_CACHE_SIZE: usize = 500;

pub fn initialize_inner_tx_cache() -> Result<(), Report> {
    let block_cache = LruCache::new(MAX_BLOCK_CACHE_SIZE);
    let block_cache_mutex = Mutex::new(block_cache);
    let block_set_result = BLOCK_CACHE.set(block_cache_mutex);
    if block_set_result.is_err() {
        return Err(Report::msg("block_cache was initialized more than once"));
    }

    let tx_cache = LruCache::new(MAX_TX_CACHE_SIZE);
    let tx_cache_mutex = Mutex::new(tx_cache);
    let tx_set_result = TX_CACHE.set(tx_cache_mutex);
    if tx_set_result.is_err() {
        return Err(Report::msg("tx_cache was initialized more than once"));
    }

    Ok(())
}

pub fn write_block_cache(key: Vec<u8>, tx_hashes: Vec<TxHash>) -> Result<(), Report> {
    let block_cache =
        BLOCK_CACHE.get().ok_or_else(|| Report::msg("BLOCK_CACHE not initialized"))?;
    let mut block_cache = block_cache
        .lock()
        .map_err(|e| Report::msg(format!("Failed to acquire BLOCK_CACHE lock: {e}")))?;

    block_cache.insert(key, tx_hashes);
    Ok(())
}
pub fn write_tx_cache(key: Vec<u8>, inner_txs: Vec<InternalTransaction>) -> Result<(), Report> {
    let tx_cache = TX_CACHE.get().ok_or_else(|| Report::msg("TX_CACHE not initialized"))?;
    let mut tx_cache = tx_cache
        .lock()
        .map_err(|e| Report::msg(format!("Failed to acquire TX_CACHE lock: {e}")))?;

    tx_cache.insert(key, inner_txs);
    Ok(())
}

pub fn read_tx_cache(tx_hash: TxHash) -> Result<Vec<InternalTransaction>, Report> {
    let tx_cache = TX_CACHE.get().ok_or_else(|| Report::msg("TX_CACHE not initialized"))?;
    let mut tx_cache = tx_cache
        .lock()
        .map_err(|e| Report::msg(format!("Failed to acquire TX_CACHE lock: {e}")))?;

    if let Some(inner_txs) = tx_cache.get_mut(&tx_hash.to_vec()) {
        return Ok(inner_txs.clone());
    }

    Ok(Vec::<InternalTransaction>::default())
}

pub fn read_block_cache(block_hash: BlockHash) -> Result<Vec<TxHash>, Report> {
    let block_cache =
        BLOCK_CACHE.get().ok_or_else(|| Report::msg("BLOCK_CACHE not initialized"))?;
    let mut block_cache = block_cache
        .lock()
        .map_err(|e| Report::msg(format!("Failed to acquire BLOCK_CACHE lock: {e}")))?;

    if let Some(tx_hashes) = block_cache.get_mut(&block_hash.to_vec()) {
        return Ok(tx_hashes.clone());
    }

    Ok(Vec::<TxHash>::default())
}
