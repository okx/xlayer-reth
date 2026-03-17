use crate::{
    cache::{CachedTxInfo, FlashblockStateCache, PendingSequence},
    execution::{BuildArgs, StateRootStrategy},
    FlashblockCachedReceipt,
};
use alloy_consensus::BlockHeader;
use alloy_eips::eip2718::WithEncoded;
use alloy_primitives::B256;
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;
use reth_engine_primitives::TreeConfig;
use reth_engine_tree::tree::{
    payload_processor::{ExecutionEnv, PayloadProcessor},
    StateProviderBuilder,
};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::*;

use reth_chain_state::{ComputedTrieData, ExecutedBlock};
use reth_errors::RethError;
use reth_evm::{
    execute::{BlockAssembler, BlockAssemblerInput, BlockExecutor},
    ConfigureEvm, Evm,
};
use reth_execution_types::{BlockExecutionOutput, BlockExecutionResult};
use reth_primitives_traits::{
    transaction::TxHashRef, BlockBody, HeaderTy, NodePrimitives, Recovered, RecoveredBlock,
};
use reth_provider::{
    providers::OverlayStateProviderFactory, BlockNumReader, BlockReader, ChangeSetReader,
    DatabaseProviderFactory, HeaderProvider, PruneCheckpointReader, StageCheckpointReader,
    StateProviderFactory, StateReader, StorageChangeSetReader, StorageSettingsCache,
};
use reth_revm::{
    cached::CachedReads,
    database::StateProviderDatabase,
    db::{states::bundle_state::BundleRetention, BundleState, State},
};
use reth_rpc_eth_types::PendingBlock;
use reth_storage_api::{DatabaseProviderROFactory, StateProvider};
use reth_tasks::Runtime;
use reth_trie::{updates::TrieUpdates, HashedPostState, StateRoot};
use reth_trie_db::ChangesetCache;
use reth_trie_parallel::root::{ParallelStateRoot, ParallelStateRootError};

// ---------------------------------------------------------------------------
// Types (moved from processor.rs)
// ---------------------------------------------------------------------------

/// Data returned from execution for cache commit.
struct ExecutionOutcome<N: NodePrimitives> {
    execution_result: BlockExecutionResult<N::Receipt>,
    block: RecoveredBlock<N::Block>,
    hashed_state: HashedPostState,
    bundle: BundleState,
    read_cache: CachedReads,
}

/// Prestate extracted from a `PendingSequence` for incremental execution.
struct IncrementalPrestate<N: NodePrimitives> {
    prestate_bundle: BundleState,
    cached_tx_count: usize,
    cached_receipts: Vec<N::Receipt>,
    cached_gas_used: u64,
    cached_blob_gas_used: u64,
    cached_reads: CachedReads,
}

/// Trait alias for the bounds required on a provider factory to create an
/// [`OverlayStateProviderFactory`] that supports parallel and serial state root computation.
pub(crate) trait OverlayProviderFactory:
    DatabaseProviderFactory<
        Provider: StageCheckpointReader
                      + PruneCheckpointReader
                      + BlockNumReader
                      + ChangeSetReader
                      + StorageChangeSetReader
                      + StorageSettingsCache,
    > + Clone
    + Send
    + Sync
    + 'static
{
}

impl<T> OverlayProviderFactory for T where
    T: DatabaseProviderFactory<
            Provider: StageCheckpointReader
                          + PruneCheckpointReader
                          + BlockNumReader
                          + ChangeSetReader
                          + StorageChangeSetReader
                          + StorageSettingsCache,
        > + Clone
        + Send
        + Sync
        + 'static
{
}

// ---------------------------------------------------------------------------
// Identity tx iterator for already-recovered transactions
// ---------------------------------------------------------------------------

/// Creates a no-op `ExecutableTxIterator` from already-recovered transactions.
///
/// In the standard reth pipeline, `ConfigureEngineEvm::tx_iterator_for_payload` decodes
/// raw bytes and recovers signatures. Flashblocks transactions arrive pre-recovered from
/// the builder WS stream, so the "conversion" strips the encoding and yields a
/// `Recovered<T>` directly — no decoding or signature recovery needed.
///
/// This satisfies `ExecutableTxTuple` via the blanket impl for `(I, F)` because:
/// - `Vec<WithEncoded<Recovered<T>>>: IntoParallelIterator + IntoIterator`
/// - `fn(WithEncoded<Recovered<T>>) -> Result<Recovered<T>, Infallible>: ConvertTx`
/// - `Recovered<T>: ExecutableTxFor<Evm>` (via `ExecutableTxParts` + `RecoveredTx`)
#[allow(clippy::type_complexity)]
fn flashblock_tx_iterator<T: Clone + Send + Sync + 'static>(
    transactions: Vec<WithEncoded<Recovered<T>>>,
) -> (
    Vec<WithEncoded<Recovered<T>>>,
    fn(WithEncoded<Recovered<T>>) -> Result<Recovered<T>, Infallible>,
) {
    (transactions, |tx| Ok(tx.1))
}

