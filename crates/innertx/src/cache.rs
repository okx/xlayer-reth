use eyre::eyre;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, RwLock},
};
use tracing::*;

use alloy_consensus::BlockHeader as _;
use alloy_evm::block::BlockExecutor;
use alloy_primitives::{TxHash, B256};

use crate::innertx_inspector::{InternalTransaction, TraceCollector};

use reth_evm::ConfigureEvm;
use reth_primitives_traits::{
    transaction::TxHashRef, BlockBody as _, NodePrimitives, RecoveredBlock,
};
use reth_revm::{cached::CachedReads, database::StateProviderDatabase};
use reth_storage_api::StateProvider;
use revm_database::states::State;

/// Per-transaction internal transaction traces for a block.
/// Each entry corresponds positionally to the transaction at the same index.
pub type InnerTxTraces = Vec<Vec<InternalTransaction>>;

const DEFAULT_CONFIRM_BLOCK_CACHE_SIZE: usize = 50;

#[derive(Debug, Clone, Default)]
struct BlockEntry {
    block_hash: B256,
    traces: InnerTxTraces,
    tx_index: HashMap<TxHash, usize>,
}

#[derive(Debug, Default)]
struct CacheInner {
    blocks: BTreeMap<u64, BlockEntry>,
}

/// Shared, thread-safe cache of internal transaction traces for flashblock
/// pending/confirmed sequences.
#[derive(Clone, Debug, Default)]
pub struct FlashblocksInnerTxCache {
    inner: Arc<RwLock<CacheInner>>,
}

impl FlashblocksInnerTxCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the innertx traces for a block.
    pub fn insert(
        &self,
        height: u64,
        block_hash: B256,
        tx_hashes: Vec<TxHash>,
        traces: InnerTxTraces,
    ) -> eyre::Result<()> {
        let mut guard = self.inner.write().expect("innertx cache lock poisoned");

        if guard.blocks.len() >= DEFAULT_CONFIRM_BLOCK_CACHE_SIZE
            && !guard.blocks.contains_key(&height)
        {
            return Err(eyre!(
                    "innertx cache at max capacity ({DEFAULT_CONFIRM_BLOCK_CACHE_SIZE}), cannot insert block: {height}"
                ));
        }
        guard.blocks.insert(
            height,
            BlockEntry {
                block_hash,
                traces,
                tx_index: tx_hashes.iter().enumerate().map(|(i, tx_hash)| (*tx_hash, i)).collect(),
            },
        );
        Ok(())
    }

    /// Single-tx lookup by hash.
    pub fn get_tx_traces(&self, tx_hash: &TxHash) -> Option<Vec<InternalTransaction>> {
        let guard = self.inner.read().expect("innertx cache lock poisoned");
        for entry in guard.blocks.values() {
            if let Some(&idx) = entry.tx_index.get(tx_hash) {
                return entry.traces.get(idx).cloned();
            }
        }
        None
    }

    /// All traces for a block by hash, keyed by tx hash string.
    pub fn get_block_traces_by_hash(
        &self,
        block_hash: &B256,
    ) -> Option<HashMap<String, Vec<InternalTransaction>>> {
        let guard = self.inner.read().expect("innertx cache lock poisoned");
        let entry = guard.blocks.values().find(|e| &e.block_hash == block_hash)?;
        let result: HashMap<String, Vec<InternalTransaction>> = entry
            .tx_index
            .iter()
            .filter_map(|(tx_hash, &idx)| {
                entry.traces.get(idx).map(|t| (tx_hash.to_string(), t.clone()))
            })
            .collect();
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    /// Raw positional traces for subscription enrichment.
    pub fn get_raw_block_traces_by_hash(&self, block_hash: &B256) -> Option<InnerTxTraces> {
        let guard = self.inner.read().expect("innertx cache lock poisoned");
        let entry = guard.blocks.values().find(|e| &e.block_hash == block_hash)?;
        Some(entry.traces.clone())
    }

    /// Returns `true` if innertx traces for the given block hash are cached.
    pub fn has_block(&self, block_hash: &B256) -> bool {
        let guard = self.inner.read().expect("innertx cache lock poisoned");
        guard.blocks.values().any(|e| e.block_hash == *block_hash)
    }

    /// Clears all entries.
    pub fn clear(&self) {
        self.inner.write().expect("innertx cache lock poisoned").blocks.clear();
    }

    /// Flushes all entries with block number <= `canonical_number`.
    pub fn flush_up_to_height(&self, canon_height: u64) {
        let mut guard = self.inner.write().expect("innertx cache lock poisoned");
        guard.blocks = guard.blocks.split_off(&(canon_height + 1));
    }
}

