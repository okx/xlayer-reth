use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use alloy_primitives::{TxHash, B256};

use crate::innertx_inspector::InternalTransaction;

/// Per-transaction internal transaction traces for a block.
/// Each entry corresponds positionally to the transaction at the same index.
pub type InnerTxTraces = Vec<Vec<InternalTransaction>>;

/// Shared, thread-safe cache of internal transaction traces for flashblock
/// pending/confirmed sequences. Keyed by block hash for O(1) lookups.
#[derive(Clone, Debug)]
pub struct FlashblocksInnerTxCache {
    inner: Arc<RwLock<CacheInner>>,
}

#[derive(Debug, Default)]
struct CacheInner {
    blocks: HashMap<B256, (u64, InnerTxTraces)>,
    number_index: HashMap<u64, B256>,
    tx_index: HashMap<TxHash, (B256, usize)>,
}

impl CacheInner {
    fn remove_block(&mut self, block_hash: B256, block_number: u64) {
        self.blocks.remove(&block_hash);
        self.number_index.remove(&block_number);
        self.tx_index.retain(|_, (bh, _)| *bh != block_hash);
    }

    fn evict_up_to(&mut self, block_number: u64) {
        let to_remove: Vec<(u64, B256)> = self
            .number_index
            .iter()
            .filter(|(num, _)| **num <= block_number)
            .map(|(num, hash)| (*num, *hash))
            .collect();
        for (num, hash) in to_remove {
            self.remove_block(hash, num);
        }
    }
}

const MAX_CACHE_BLOCKS: usize = 64;

impl FlashblocksInnerTxCache {
    pub fn new() -> Self {
        Self { inner: Arc::new(RwLock::new(CacheInner::default())) }
    }

    pub fn insert(
        &self,
        block_hash: B256,
        block_number: u64,
        tx_hashes: &[TxHash],
        traces: InnerTxTraces,
    ) {
        let mut guard = self.inner.write().expect("innertx cache lock poisoned");
        while guard.blocks.len() >= MAX_CACHE_BLOCKS {
            if let Some((&oldest_num, &oldest_hash)) =
                guard.number_index.iter().min_by_key(|(n, _)| **n)
            {
                guard.remove_block(oldest_hash, oldest_num);
            } else {
                break;
            }
        }
        for (idx, tx_hash) in tx_hashes.iter().enumerate() {
            guard.tx_index.insert(*tx_hash, (block_hash, idx));
        }
        guard.number_index.insert(block_number, block_hash);
        guard.blocks.insert(block_hash, (block_number, traces));
    }

    /// Single-tx lookup by hash.
    pub fn get_tx_traces(&self, tx_hash: &TxHash) -> Option<Vec<InternalTransaction>> {
        let guard = self.inner.read().expect("innertx cache lock poisoned");
        let (block_hash, tx_idx) = guard.tx_index.get(tx_hash)?;
        let (_, traces) = guard.blocks.get(block_hash)?;
        traces.get(*tx_idx).cloned()
    }

    /// All traces for a block by hash, keyed by tx hash string.
    pub fn get_block_traces_by_hash(
        &self,
        block_hash: &B256,
    ) -> Option<HashMap<String, Vec<InternalTransaction>>> {
        let guard = self.inner.read().expect("innertx cache lock poisoned");
        let (_, traces) = guard.blocks.get(block_hash)?;
        let result: HashMap<String, Vec<InternalTransaction>> = guard
            .tx_index
            .iter()
            .filter(|(_, (bh, _))| bh == block_hash)
            .filter_map(|(tx_hash, (_, idx))| {
                traces.get(*idx).map(|t| (tx_hash.to_string(), t.clone()))
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
        guard.blocks.get(block_hash).map(|(_, traces)| traces.clone())
    }

    /// Returns `true` if innertx traces for the given block hash are cached.
    pub fn has_block(&self, block_hash: &B256) -> bool {
        self.inner.read().expect("innertx cache lock poisoned").blocks.contains_key(block_hash)
    }

    pub fn evict_up_to(&self, block_number: u64) {
        self.inner.write().expect("innertx cache lock poisoned").evict_up_to(block_number);
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
use reth_revm::database::StateProviderDatabase;
use reth_storage_api::StateProvider;
use reth_tracing::tracing::{debug, warn};
use revm_database::states::State;

use crate::innertx_inspector::TraceCollector;

/// Spawns innertx trace computation on a blocking thread and returns a handle.
///
/// Called from the flashblocks validator after block assembly. The caller
/// should `.await` the handle before committing the pending sequence so that
/// the innertx cache is populated atomically with the state cache update.
///
/// Errors are logged and swallowed — innertx failure must not block the
/// flashblocks execution pipeline.
pub fn spawn_compute_and_cache_innertx<N, E>(
    cache: FlashblocksInnerTxCache,
    evm_config: E,
    block: RecoveredBlock<N::Block>,
    state_provider: reth_storage_api::StateProviderBox,
) -> tokio::task::JoinHandle<()>
where
    N: NodePrimitives + 'static,
    E: ConfigureEvm<Primitives = N> + Send + Sync + 'static,
{
    tokio::task::spawn_blocking(move || {
        if block.body().transactions().is_empty() {
            return;
        }

        match replay_block_innertx(&evm_config, &block, state_provider.as_ref()) {
            Ok(traces) => {
                let block_hash = block.hash();
                let block_number = block.header().number();
                let tx_hashes: Vec<TxHash> =
                    block.body().transactions().iter().map(|tx| *tx.tx_hash()).collect();

                cache.insert(block_hash, block_number, &tx_hashes, traces);
                debug!(
                    target: "xlayer::flashblocks::innertx",
                    block_number,
                    ?block_hash,
                    tx_count = tx_hashes.len(),
                    "Computed and cached innertx"
                );
            }
            Err(err) => {
                warn!(
                    target: "xlayer::flashblocks::innertx",
                    %err,
                    block_number = block.header().number(),
                    "Failed to compute innertx"
                );
            }
        }
    })
}

/// Replays a block with [`TraceCollector`] to capture internal transaction traces.
fn replay_block_innertx<E, N>(
    evm_config: &E,
    block: &RecoveredBlock<N::Block>,
    state_provider: &dyn StateProvider,
) -> eyre::Result<InnerTxTraces>
where
    E: ConfigureEvm<Primitives = N> + Send + Sync + 'static,
    N: NodePrimitives + 'static,
{
    let mut db = State::builder()
        .with_database(StateProviderDatabase::new(state_provider))
        .with_bundle_update()
        .without_state_clear()
        .build();

    let mut inspector = TraceCollector::default();
    let evm_env = evm_config.evm_env(block.header())?;
    let evm = evm_config.evm_with_env_and_inspector(&mut db, evm_env, &mut inspector);
    let block_ctx = evm_config.context_for_block(block)?;
    let mut executor = evm_config.create_executor(evm, block_ctx);

    executor.set_state_hook(None);
    let _output = executor.execute_block(block.transactions_recovered())?;

    Ok(inspector.get())
}