// ---------------------------------------------------------------------------
// Free functions (moved from processor.rs)
// ---------------------------------------------------------------------------

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

/// Dispatches state root computation based on the configured [`StateRootStrategy`].
///
/// Uses a two-tier approach ported from reth's `payload_validator`:
/// 1. **Primary**: Try `Parallel` (if overlay factory available)
/// 2. **Fallback**: Overlay-based serial with prefix sets, or raw `state_provider` SR
fn compute_state_root<P>(
    strategy: StateRootStrategy,
    state_provider: &dyn StateProvider,
    hashed_state: &HashedPostState,
    overlay_factory: Option<OverlayStateProviderFactory<P>>,
    runtime: Runtime,
) -> eyre::Result<(B256, TrieUpdates)>
where
    P: OverlayProviderFactory,
{
    let mut maybe_result = None;

    match strategy {
        StateRootStrategy::Parallel | StateRootStrategy::SparseTrieTask => {
            if let Some(ref factory) = overlay_factory {
                match compute_state_root_parallel(factory.clone(), hashed_state, runtime) {
                    Ok(result) => maybe_result = Some(result),
                    Err(e) => {
                        warn!(target: "flashblocks", %e, "parallel state root failed, falling back to serial")
                    }
                }
            }
        }
        StateRootStrategy::Synchronous => {}
    }

    if let Some(result) = maybe_result {
        Ok(result)
    } else if let Some(factory) = overlay_factory {
        compute_state_root_serial(factory, hashed_state).map_err(|e| eyre::eyre!(e))
    } else {
        state_provider.state_root_with_updates(hashed_state.clone()).map_err(|e| eyre::eyre!(e))
    }
}

/// Computes state root in parallel using [`ParallelStateRoot`].
fn compute_state_root_parallel<P>(
    overlay_factory: OverlayStateProviderFactory<P>,
    hashed_state: &HashedPostState,
    runtime: Runtime,
) -> Result<(B256, TrieUpdates), ParallelStateRootError>
where
    P: OverlayProviderFactory,
{
    let prefix_sets = hashed_state.construct_prefix_sets().freeze();
    let overlay_factory =
        overlay_factory.with_extended_hashed_state_overlay(hashed_state.clone_into_sorted());
    ParallelStateRoot::new(overlay_factory, prefix_sets, runtime).incremental_root_with_updates()
}

/// Computes state root serially using overlay factory with prefix sets.
fn compute_state_root_serial<P>(
    overlay_factory: OverlayStateProviderFactory<P>,
    hashed_state: &HashedPostState,
) -> reth_errors::ProviderResult<(B256, TrieUpdates)>
where
    P: OverlayProviderFactory,
{
    let prefix_sets = hashed_state.construct_prefix_sets().freeze();
    let overlay_factory =
        overlay_factory.with_extended_hashed_state_overlay(hashed_state.clone_into_sorted());
    let provider = overlay_factory.database_provider_ro()?;
    Ok(StateRoot::new(&provider, &provider).with_prefix_sets(prefix_sets).root_with_updates()?)
}

// ---------------------------------------------------------------------------
// Validator
// ---------------------------------------------------------------------------

