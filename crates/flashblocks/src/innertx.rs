use alloy_consensus::BlockHeader as _;
use alloy_evm::block::BlockExecutor;
use reth_evm::ConfigureEvm;
use reth_optimism_flashblocks::PendingFlashBlock;
use reth_primitives_traits::{BlockBody as _, NodePrimitives, RecoveredBlock};
use reth_revm::database::StateProviderDatabase;
use reth_storage_api::StateProviderFactory;
use revm_database::states::State;
use tracing::warn;
use xlayer_innertx::innertx_inspector::{InternalTransaction, TraceCollector};

/// Type used to carry innertx traces alongside a flashblock.
///
/// Each outer entry corresponds positionally to a transaction in the block;
/// the inner vector holds that transaction's internal traces.
pub type InnerTxTraces = Vec<Vec<InternalTransaction>>;

/// A pending flashblock paired with optional pre-computed innertx traces.
///
/// The reth fork at the current rev does not expose `FlashBlockExtension` on
/// `PendingFlashBlock`, so innertx is threaded to the subscription layer through
/// this side-channel wrapper instead. The enricher bridge task in `main.rs`
/// computes traces once per flashblock update and wraps each block here; the
/// subscription layer reads `innertx` back out when a subscriber requests it.
pub struct EnrichedFlashBlock<N: NodePrimitives> {
    /// The pending flashblock.
    pub block: PendingFlashBlock<N>,
    /// Pre-computed per-transaction innertx traces, if computed.
    pub innertx: Option<InnerTxTraces>,
}

impl<N: NodePrimitives> Clone for EnrichedFlashBlock<N> {
    fn clone(&self) -> Self {
        Self { block: self.block.clone(), innertx: self.innertx.clone() }
    }
}

/// Computes internal transaction traces for a block by replaying execution
/// with a [`TraceCollector`] inspector.
///
/// Implements the same replay logic as [`replay_and_index_block`] but returns
/// the traces in-memory instead of persisting them to the innertx database.
///
/// [`replay_and_index_block`]: xlayer_innertx::replay_utils::replay_and_index_block
pub struct InnerTxHook<P, E> {
    provider: P,
    evm_config: E,
}

impl<P, E> InnerTxHook<P, E> {
    /// Creates a new [`InnerTxHook`] with the given provider and EVM config.
    pub fn new(provider: P, evm_config: E) -> Self {
        Self { provider, evm_config }
    }
}

impl<P, E> InnerTxHook<P, E> {
    /// Computes innertx traces for the given block.
    ///
    /// Returns `None` if the block has no transactions or if replay fails.
    pub fn compute_innertx<N>(&self, block: &RecoveredBlock<N::Block>) -> Option<InnerTxTraces>
    where
        P: StateProviderFactory + Send + Sync + 'static,
        E: ConfigureEvm<Primitives = N> + Send + Sync + 'static,
        N: NodePrimitives + 'static,
    {
        if block.body().transactions().is_empty() {
            return None;
        }

        match replay_block_innertx(&self.provider, &self.evm_config, block) {
            Ok(traces) => Some(traces),
            Err(err) => {
                warn!(
                    target: "xlayer::flashblocks::innertx",
                    %err,
                    "Failed to compute innertx for flashblock"
                );
                None
            }
        }
    }
}

/// Replays a block with [`TraceCollector`] to capture internal transaction traces.
fn replay_block_innertx<P, E, N>(
    provider: &P,
    evm_config: &E,
    block: &RecoveredBlock<N::Block>,
) -> eyre::Result<Vec<Vec<InternalTransaction>>>
where
    P: StateProviderFactory + Send + Sync + 'static,
    E: ConfigureEvm<Primitives = N> + Send + Sync + 'static,
    N: NodePrimitives + 'static,
{
    let state_provider = provider.history_by_block_hash(block.parent_hash())?;

    let mut db = State::builder()
        .with_database(StateProviderDatabase::new(&state_provider))
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
