use crate::{execution::StateRootStrategy, FlashblockCachedReceipt};
use alloy_eips::eip2718::WithEncoded;
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;

use reth_errors::RethError;
use reth_evm::{
    execute::{
        BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, BlockExecutor,
    },
    ConfigureEvm, Evm,
};
use reth_execution_types::BlockExecutionResult;
use reth_primitives_traits::{HeaderTy, NodePrimitives, Recovered, RecoveredBlock};
use reth_revm::{
    cached::CachedReads,
    database::StateProviderDatabase,
    db::{states::bundle_state::BundleRetention, BundleState, State},
};
use reth_storage_api::StateProvider;
use reth_trie_common::HashedPostState;

/// Data returned from processor to validator for cache commit.
pub(crate) struct ProcessorOutcome<N: NodePrimitives> {
    pub(crate) execution_result: BlockExecutionResult<N::Receipt>,
    pub(crate) block: RecoveredBlock<N::Block>,
    pub(crate) hashed_state: HashedPostState,
    pub(crate) bundle: BundleState,
    pub(crate) read_cache: CachedReads,
}

/// Data extracted by validator from `PendingSequence`, passed to processor for incremental
/// execution.
pub(crate) struct IncrementalPrestate<N: NodePrimitives> {
    pub(crate) prestate_bundle: BundleState,
    pub(crate) cached_tx_count: usize,
    pub(crate) cached_receipts: Vec<N::Receipt>,
    pub(crate) cached_gas_used: u64,
    pub(crate) cached_blob_gas_used: u64,
    pub(crate) cached_reads: CachedReads,
}

/// Handles transaction execution, state root computation, and block assembly for flashblock
/// sequences.
///
/// Separated from [`super::validator::FlashblockSequenceValidator`] so that configurable state
/// root strategies (`Synchronous`, `Parallel`, `SparseTrieTask`) live cleanly here, while the
/// validator handles flashblocks-specific cache orchestration.
#[derive(Debug)]
pub(crate) struct FlashblockSequenceProcessor<N, EvmConfig> {
    evm_config: EvmConfig,
    state_root_strategy: StateRootStrategy,
    _primitives: std::marker::PhantomData<N>,
}

impl<N, EvmConfig> FlashblockSequenceProcessor<N, EvmConfig>
where
    N: NodePrimitives,
    N::Receipt: FlashblockCachedReceipt,
{
    pub(crate) fn new(evm_config: EvmConfig) -> Self {
        Self {
            evm_config,
            state_root_strategy: StateRootStrategy::default(),
            _primitives: std::marker::PhantomData,
        }
    }
}