/// Spawns innertx computation on a blocking thread.
///
/// Replays the full block and keeps all innertx traces.
/// Uses `CachedReads` from the validator's execution as a warm read cache.
///
/// Results are appended to the cache entry for the block number.
pub fn spawn_compute_and_cache_innertx<N, E>(
    cache: FlashblocksInnerTxCache,
    evm_config: E,
    block: RecoveredBlock<N::Block>,
    state_provider: reth_storage_api::StateProviderBox,
    cached_reads: CachedReads,
) -> tokio::task::JoinHandle<()>
where
    N: NodePrimitives + 'static,
    E: ConfigureEvm<Primitives = N> + Send + Sync + 'static,
{
    tokio::task::spawn_blocking(move || {
        let block_hash = block.hash();
        let block_number = block.header().number();
        let tx_count = block.body().transactions().len();

        // Derive delta tx hashes from the block body.
        let tx_hashes = block.body().transactions().iter().map(|tx| *tx.tx_hash()).collect();

        if tx_count == 0 {
            // No new transactions — just update the block hash.
            if let Err(err) = cache.insert(block_number, block_hash, Vec::new(), Vec::new()) {
                warn!(target: "xlayer::flashblocks::innertx", %err, "Failed to update innertx cache");
            }
            return;
        }

        match replay_innertx(&evm_config, &block, state_provider.as_ref(), cached_reads) {
            Ok(traces) => {
                if let Err(err) = cache.insert(block_number, block_hash, tx_hashes, traces) {
                    warn!(target: "xlayer::flashblocks::innertx", %err, "Failed to append innertx to cache");
                } else {
                    debug!(
                        target: "xlayer::flashblocks::innertx",
                        block_number,
                        ?block_hash,
                        tx_count,
                        "Computed and cached delta innertx"
                    );
                }
            }
            Err(err) => {
                warn!(
                    target: "xlayer::flashblocks::innertx",
                    %err,
                    block_number,
                    tx_count,
                    "Failed to compute innertx"
                );
            }
        }
    })
}

/// Replays all block transactions with a [`TraceCollector`] inspector.
/// The state provider must be at `parent_hash` (start of block).
///
/// `CachedReads` from the validator's execution provides a warm read cache
/// so prefix transaction re-execution is fast (cache hits).
fn replay_innertx<E, N>(
    evm_config: &E,
    block: &RecoveredBlock<N::Block>,
    state_provider: &dyn StateProvider,
    mut cached_reads: CachedReads,
) -> eyre::Result<InnerTxTraces>
where
    E: ConfigureEvm<Primitives = N> + Send + Sync + 'static,
    N: NodePrimitives + 'static,
{
    let txs: Vec<_> = block.transactions_recovered().collect();

    // Layer CachedReads on top of the state provider for warm reads
    let db = StateProviderDatabase::new(state_provider);
    let cached_db = cached_reads.as_db_mut(db);
    let mut state = State::builder()
        .with_database(cached_db)
        .with_bundle_update()
        .without_state_clear()
        .build();

    let mut inspector = TraceCollector::default();
    let evm_env = evm_config.evm_env(block.header())?;
    let evm = evm_config.evm_with_env_and_inspector(&mut state, evm_env, &mut inspector);
    let block_ctx = evm_config.context_for_block(block)?;
    let mut executor = evm_config.create_executor(evm, block_ctx);
    executor.set_state_hook(None);

    // Replay transactions with innertx inspector
    let _output = executor.execute_block(txs.into_iter())?;
    Ok(inspector.get())
}