/// Builds the [`PendingSequence`]s from the accumulated flashblock transaction sequences.
/// Commits results directly to [`FlashblockStateCache`] via `handle_pending_sequence()`.
///
/// Supports two execution modes:
/// - **Fresh**: Full execution for a new block height. Uses `PayloadProcessor` for canonical
///   parents (prewarming + optional sparse trie SR pipeline). Falls back to direct execution
///   for non-canonical parents.
/// - **Incremental**: Suffix-only execution reusing cached prefix state from an existing
///   pending sequence at the same height.
pub(crate) struct FlashblockSequenceValidator<N: NodePrimitives, EvmConfig, Provider>
where
    N::Receipt: FlashblockCachedReceipt,
    EvmConfig: ConfigureEvm,
{
    /// EVM configuration — stored separately for access without borrowing `payload_processor`.
    evm_config: EvmConfig,
    /// Reth's payload processor — handles tx preparation, prewarming, and optional sparse
    /// trie SR pipeline. Used for fresh execution on canonical parents.
    payload_processor: PayloadProcessor<EvmConfig>,
    /// The state provider factory for resolving canonical and historical state.
    provider: Provider,
    /// The flashblocks state cache containing the flashblocks state cache layer.
    flashblocks_state: FlashblockStateCache<N>,
    /// State root computation strategy.
    state_root_strategy: StateRootStrategy,
    /// Configuration for engine tree (needed for `spawn()` calls).
    tree_config: Arc<TreeConfig>,
    /// Changeset cache for overlay state provider factory construction.
    changeset_cache: ChangesetCache,
    /// Runtime handle for spawning blocking tasks and SR computation.
    runtime: Runtime,
}

impl<N: NodePrimitives, EvmConfig, Provider> FlashblockSequenceValidator<N, EvmConfig, Provider>
where
    N::Receipt: FlashblockCachedReceipt,
    EvmConfig: ConfigureEvm + Clone,
{
    pub(crate) fn new(
        evm_config: EvmConfig,
        provider: Provider,
        flashblocks_state: FlashblockStateCache<N>,
        runtime: Runtime,
        state_root_strategy: StateRootStrategy,
        tree_config: Arc<TreeConfig>,
    ) -> Self {
        let payload_processor = PayloadProcessor::new(
            runtime.clone(),
            evm_config.clone(),
            &tree_config,
            Default::default(),
        );
        Self {
            evm_config,
            payload_processor,
            provider,
            flashblocks_state,
            state_root_strategy,
            tree_config,
            changeset_cache: ChangesetCache::new(),
            runtime,
        }
    }

    pub(crate) const fn provider(&self) -> &Provider {
        &self.provider
    }
}

