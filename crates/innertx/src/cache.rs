use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, RwLock},
};

use alloy_primitives::{TxHash, B256};
use eyre::eyre;

use crate::innertx_inspector::InternalTransaction;

/// Per-transaction internal transaction traces for a block.
/// Each entry corresponds positionally to the transaction at the same index.
pub type InnerTxTraces = Vec<Vec<InternalTransaction>>;

const DEFAULT_CONFIRM_BLOCK_CACHE_SIZE: usize = 50;

/// Shared, thread-safe cache of internal transaction traces for flashblock
/// pending/confirmed sequences.
#[derive(Clone, Debug)]
pub struct FlashblocksInnerTxCache {
    inner: Arc<RwLock<CacheInner>>,
}

#[derive(Debug, Default)]
struct CacheInner {
    blocks: BTreeMap<u64, BlockEntry>,
}

#[derive(Debug, Clone)]
struct BlockEntry {
    block_hash: B256,
    traces: InnerTxTraces,
    tx_index: HashMap<TxHash, usize>,
}

impl CacheInner {
    fn flush_up_to(&mut self, block_number: u64) {
        self.blocks = self.blocks.split_off(&(block_number + 1));
    }

    fn insert(
        &mut self,
        height: u64,
        block_hash: B256,
        delta_tx_hashes: Vec<TxHash>,
        delta_traces: InnerTxTraces,
    ) -> eyre::Result<()> {
        if self.blocks.len() >= DEFAULT_CONFIRM_BLOCK_CACHE_SIZE
            && !self.blocks.contains_key(&height)
        {
            return Err(eyre!(
                "innertx cache at max capacity ({DEFAULT_CONFIRM_BLOCK_CACHE_SIZE}), cannot insert block: {height}"
            ));
        }

        let entry = self.blocks.entry(height).or_insert_with(|| BlockEntry {
            block_hash,
            traces: Vec::new(),
            tx_index: HashMap::new(),
        });
        // Update hash to latest pending hash
        entry.block_hash = block_hash;
        // Append delta traces and build tx index
        let base = entry.traces.len();
        for (i, tx_hash) in delta_tx_hashes.iter().enumerate() {
            entry.tx_index.insert(*tx_hash, base + i);
        }
        entry.traces.extend(delta_traces);
        Ok(())
    }
}

impl FlashblocksInnerTxCache {
    pub fn new() -> Self {
        Self { inner: Arc::new(RwLock::new(CacheInner::default())) }
    }

