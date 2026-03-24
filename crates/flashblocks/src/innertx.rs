use alloy_consensus::BlockHeader as _;
use alloy_evm::block::BlockExecutor;
use reth_evm::ConfigureEvm;
use reth_primitives_traits::{BlockBody as _, NodePrimitives, RecoveredBlock};
use reth_revm::database::StateProviderDatabase;
use reth_storage_api::StateProviderFactory;
use revm_database::states::State;
use tracing::warn;
use xlayer_innertx::innertx_inspector::{InternalTransaction, TraceCollector};

// NOTE: Once the reth fork is updated with the hook changes, replace this with:
//   use reth_optimism_flashblocks::hook::{FlashBlockExtension, PostExecutionHook};
// For now, we define a local shim type for the extension data type used by the
// bridge task in main.rs.

/// Type used to store innertx traces inside a `FlashBlockExtension`.
/// The bridge enricher task wraps `Vec<Vec<InternalTransaction>>` in a
/// `FlashBlockExtension::new(traces)`, and consumers downcast via
/// `extract_innertx`.
pub type InnerTxTraces = Vec<Vec<InternalTransaction>>;

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
                    "Failed to compute innertx in post-execution hook"
                );
                None
            }
        }
    }
}

// NOTE: Once the reth fork is updated, implement the PostExecutionHook trait:
//
// impl<P, E, N> PostExecutionHook<N> for InnerTxHook<P, E>
// where
//     P: StateProviderFactory + Send + Sync + 'static,
//     E: ConfigureEvm<Primitives = N> + Send + Sync + 'static,
//     N: NodePrimitives + 'static,
// {
//     fn on_executed(&self, block: &RecoveredBlock<N::Block>) -> Option<FlashBlockExtension> {
//         self.compute_innertx::<N>(block).map(FlashBlockExtension::new)
//     }
// }

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

/// Convenience function to extract innertx data from a `FlashBlockExtension`.
///
/// Once the reth fork is updated, this downcasts from the type-erased extension.
/// For now it works with the bridge task's direct `InnerTxTraces` value.
///
/// Returns `None` if the extension is absent or does not contain innertx data.
pub fn extract_innertx_from_traces(
    traces: Option<&InnerTxTraces>,
) -> Option<&Vec<Vec<InternalTransaction>>> {
    traces
}

// NOTE: Once the reth fork is updated with FlashBlockExtension, use this instead:
// pub fn extract_innertx(
//     extension: Option<&FlashBlockExtension>,
// ) -> Option<&Vec<Vec<InternalTransaction>>> {
//     extension.and_then(|ext| ext.downcast_ref::<Vec<Vec<InternalTransaction>>>())
// }