impl<N, EvmConfig, Provider> FlashblockSequenceValidator<N, EvmConfig, Provider>
where
    N: NodePrimitives,
    N::Receipt: FlashblockCachedReceipt,
    EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin>
        + 'static,
    Provider: StateProviderFactory
        + HeaderProvider<Header = HeaderTy<N>>
        + OverlayProviderFactory
        + BlockReader
        + StateReader
        + Unpin
        + Clone,
{
    /// Executes a flashblock transaction sequence and commits the result to the flashblocks
    /// state cache.
    ///
    /// Determines execution mode from the current pending state:
    /// - No pending sequence / different height → fresh build.
    /// - Pending exists at the same height → incremental build.
    pub(crate) fn execute<I: IntoIterator<Item = WithEncoded<Recovered<N::SignedTx>>>>(
        &mut self,
        args: BuildArgs<I>,
    ) -> eyre::Result<()> {
        let block_number = args.base.block_number;
        let transactions: Vec<_> = args.transactions.into_iter().collect();

        let pending = self.flashblocks_state.get_pending_sequence();
        let pending_height = pending.as_ref().map(|p| p.get_height());
        let incremental = pending_height == Some(block_number);

        // Validate height continuity
        if let Some(pending_height) = pending_height
            && block_number != pending_height
            && block_number != pending_height + 1
        {
            warn!(
                target: "flashblocks",
                incoming_height = block_number,
                pending_height = pending_height,
                "state mismatch from incoming sequence to current pending tip",
            );
            return Err(eyre::eyre!(
                "state mismatch from incoming sequence to current pending tip"
            ));
        }

        if incremental {
            self.execute_incremental(
                args.base,
                transactions,
                args.last_flashblock_index,
                pending.unwrap(),
            )
        } else {
            self.execute_fresh(args.base, transactions, args.last_flashblock_index)
        }
    }

    /// Full flashblocks sequence execution from a new block height.
    ///
    /// For canonical parents, uses [`PayloadProcessor`] to benefit from prewarming and
    /// (when `SparseTrieTask` is enabled) the concurrent sparse trie SR pipeline.
    /// Non-canonical parents fall back to direct execution since the flashblocks overlay
    /// state provider does not satisfy `PayloadProcessor`'s provider bounds.
    fn execute_fresh(
        &mut self,
        base: OpFlashblockPayloadBase,
        transactions: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        last_flashblock_index: u64,
    ) -> eyre::Result<()> {
        let parent_hash = base.parent_hash;

        match self.provider.history_by_block_hash(parent_hash) {
            Ok(canon_provider) => {
                let header = self
                    .provider
                    .sealed_header_by_hash(parent_hash)?
                    .ok_or_else(|| eyre::eyre!("parent header not found for hash {parent_hash}"))?;
                let overlay_factory = OverlayStateProviderFactory::new(
                    self.provider.clone(),
                    self.changeset_cache.clone(),
                );
                self.execute_fresh_with_processor(
                    base,
                    transactions,
                    last_flashblock_index,
                    canon_provider.as_ref(),
                    &header,
                    overlay_factory,
                )
            }
            Err(err) => {
                trace!(
                    target: "flashblocks",
                    error = %err,
                    "parent not in canonical chain, try getting state from pending state",
                );
                let canonical_state = self.provider.latest()?;
                let (provider, header) = self
                    .flashblocks_state
                    .get_pending_state_provider(canonical_state)
                    .ok_or_else(|| {
                        eyre::eyre!(
                            "parent {parent_hash} not in canonical chain and no \
                             pending state available for overlay"
                        )
                    })?;
                // Non-canonical parent — direct execution, no PayloadProcessor
                let outcome = self.execute_direct_fresh::<Provider>(
                    &base,
                    &transactions,
                    provider.as_ref(),
                    &header,
                    None,
                )?;
                self.commit_pending_sequence(base, transactions, last_flashblock_index, outcome)
            }
        }
    }

    /// Fresh execution using [`PayloadProcessor`] for prewarming and optional SR pipeline.
    ///
    /// Only used for canonical parents where the provider satisfies `PayloadProcessor` bounds.
    fn execute_fresh_with_processor(
        &mut self,
        base: OpFlashblockPayloadBase,
        transactions: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        last_flashblock_index: u64,
        state_provider: &dyn StateProvider,
        parent_header: &reth_primitives_traits::SealedHeader<HeaderTy<N>>,
        overlay_factory: OverlayStateProviderFactory<Provider>,
    ) -> eyre::Result<()> {
        // Declare read_cache and state first — they must outlive execution_ctx (drop order).
        let mut read_cache = CachedReads::default();
        let cached_db = read_cache.as_db_mut(StateProviderDatabase::new(state_provider));
        let mut state = State::builder().with_database(cached_db).with_bundle_update().build();

        let attrs = base.clone().into();
        let evm_env =
            self.evm_config.next_evm_env(parent_header, &attrs).map_err(RethError::other)?;
        let execution_ctx = self
            .evm_config
            .context_for_next_block(parent_header, attrs)
            .map_err(RethError::other)?;

        // Build identity tx iterator (strip encoding, yield Recovered)
        let tx_iter = flashblock_tx_iterator(transactions.clone());

        // Build state provider builder for `PayloadProcessor`'s prewarming pipeline
        let provider_builder = StateProviderBuilder::new(
            self.provider.clone(),
            base.parent_hash,
            None, // no overlay blocks — flashblocks overlay is handled separately
        );

        let execution_env = ExecutionEnv {
            evm_env: evm_env.clone(),
            hash: B256::ZERO,
            parent_hash: base.parent_hash,
            parent_state_root: parent_header.state_root(),
            transaction_count: transactions.len(),
            withdrawals: None,
        };

        // Spawn `PayloadProcessor`: prewarming + optional SR pipeline
        let mut handle = match self.state_root_strategy {
            StateRootStrategy::SparseTrieTask => self.payload_processor.spawn(
                execution_env,
                tx_iter,
                provider_builder,
                overlay_factory.clone(),
                &self.tree_config,
                None,
            ),
            _ => self.payload_processor.spawn_cache_exclusive(
                execution_env,
                tx_iter,
                provider_builder,
                None,
            ),
        };

        // Build executor with our state (separate from prewarming)
        state.set_state_clear_flag(true);
        let evm = self.evm_config.evm_with_env(&mut state, evm_env.clone());
        let mut executor = self.evm_config.create_executor(evm, execution_ctx.clone());

        // Attach state hook (streams diffs to SR pipeline for `SparseTrieTask`)
        executor.set_state_hook(Some(Box::new(handle.state_hook())));

        executor.apply_pre_execution_changes()?;

        // Execute transactions from the handle (already converted to `WithTxEnv`)
        for tx_result in handle.iter_transactions() {
            let tx = tx_result.map_err(|e| eyre::eyre!("{e}"))?;
            executor.execute_transaction(tx)?;
        }

        let (evm, execution_result) = executor.finish()?;
        let (db, _evm_env) = evm.finish();
        db.merge_transitions(BundleRetention::Reverts);

        // Compute state root
        let hashed_state = state_provider.hashed_post_state(&db.bundle_state);
        let (state_root, _) = if self.state_root_strategy == StateRootStrategy::SparseTrieTask {
            let outcome = handle.state_root().map_err(|e| eyre::eyre!("{e}"))?;
            (outcome.state_root, outcome.trie_updates)
        } else {
            compute_state_root(
                self.state_root_strategy,
                state_provider,
                &hashed_state,
                Some(overlay_factory),
                self.runtime.clone(),
            )?
        };

        // Terminate caching + update execution cache
        let exec_output = Arc::new(BlockExecutionOutput {
            state: db.bundle_state.clone(),
            result: execution_result.clone(),
        });
        handle.terminate_caching(Some(exec_output));

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

        // Update `PayloadProcessor`'s execution cache for next block's prewarming
        self.payload_processor.on_inserted_executed_block(block.block_with_parent(), &bundle);

        let outcome =
            ExecutionOutcome { execution_result, block, hashed_state, bundle, read_cache };
        self.commit_pending_sequence(base, transactions, last_flashblock_index, outcome)
    }

    /// Direct fresh execution without `PayloadProcessor`.
    ///
    /// Used for non-canonical parents (flashblocks overlay state) where the provider
    /// does not satisfy `PayloadProcessor`'s bounds.
    fn execute_direct_fresh<P>(
        &self,
        base: &OpFlashblockPayloadBase,
        transactions: &[WithEncoded<Recovered<N::SignedTx>>],
        state_provider: &dyn StateProvider,
        parent_header: &reth_primitives_traits::SealedHeader<HeaderTy<N>>,
        overlay_factory: Option<OverlayStateProviderFactory<P>>,
    ) -> eyre::Result<ExecutionOutcome<N>>
    where
        P: OverlayProviderFactory,
    {
        let mut read_cache = CachedReads::default();
        let cached_db = read_cache.as_db_mut(StateProviderDatabase::new(state_provider));
        let mut state = State::builder().with_database(cached_db).with_bundle_update().build();

        let attrs = base.clone().into();
        let evm_env =
            self.evm_config.next_evm_env(parent_header, &attrs).map_err(RethError::other)?;
        let execution_ctx = self
            .evm_config
            .context_for_next_block(parent_header, attrs)
            .map_err(RethError::other)?;

        state.set_state_clear_flag(true);
        let evm = self.evm_config.evm_with_env(&mut state, evm_env.clone());
        let mut executor = self.evm_config.create_executor(evm, execution_ctx.clone());

        executor.apply_pre_execution_changes()?;

        for tx in transactions {
            executor.execute_transaction(tx.clone())?;
        }

        let (evm, execution_result) = executor.finish()?;
        let (db, _evm_env) = evm.finish();
        db.merge_transitions(BundleRetention::Reverts);

        let hashed_state = state_provider.hashed_post_state(&db.bundle_state);
        let (state_root, _) = compute_state_root(
            self.state_root_strategy,
            state_provider,
            &hashed_state,
            overlay_factory,
            self.runtime.clone(),
        )?;
        let bundle = db.take_bundle();

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

        Ok(ExecutionOutcome { execution_result, block, hashed_state, bundle, read_cache })
    }

    /// Incremental execution for the same block height as the current pending. Reuses
    /// the pending sequence's `BundleState` as prestate and its warm `CachedReads`,
    /// executing only new unexecuted transactions from incremental flashblock payloads.
    ///
    /// `SparseTrieTask` mode falls back to `Parallel` for incremental execution because
    /// the prewarming benefit is minimal and wiring prestate through `PayloadProcessor`
    /// adds complexity.
    fn execute_incremental(
        &self,
        base: OpFlashblockPayloadBase,
        transactions: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        last_flashblock_index: u64,
        pending: PendingSequence<N>,
    ) -> eyre::Result<()> {
        if pending.last_flashblock_index != last_flashblock_index {
            warn!(
                target: "flashblocks",
                incoming_last_flashblock_index = last_flashblock_index,
                pending_last_flashblock_index = pending.last_flashblock_index,
                "state mismatch from incoming sequence to current pending tip",
            );
            return Err(eyre::eyre!(
                "state mismatch, last flashblock index mismatch pending index"
            ));
        }

        let parent_hash = base.parent_hash;
        let canonical_state = self.provider.latest()?;
        let (state_provider, parent_header) = self
            .flashblocks_state
            .get_state_provider_by_hash(parent_hash, canonical_state)
            .ok_or_else(|| {
                eyre::eyre!("failed to build overlay state provider for parent {parent_hash}")
            })?;

        // Extract prestate from current pending
        let exec_output = &pending.pending.executed_block.execution_output;
        let prestate = IncrementalPrestate {
            prestate_bundle: exec_output.state.clone(),
            cached_tx_count: pending
                .pending
                .executed_block
                .recovered_block
                .body()
                .transaction_count(),
            cached_receipts: exec_output.result.receipts.clone(),
            cached_gas_used: exec_output.result.gas_used,
            cached_blob_gas_used: exec_output.result.blob_gas_used,
            cached_reads: pending.cached_reads,
        };

        // Create overlay factory for SR computation
        let overlay_factory = Some(OverlayStateProviderFactory::new(
            self.provider.clone(),
            self.changeset_cache.clone(),
        ));

        let outcome = self.execute_direct_incremental(
            &base,
            &transactions,
            state_provider.as_ref(),
            &parent_header,
            prestate,
            overlay_factory,
        )?;

        self.commit_pending_sequence(base, transactions, last_flashblock_index, outcome)
    }

    /// Suffix-only execution reusing cached prefix state.
    ///
    /// `SparseTrieTask` falls back to `Parallel` because the streaming hook only sees
    /// suffix transaction diffs — the prefix state is already baked into the prestate
    /// bundle.
    fn execute_direct_incremental<P>(
        &self,
        base: &OpFlashblockPayloadBase,
        transactions: &[WithEncoded<Recovered<N::SignedTx>>],
        state_provider: &dyn StateProvider,
        parent_header: &reth_primitives_traits::SealedHeader<HeaderTy<N>>,
        prestate: IncrementalPrestate<N>,
        overlay_factory: Option<OverlayStateProviderFactory<P>>,
    ) -> eyre::Result<ExecutionOutcome<N>>
    where
        P: OverlayProviderFactory,
    {
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

        // `SparseTrieTask` falls back to Parallel for incremental
        let hashed_state = state_provider.hashed_post_state(&db.bundle_state);
        let (state_root, _) = compute_state_root(
            self.state_root_strategy,
            state_provider,
            &hashed_state,
            overlay_factory,
            self.runtime.clone(),
        )?;
        let bundle = db.take_bundle();

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

        Ok(ExecutionOutcome { execution_result, block, hashed_state, bundle, read_cache })
    }

    /// Builds a [`PendingSequence`] from an [`ExecutionOutcome`] and commits it to the
    /// flashblocks state cache.
    fn commit_pending_sequence(
        &self,
        base: OpFlashblockPayloadBase,
        transactions: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        last_flashblock_index: u64,
        outcome: ExecutionOutcome<N>,
    ) -> eyre::Result<()> {
        let block_hash = outcome.block.hash();
        let parent_hash = base.parent_hash;

        let execution_outcome = Arc::new(BlockExecutionOutput {
            state: outcome.bundle,
            result: outcome.execution_result,
        });
        let executed_block = ExecutedBlock::new(
            outcome.block.into(),
            execution_outcome.clone(),
            ComputedTrieData::without_trie_input(
                Arc::new(outcome.hashed_state.into_sorted()),
                Arc::default(),
            ),
        );
        let pending_block = PendingBlock::with_executed_block(
            Instant::now() + Duration::from_secs(1),
            executed_block,
        );

        // Build tx index
        let mut tx_index = HashMap::with_capacity(transactions.len());
        for (idx, tx) in transactions.iter().enumerate() {
            tx_index.insert(
                *tx.tx_hash(),
                CachedTxInfo {
                    block_number: base.block_number,
                    block_hash,
                    tx_index: idx as u64,
                    tx: tx.1.clone().into_inner(),
                    receipt: execution_outcome.result.receipts[idx].clone(),
                },
            );
        }
        self.flashblocks_state.handle_pending_sequence(PendingSequence {
            pending: pending_block,
            tx_index,
            cached_reads: outcome.read_cache,
            block_hash,
            parent_hash,
            last_flashblock_index,
        })
    }
}