impl<N, EvmConfig> FlashblockSequenceProcessor<N, EvmConfig>
where
    N: NodePrimitives,
    N::Receipt: FlashblockCachedReceipt,
    EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin>,
{
    /// Full flashblocks sequence execution from scratch.
    pub(crate) fn process_fresh(
        &self,
        base: &OpFlashblockPayloadBase,
        transactions: &[WithEncoded<Recovered<N::SignedTx>>],
        state_provider: &dyn StateProvider,
        parent_header: &reth_primitives_traits::SealedHeader<HeaderTy<N>>,
    ) -> eyre::Result<ProcessorOutcome<N>> {
        let mut read_cache = CachedReads::default();
        let cached_db = read_cache.as_db_mut(StateProviderDatabase::new(state_provider));
        let mut state = State::builder().with_database(cached_db).with_bundle_update().build();

        let mut builder = self
            .evm_config
            .builder_for_next_block(&mut state, parent_header, base.clone().into())
            .map_err(RethError::other)?;
        builder.apply_pre_execution_changes()?;
        for tx in transactions {
            builder.execute_transaction(tx.clone())?;
        }
        let BlockBuilderOutcome { execution_result, block, hashed_state, .. } =
            builder.finish(state_provider)?;
        let bundle = state.take_bundle();

        Ok(ProcessorOutcome { execution_result, block, hashed_state, bundle, read_cache })
    }

    /// Suffix-only execution reusing cached prefix state from an existing pending sequence.
    pub(crate) fn process_incremental(
        &self,
        base: &OpFlashblockPayloadBase,
        transactions: &[WithEncoded<Recovered<N::SignedTx>>],
        state_provider: &dyn StateProvider,
        parent_header: &reth_primitives_traits::SealedHeader<HeaderTy<N>>,
        prestate: IncrementalPrestate<N>,
    ) -> eyre::Result<ProcessorOutcome<N>> {
        let mut read_cache = prestate.cached_reads;
        let cached_db = read_cache.as_db_mut(StateProviderDatabase::new(state_provider));
        let mut state = State::builder()
            .with_database(cached_db)
            .with_bundle_prestate(prestate.prestate_bundle)
            .with_bundle_update()
            .build();

        let attrs = base.clone().into();
        let evm_env =
            self.evm_config.next_evm_env(parent_header, &attrs).map_err(RethError::other)?;
        let execution_ctx = self
            .evm_config
            .context_for_next_block(parent_header, attrs)
            .map_err(RethError::other)?;

        // Skip `apply_pre_execution_changes` (already applied in the original fresh build).
        // The only pre-execution effect we need is `set_state_clear_flag`, which configures
        // EVM empty-account handling (OP Stack chains activate Spurious Dragon at genesis, so
        // this is always true).
        state.set_state_clear_flag(true);
        let evm = self.evm_config.evm_with_env(&mut state, evm_env.clone());
        let mut executor = self.evm_config.create_executor(evm, execution_ctx.clone());

        for tx in transactions.iter().skip(prestate.cached_tx_count).cloned() {
            executor.execute_transaction(tx)?;
        }

        let (evm, execution_result) = executor.finish()?;
        let (db, _evm_env) = evm.finish();
        db.merge_transitions(BundleRetention::Reverts);

        let execution_result = merge_cached_block_execution_results::<N>(
            prestate.cached_receipts,
            prestate.cached_gas_used,
            prestate.cached_blob_gas_used,
            execution_result,
        );

        // Compute state root
        let hashed_state = state_provider.hashed_post_state(&db.bundle_state);
        let (state_root, _) = self.compute_state_root(state_provider, &hashed_state)?;
        let bundle = db.take_bundle();

        // Assemble block
        let (block_transactions, senders): (Vec<_>, Vec<_>) =
            transactions.iter().map(|tx| tx.1.clone().into_parts()).unzip();
        let block = self
            .evm_config
            .block_assembler()
            .assemble_block(BlockAssemblerInput::new(
                evm_env,
                execution_ctx,
                parent_header,
                block_transactions,
                &execution_result,
                &bundle,
                state_provider,
                state_root,
            ))
            .map_err(RethError::other)?;
        let block = RecoveredBlock::new_unhashed(block, senders);

        Ok(ProcessorOutcome { execution_result, block, hashed_state, bundle, read_cache })
    }

    /// Dispatches state root computation based on the configured [`StateRootStrategy`].
    fn compute_state_root(
        &self,
        state_provider: &dyn StateProvider,
        hashed_state: &HashedPostState,
    ) -> eyre::Result<(alloy_primitives::B256, reth_trie_common::updates::TrieUpdates)> {
        match self.state_root_strategy {
            StateRootStrategy::Synchronous => state_provider
                .state_root_with_updates(hashed_state.clone())
                .map_err(|e| eyre::eyre!(e)),
            StateRootStrategy::Parallel | StateRootStrategy::SparseTrieTask => {
                unimplemented!(
                    "Parallel and SparseTrieTask state root strategies are not yet implemented"
                )
            }
        }
    }
}

/// Merges prefix (cached) and suffix execution results into a single
/// [`BlockExecutionResult`].
fn merge_cached_block_execution_results<N: NodePrimitives>(
    cached_receipts: Vec<N::Receipt>,
    cached_gas_used: u64,
    cached_blob_gas_used: u64,
    mut execution_result: BlockExecutionResult<N::Receipt>,
) -> BlockExecutionResult<N::Receipt>
where
    N::Receipt: FlashblockCachedReceipt,
{
    N::Receipt::add_cumulative_gas_offset(&mut execution_result.receipts, cached_gas_used);
    let mut receipts = cached_receipts;
    receipts.extend(execution_result.receipts);
    BlockExecutionResult {
        receipts,
        requests: execution_result.requests,
        gas_used: cached_gas_used.saturating_add(execution_result.gas_used),
        blob_gas_used: cached_blob_gas_used.saturating_add(execution_result.blob_gas_used),
    }
}