    /// Appends delta innertx traces for a block, updating the block hash.
    pub fn insert(
        &self,
        height: u64,
        block_hash: B256,
        delta_tx_hashes: Vec<TxHash>,
        delta_traces: InnerTxTraces,
    ) -> eyre::Result<()> {
        self.inner.write().expect("innertx cache lock poisoned").insert(
            height,
            block_hash,
            delta_tx_hashes,
            delta_traces,
        )
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
    pub fn get_raw_block_traces(&self, block_hash: &B256) -> Option<InnerTxTraces> {
        let guard = self.inner.read().expect("innertx cache lock poisoned");
        let entry = guard.blocks.values().find(|e| &e.block_hash == block_hash)?;
        Some(entry.traces.clone())
    }

    /// Returns `true` if innertx traces for the given block hash are cached.
    pub fn has_block(&self, block_hash: &B256) -> bool {
        let guard = self.inner.read().expect("innertx cache lock poisoned");
        guard.blocks.values().any(|e| e.block_hash == *block_hash)
    }

    /// Returns the raw traces for a block number (for canonical indexing).
    pub fn get_raw_traces_by_number(&self, block_number: u64) -> Option<InnerTxTraces> {
        let guard = self.inner.read().expect("innertx cache lock poisoned");
        guard.blocks.get(&block_number).map(|e| e.traces.clone())
    }

    /// Clears all entries.
    pub fn clear(&self) {
        self.inner.write().expect("innertx cache lock poisoned").blocks.clear();
    }

    /// Evicts all cached entries at or below the given block number.
    pub fn evict_up_to(&self, block_number: u64) {
        self.inner.write().expect("innertx cache lock poisoned").flush_up_to(block_number);
    }
}

impl Default for FlashblocksInnerTxCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Async innertx computation for flashblocks validator
// ---------------------------------------------------------------------------

use alloy_consensus::BlockHeader as _;
use alloy_evm::block::BlockExecutor;
use reth_evm::ConfigureEvm;
use reth_primitives_traits::{
    transaction::TxHashRef, BlockBody as _, NodePrimitives, RecoveredBlock,
};
use reth_revm::{cached::CachedReads, database::StateProviderDatabase};
use reth_storage_api::StateProvider;
use reth_tracing::tracing::{debug, warn};
use revm_database::states::State;

use crate::innertx_inspector::TraceCollector;

/// Spawns incremental innertx computation on a blocking thread.
///
/// Replays the full block but only keeps innertx traces for transactions
/// after `prefix_tx_count` (the delta since the previous flashblock).
/// Uses `CachedReads` from the validator's execution as a warm read cache.
///
/// Results are appended to the cache entry for the block number.
pub fn spawn_compute_and_cache_innertx<N, E>(
    cache: FlashblocksInnerTxCache,
    evm_config: E,
    block: RecoveredBlock<N::Block>,
    state_provider: reth_storage_api::StateProviderBox,
    cached_reads: CachedReads,
    prefix_tx_count: usize,
) -> tokio::task::JoinHandle<()>
where
    N: NodePrimitives + 'static,
    E: ConfigureEvm<Primitives = N> + Send + Sync + 'static,
{
    tokio::task::spawn_blocking(move || {
        let block_hash = block.hash();
        let block_number = block.header().number();
        let total_txs = block.body().transactions().len();
        let delta_tx_count = total_txs.saturating_sub(prefix_tx_count);

        // Derive delta tx hashes from the block body.
        let delta_tx_hashes = block
            .body()
            .transactions()
            .iter()
            .skip(prefix_tx_count)
            .map(|tx| *tx.tx_hash())
            .collect();

        if delta_tx_count == 0 {
            // No new transactions — just update the block hash.
            if let Err(err) = cache.insert(block_number, block_hash, Vec::new(), Vec::new()) {
                warn!(target: "xlayer::flashblocks::innertx", %err, "Failed to update innertx cache");
            }
            return;
        }

        match replay_delta_innertx(
            &evm_config,
            &block,
            state_provider.as_ref(),
            cached_reads,
            prefix_tx_count,
        ) {
            Ok(traces) => {
                if let Err(err) = cache.insert(block_number, block_hash, delta_tx_hashes, traces) {
                    warn!(target: "xlayer::flashblocks::innertx", %err, "Failed to append innertx to cache");
                } else {
                    debug!(
                        target: "xlayer::flashblocks::innertx",
                        block_number,
                        ?block_hash,
                        delta_tx_count,
                        "Computed and cached delta innertx"
                    );
                }
            }
            Err(err) => {
                warn!(
                    target: "xlayer::flashblocks::innertx",
                    %err,
                    block_number,
                    delta_tx_count,
                    "Failed to compute delta innertx"
                );
            }
        }
    })
}

/// Replays only the **delta transactions** (skipping the prefix) with a
/// [`TraceCollector`] inspector. The state provider must be at the point
/// where prefix txs have already been applied (pending sequence hash for
/// incremental builds, parent hash for fresh builds).
///
/// `CachedReads` from the validator's execution provides a warm read cache.
fn replay_delta_innertx<E, N>(
    evm_config: &E,
    block: &RecoveredBlock<N::Block>,
    state_provider: &dyn StateProvider,
    mut cached_reads: CachedReads,
    prefix_tx_count: usize,
) -> eyre::Result<InnerTxTraces>
where
    E: ConfigureEvm<Primitives = N> + Send + Sync + 'static,
    N: NodePrimitives + 'static,
{
    // Only the delta transactions (skip prefix).
    let delta_txs: Vec<_> = block.transactions_recovered().skip(prefix_tx_count).collect();

    if delta_txs.is_empty() {
        return Ok(Vec::new());
    }

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
    let _output = executor.execute_block(delta_txs.into_iter())?;

    Ok(inspector.get())
}
