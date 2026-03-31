use crate::{
    cache::{CachedTxInfo, FlashblockStateCache, PendingSequence},
    execution::{
        assemble::{assemble_flashblock, FlashblockAssemblerInput},
        BuildArgs, FlashblockReceipt, OverlayProviderFactory, PrefixExecutionMeta,
        StateRootStrategy,
    },
};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{mpsc::RecvTimeoutError, Arc},
    time::{Duration, Instant},
};
use tracing::*;

use alloy_consensus::{proofs::calculate_transaction_root, BlockHeader};
use alloy_eip7928::BlockAccessList;
use alloy_eips::eip2718::{Encodable2718, WithEncoded};
use alloy_evm::block::ExecutableTxParts;
use alloy_primitives::{Address, B256};
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;

use reth_chain_state::{ComputedTrieData, DeferredTrieData, ExecutedBlock, LazyOverlay};
use reth_engine_primitives::TreeConfig;
use reth_engine_tree::tree::{
    payload_processor::{
        receipt_root_task::{IndexedReceipt, ReceiptRootTaskHandle},
        ExecutionEnv, PayloadProcessor,
    },
    sparse_trie::StateRootComputeOutcome,
    CachedStateProvider, PayloadHandle, StateProviderBuilder,
};
use reth_errors::BlockExecutionError;
use reth_errors::RethError;
use reth_evm::{
    execute::{BlockExecutor, ExecutableTxFor},
    ConfigureEvm, Evm, EvmEnvFor, TxEnvFor,
};
use reth_execution_types::{BlockExecutionOutput, BlockExecutionResult};
use reth_optimism_forks::OpHardforks;
use reth_primitives_traits::{
    transaction::TxHashRef, HeaderTy, NodePrimitives, Recovered, RecoveredBlock, SealedHeaderFor,
};
use reth_provider::{
    providers::OverlayStateProviderFactory, BlockReader, DatabaseProviderROFactory,
    HashedPostStateProvider, HeaderProvider, ProviderError, StateProvider, StateProviderFactory,
    StateReader,
};
use reth_revm::{
    cached::CachedReads,
    database::StateProviderDatabase,
    db::{
        states::{
            bundle_state::BundleRetention,
            reverts::{AccountRevert, Reverts},
        },
        State,
    },
};
use reth_rpc_eth_types::PendingBlock;
use reth_tasks::Runtime;
use reth_trie::{updates::TrieUpdates, HashedPostState, StateRoot};
use reth_trie_parallel::root::{ParallelStateRoot, ParallelStateRootError};

/// Builds [`PendingSequence`]s from the accumulated flashblock transaction sequences.
/// Commits results directly to [`FlashblockStateCache`] via `handle_pending_sequence()`.
///
/// The execution uses the Reth's [`PayloadProcessor`] for optimal execution and state
/// root calculation of flashlbocks sequence. All 3 state root computation strategies
/// are supported (synchronous, parrallel and state root task using sparse trie).
///
/// - **Fresh (canonical parent)**: `StateProviderBuilder` with no overlay blocks.
/// - **Fresh (non-canonical parent)**: `StateProviderBuilder` with overlay blocks from
///   the flashblocks confirm/pending cache via `get_overlay_data()`.
/// - **Incremental (same height)**: Full re-execution via `execute_fresh()`. The warm
///   execution cache and `PreservedSparseTrie` from the previous sequence build offset
///   the cost of re-executing prefix transactions.
pub(crate) struct FlashblockSequenceValidator<N: NodePrimitives, EvmConfig, Provider, ChainSpec>
where
    EvmConfig: ConfigureEvm,
    ChainSpec: OpHardforks,
{
    /// The flashblocks state cache containing the flashblocks state cache layer.
    flashblocks_state: FlashblockStateCache<N>,
    /// Provider for database state access.
    provider: Provider,
    /// EVM configuration.
    evm_config: EvmConfig,
    /// Chain specification for hardfork checks.
    chain_spec: Arc<ChainSpec>,
    /// Configuration for the engine tree.
    tree_config: TreeConfig,
    /// Payload processor for state root computation.
    payload_processor: PayloadProcessor<EvmConfig>,
    /// Task runtime for spawning parallel work.
    runtime: Runtime,
}

impl<N, EvmConfig, Provider, ChainSpec>
    FlashblockSequenceValidator<N, EvmConfig, Provider, ChainSpec>
where
    N: NodePrimitives,
    N::Receipt: FlashblockReceipt,
    EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin + Send>
        + 'static,
    Provider: StateProviderFactory
        + HeaderProvider<Header = HeaderTy<N>>
        + OverlayProviderFactory
        + BlockReader
        + StateReader
        + HashedPostStateProvider
        + Unpin
        + Clone,
    ChainSpec: OpHardforks,
{
    pub(crate) fn new(
        evm_config: EvmConfig,
        provider: Provider,
        chain_spec: Arc<ChainSpec>,
        flashblocks_state: FlashblockStateCache<N>,
        runtime: Runtime,
        tree_config: TreeConfig,
    ) -> Self {
        let payload_processor = PayloadProcessor::new(
            runtime.clone(),
            evm_config.clone(),
            &tree_config,
            Default::default(),
        );
        Self {
            flashblocks_state,
            provider,
            evm_config,
            chain_spec,
            tree_config,
            payload_processor,
            runtime,
        }
    }

    /// Executes the incoming flashblocks sequence transactions delta and commits the
    /// result to the flashblocks state cache.
    pub(crate) async fn execute_sequence<
        I: IntoIterator<Item = WithEncoded<Recovered<N::SignedTx>>>,
    >(
        &mut self,
        args: BuildArgs<I>,
    ) -> eyre::Result<()>
    where
        N::SignedTx: Encodable2718,
        N::Block: From<alloy_consensus::Block<N::SignedTx>>,
    {
        // Pre-validate incoming flashblocks sequence
        let pending_sequence = self.prevalidate_incoming_sequence(&args)?;

        // Only compute state root on the final flashblock (target_index == last_index) or when
        // sequence end is received.
        //
        // Intermediate flashblocks use spawn_cache_exclusive() for execution + prewarming only,
        // skipping the sparse trie pipeline, deferred trie tasks, and changeset cache population.
        // This reduces MDBX read contention with the engine's persistence writes.
        let calculate_state_root =
            args.last_flashblock_index == args.target_index || args.sequence_end;
        let strategy = if calculate_state_root {
            self.plan_state_root_computation()
        } else {
            // No SR calculation logic - always use cache-exclusive
            StateRootStrategy::Synchronous
        };

        let parent_hash = args.base.parent_hash;
        let block_transactions: Vec<_> = args.transactions.into_iter().collect();
        let block_transaction_count = block_transactions.len();

        let mut transactions: Vec<_> = if let Some(ref seq) = pending_sequence {
            block_transactions
                .iter()
                .skip(seq.prefix_execution_meta.cached_tx_count)
                .cloned()
                .collect()
        } else {
            block_transactions.clone()
        };

        // Hash to get state provider builder for overlay data.
        // 1. Fresh builds - get the provider builder from parent hash.
        // 2. Incremental builds - get provider builder from pending sequence hash.
        let mut hash = pending_sequence.as_ref().map_or(parent_hash, |seq| seq.get_hash());

        // Re-execute the full pending sequence for concurrent state root task. We use:
        // 1. Parent block's state root to get the trie nodes (PreservedSparseTrie) from
        //    the previous block's computed state root.
        // 2. Parent hash to get the state overlay provider builder at start of block.
        // 3. Full block transactions to execute.
        // 4. Re-use cache reads in pending sequence for fast re-execution with pre-warm
        //    state accounts.
        if calculate_state_root {
            match strategy {
                StateRootStrategy::StateRootTask => {
                    transactions = block_transactions.clone();
                    hash = parent_hash;
                }
                // Parallel and synchronous state root strategies will use incremental
                // execution of tx deltas, using the pending sequence
                _ => {}
            }
        }

        let (provider_builder, header, overlay_data) = self.state_provider_builder(hash)?;
        let mut state_provider = provider_builder.build()?;

        let attrs = args.base.clone().into();
        let parent_header =
            pending_sequence.as_ref().map_or(header, |seq| seq.parent_header.clone());
        let evm_env =
            self.evm_config.next_evm_env(&parent_header, &attrs).map_err(RethError::other)?;

        let execution_env = ExecutionEnv {
            evm_env: evm_env.clone(),
            hash: B256::ZERO,
            parent_hash,
            parent_state_root: parent_header.state_root(),
            transaction_count: transactions.len(),
            withdrawals: Some(args.withdrawals),
        };

        // TODO: Extract the BAL once flashblocks BAL is supported
        let bal = None;

        // Create lazy overlay from ancestors - this doesn't block, allowing execution to start
        // before the trie data is ready. The overlay will be computed on first access.
        let (lazy_overlay, anchor_hash) =
            Self::get_parent_lazy_overlay(overlay_data.as_ref(), hash);

        // Create overlay factory for payload processor (StateRootTask path needs it for
        // multiproofs)
        let overlay_factory = OverlayStateProviderFactory::new(
            self.provider.clone(),
            self.flashblocks_state.get_changeset_cache(),
        )
        .with_block_hash(Some(anchor_hash))
        .with_lazy_overlay(lazy_overlay);

        // Spawn the appropriate processor based on strategy.
        let mut handle = self.spawn_payload_processor(
            execution_env,
            transactions.clone(),
            provider_builder,
            overlay_factory.clone(),
            strategy,
            bal,
        )?;

        // Use cached state provider before executing, used in execution after prewarming threads
        // complete
        if let Some((caches, cache_metrics)) = handle.caches().zip(handle.cache_metrics()) {
            state_provider =
                Box::new(CachedStateProvider::new(state_provider, caches, cache_metrics));
        };

        // Execute the block and handle any execution errors.
        // The receipt root task is spawned before execution and receives receipts incrementally
        // as transactions complete, allowing parallel computation during execution.
        let (output, senders, receipt_root_rx, cached_reads) = self.execute_block(
            state_provider.as_ref(),
            evm_env,
            &parent_header,
            attrs,
            transactions,
            pending_sequence.as_ref(),
            &mut handle,
            calculate_state_root && strategy == StateRootStrategy::StateRootTask,
        )?;
        debug!(
            target: "flashblocks::validator",
            execute_height = args.base.block_number,
            flashblock_index = args.last_flashblock_index,
            "Executed block",
        );

        // After executing the block we can stop prewarming transactions
        handle.stop_prewarming_execution();

        // Create ExecutionOutcome early so we can terminate caching before validation and state
        // root computation. Using Arc allows sharing with both the caching task and the deferred
        // trie task without cloning the expensive BundleState.
        let output = Arc::new(output);

        // Terminate caching task early since execution is complete and caching is no longer
        // needed. This frees up resources while state root computation continues.
        let valid_block_tx = handle.terminate_caching(Some(output.clone()));

        // Extract signed transactions for the block body before moving
        // `block_transactions` into the tx root closure.
        let body_transactions: Vec<N::SignedTx> =
            block_transactions.iter().map(|tx| tx.1.inner().clone()).collect();

        // Spawn async tx root computation
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.payload_processor.executor().spawn_blocking(move || {
            let txs: Vec<_> = block_transactions.iter().map(|tx| &tx.1).collect();
            let _ = result_tx.send(calculate_transaction_root(&txs));
        });

        // Wait for the receipt root computation to complete.
        let (receipts_root, logs_bloom) = {
            debug!(target: "flashblocks::validator", "wait_receipt_root");
            receipt_root_rx
                        .await
                        .inspect_err(|_| {
                            tracing::error!(
                                target: "flashblocks::validator",
                                execute_height = args.base.block_number,
                                "Receipt root task dropped sender without result, receipt root calculation likely aborted"
                            );
                        })?
        };
        let transactions_root = result_rx.await.inspect_err(|_| {
            tracing::error!(
                target: "flashblocks::validator",
                "Transaction root task dropped sender without result, transaction root calculation likely aborted"
            );
        })?;

        let (state_root, trie_output, hashed_state) = if calculate_state_root {
            let root_time = Instant::now();
            let hashed_state = self.provider.hashed_post_state(&output.state);
            let mut maybe_state_root = None;
            match strategy {
                StateRootStrategy::StateRootTask => {
                    debug!(target: "flashblocks::validator", execute_height = args.base.block_number, "Using sparse trie state root algorithm");

                    let task_result = self.await_state_root_with_timeout(
                        &mut handle,
                        overlay_factory.clone(),
                        &hashed_state,
                        args.base.block_number,
                    )?;

                    match task_result {
                        Ok(StateRootComputeOutcome { state_root, trie_updates }) => {
                            let elapsed = root_time.elapsed();
                            maybe_state_root = Some((state_root, trie_updates));
                            info!(
                                target: "flashblocks::validator",
                                execute_height = args.base.block_number,
                                flashblock_index = args.last_flashblock_index,
                                target_index = args.target_index,
                                ?state_root,
                                ?elapsed,
                                "State root task finished",
                            );
                        }
                        Err(error) => {
                            debug!(
                                target: "flashblocks::validator",
                                execute_height = args.base.block_number,
                                flashblock_index = args.last_flashblock_index,
                                target_index = args.target_index,
                                %error,
                                "State root task failed",
                            );
                        }
                    }
                }
                StateRootStrategy::Parallel => {
                    debug!(
                        target: "flashblocks::validator",
                        execute_height = args.base.block_number,
                        flashblock_index = args.last_flashblock_index,
                        target_index = args.target_index,
                        "Using parallel state root algorithm",
                    );
                    match self.compute_state_root_parallel(overlay_factory.clone(), &hashed_state) {
                        Ok(result) => {
                            let elapsed = root_time.elapsed();
                            info!(
                                target: "flashblocks::validator",
                                execute_height = args.base.block_number,
                                flashblock_index = args.last_flashblock_index,
                                target_index = args.target_index,
                                regular_state_root = ?result.0,
                                ?elapsed,
                                "Parallel state root computation finished"
                            );
                            maybe_state_root = Some((result.0, result.1));
                        }
                        Err(error) => {
                            debug!(
                                target: "flashblocks::validator",
                                execute_height = args.base.block_number,
                                flashblock_index = args.last_flashblock_index,
                                target_index = args.target_index,
                                err = %error,
                                "Parallel state root computation failed",
                            );
                        }
                    }
                }
                StateRootStrategy::Synchronous => {}
            }

            // Determine the state root.
            if let Some(maybe_state_root) = maybe_state_root {
                (maybe_state_root.0, maybe_state_root.1, hashed_state)
            } else {
                warn!(
                    target: "flashblocks::validator",
                    execute_height = args.base.block_number,
                    flashblock_index = args.last_flashblock_index,
                    target_index = args.target_index,
                    "Failed to compute state root",
                );
                let (state_root, trie_output) =
                    Self::compute_state_root_serial(overlay_factory.clone(), &hashed_state)?;
                (state_root, trie_output, hashed_state)
            }
        } else {
            (B256::ZERO, TrieUpdates::default(), HashedPostState::default())
        };

        // Capture execution metrics before `output` is moved into the deferred trie task.
        let prefix_gas_used = output.result.gas_used;
        let prefix_blob_gas_used = output.result.blob_gas_used;

        // Assemble the block using pre-computed roots (avoids recomputation).
        let block = assemble_flashblock(
            self.chain_spec.as_ref(),
            FlashblockAssemblerInput {
                base: &args.base,
                state_root,
                transactions_root,
                receipts_root,
                logs_bloom,
                gas_used: prefix_gas_used,
                blob_gas_used: prefix_blob_gas_used,
                bundle_state: &output.state,
                state_provider: state_provider.as_ref(),
                transactions: body_transactions,
            },
        )?;
        let block: N::Block = block.into();
        let block = RecoveredBlock::new_unhashed(block, senders);

        if let Some(valid_block_tx) = valid_block_tx {
            let _ = valid_block_tx.send(());
        }
        let executed_block = if calculate_state_root {
            self.spawn_deferred_trie_task(
                block,
                output,
                hashed_state,
                trie_output,
                overlay_data,
                overlay_factory,
            )
        } else {
            ExecutedBlock::new(
                Arc::new(block),
                output,
                ComputedTrieData::without_trie_input(
                    Arc::new(hashed_state.into_sorted()),
                    Arc::new(trie_output.into_sorted()),
                ),
            )
        };

        // Update `PayloadProcessor`'s execution cache for next block's prewarming
        self.payload_processor.on_inserted_executed_block(
            executed_block.recovered_block.block_with_parent(),
            &executed_block.execution_output.state,
        );

        let execution_height = args.base.block_number;
        let last_index = args.last_flashblock_index;
        let target_index = args.target_index;
        let block_hash = executed_block.recovered_block.hash();
        self.commit_pending_sequence(
            args.base,
            executed_block,
            parent_header,
            PrefixExecutionMeta {
                payload_id: args.payload_id,
                cached_reads,
                cached_tx_count: block_transaction_count,
                gas_used: prefix_gas_used,
                blob_gas_used: prefix_blob_gas_used,
                last_flashblock_index: args.last_flashblock_index,
            },
            block_transaction_count,
            args.target_index,
        )?;

        info!(
            target: "flashblocks",
            execution_height,
            last_index,
            target_index,
            ?block_hash,
            "Executed and validated flashblocks sequence",
        );
        Ok(())
    }

    /// Builds a [`PendingSequence`] from an [`ExecutionOutcome`] and commits it to the
    /// flashblocks state cache.
    fn commit_pending_sequence(
        &self,
        base: OpFlashblockPayloadBase,
        executed_block: ExecutedBlock<N>,
        parent_header: SealedHeaderFor<N>,
        prefix_execution_meta: PrefixExecutionMeta,
        transaction_count: usize,
        target_index: u64,
    ) -> eyre::Result<()> {
        // Build tx index
        let block_hash = executed_block.recovered_block.hash();
        let mut tx_index = HashMap::with_capacity(transaction_count);
        for (idx, tx) in executed_block.recovered_block.transactions_recovered().enumerate() {
            tx_index.insert(
                *tx.tx_hash(),
                CachedTxInfo {
                    block_number: base.block_number,
                    block_hash,
                    tx_index: idx as u64,
                    tx: tx.into_inner().clone(),
                    receipt: executed_block.execution_output.result.receipts[idx].clone(),
                },
            );
        }

        self.flashblocks_state.handle_pending_sequence(PendingSequence {
            // Set pending block deadline to 1 second matching default blocktime.
            pending: PendingBlock::with_executed_block(
                Instant::now() + Duration::from_secs(1),
                executed_block,
            ),
            tx_index,
            block_hash,
            parent_header,
            prefix_execution_meta,
            target_index,
        })
    }

    fn prevalidate_incoming_sequence<
        I: IntoIterator<Item = WithEncoded<Recovered<N::SignedTx>>>,
    >(
        &self,
        args: &BuildArgs<I>,
    ) -> eyre::Result<Option<PendingSequence<N>>> {
        let incoming_payload_id = args.payload_id;
        let incoming_block_number = args.base.block_number;
        let incoming_last_index = args.last_flashblock_index;
        if let Some(pending) = self.flashblocks_state.get_pending_sequence() {
            // Validate incoming height continuity
            let pending_height = pending.get_height();
            if pending_height != incoming_block_number
                && pending_height + 1 != incoming_block_number
            {
                return Err(eyre::eyre!(
                    "height mismatch: incoming={incoming_block_number}, pending={pending_height}"
                ));
            }
            if pending_height == incoming_block_number {
                // Validate for incremental builds
                let pending_payload_id = pending.prefix_execution_meta.payload_id;
                if pending_payload_id != incoming_payload_id {
                    return Err(eyre::eyre!(
                        "payload_id mismatch on incremental build: incoming={incoming_payload_id}, pending={pending_payload_id}"
                    ));
                }
                let pending_last_index = pending.prefix_execution_meta.last_flashblock_index;
                if !args.sequence_end && pending_last_index >= incoming_last_index {
                    return Err(eyre::eyre!(
                        "skipping, flashblock index already validated: incoming={incoming_last_index}, pending={pending_last_index}"
                    ));
                }
                return Ok(Some(pending));
            }
            // Optimistic fresh build
            return Ok(None);
        }
        // No pending sequence. Validate with flashblocks state cache highest confirm height
        let confirm_height = self.flashblocks_state.get_confirm_height();
        if confirm_height == 0 {
            return Err(eyre::eyre!(
                "confirm height not yet initialized, skipping: incoming={incoming_block_number}"
            ));
        }
        if incoming_block_number > confirm_height + 1 {
            return Err(eyre::eyre!(
                "flashblock height too far ahead: incoming={incoming_block_number}, confirm={confirm_height}"
            ));
        }
        if incoming_block_number <= confirm_height {
            return Err(eyre::eyre!(
                "stale height: incoming={incoming_block_number}, confirm={confirm_height}"
            ));
        }
        Ok(None)
    }

    /// Executes a block with the given state provider.
    ///
    /// This method orchestrates block execution:
    /// 1. Sets up the EVM with state database and precompile caching
    /// 2. Spawns a background task for incremental receipt root computation
    /// 3. Executes transactions with metrics collection via state hooks
    /// 4. Merges state transitions and records execution metrics
    #[expect(clippy::type_complexity, clippy::too_many_arguments)]
    fn execute_block<Err, T>(
        &mut self,
        state_provider: &dyn StateProvider,
        evm_env: EvmEnvFor<EvmConfig>,
        parent_header: &SealedHeaderFor<N>,
        attrs: EvmConfig::NextBlockEnvCtx,
        transactions: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        pending_sequence: Option<&PendingSequence<N>>,
        handle: &mut PayloadHandle<T, Err, N::Receipt>,
        state_root_task: bool,
    ) -> eyre::Result<(
        BlockExecutionOutput<N::Receipt>,
        Vec<Address>,
        tokio::sync::oneshot::Receiver<(B256, alloy_primitives::Bloom)>,
        CachedReads,
    )>
    where
        T: ExecutableTxFor<EvmConfig> + ExecutableTxParts<TxEnvFor<EvmConfig>, N::SignedTx>,
        Err: core::error::Error + Send + Sync + 'static,
        N::SignedTx: TxHashRef,
        EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin>
            + 'static,
    {
        // Build state
        let mut read_cache = pending_sequence
            .map(|p| p.prefix_execution_meta.cached_reads.clone())
            .unwrap_or_default();
        let cached_db = read_cache.as_db_mut(StateProviderDatabase::new(state_provider));
        let mut state_builder = State::builder().with_database(cached_db).with_bundle_update();
        if !state_root_task && let Some(seq) = pending_sequence {
            state_builder = state_builder
                .with_bundle_prestate(seq.pending.executed_block.execution_output.state.clone());
        }
        let mut db = state_builder.build();

        // For incremental builds, the only pre-execution effect we need is set_state_clear_flag,
        // which configures EVM empty-account handling (OP Stack chains activate Spurious Dragon
        // at genesis, so this is always true).
        if !state_root_task && pending_sequence.is_some() {
            db.set_state_clear_flag(true);
        }

        let evm = self.evm_config.evm_with_env(&mut db, evm_env);
        let execution_ctx = self
            .evm_config
            .context_for_next_block(parent_header, attrs)
            .map_err(RethError::other)?;
        let executor = self.evm_config.create_executor(evm, execution_ctx.clone());
        // Release the lifetime tie to &mut db so subsequent mutable borrows of db are allowed.
        drop(execution_ctx);

        // Spawn background task to compute receipt root and logs bloom incrementally.
        // Unbounded channel is used since tx count bounds capacity anyway (max ~30k txs per block).
        let prefix_receipt_count =
            pending_sequence.filter(|_| !state_root_task).map_or(0, |s| s.pending.receipts.len());

        let receipts_len = prefix_receipt_count + transactions.len();
        let (receipt_tx, receipt_rx) = crossbeam_channel::unbounded();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let task_handle = ReceiptRootTaskHandle::new(receipt_rx, result_tx);
        self.payload_processor.executor().spawn_blocking(move || task_handle.run(receipts_len));

        let transaction_count = transactions.len();
        let mut executor = executor.with_state_hook(Some(Box::new(handle.state_hook())));

        // Apply pre-execution changes for fresh builds
        if state_root_task || pending_sequence.is_none() {
            executor.apply_pre_execution_changes()?;
        }

        // Execute all transactions and finalize
        let execute_height = parent_header.number() + 1;
        let (executor, suffix_senders, suffix_receipts) = self.execute_transactions(
            executor,
            pending_sequence,
            transaction_count,
            handle,
            &receipt_tx,
            execute_height,
            state_root_task,
        )?;
        drop(receipt_tx);

        // Finish execution and replace with the generated suffix receipts
        let (_evm, mut result) = executor.finish().map(|(evm, result)| (evm.into_db(), result))?;
        result.receipts = suffix_receipts;
        if !state_root_task && let Some(seq) = pending_sequence {
            result = Self::merge_suffix_results(
                &seq.prefix_execution_meta,
                (*seq.pending.receipts).clone(),
                result,
            );
        }
        // Reconstruct full senders list
        let senders = if !state_root_task && let Some(seq) = pending_sequence {
            let mut all_senders = seq.pending.executed_block.recovered_block.senders().to_vec();
            all_senders.extend(suffix_senders);
            all_senders
        } else {
            suffix_senders
        };

        // Merge transitions into bundle state
        db.merge_transitions(BundleRetention::Reverts);

        // Explicitly drop db to release the mutable borrow on read_cache held via cached_db,
        // allowing read_cache to be moved into the return value.
        let mut bundle = db.take_bundle();
        drop(db);

        // For incremental builds, the bundle accumulates one revert entry per flashblock
        // index (from with_bundle_prestate + merge_transitions at each index). The engine
        // persistence service expects a single revert entry per block. Flatten all revert
        // transitions into one:
        // - Keep the earliest (parent-state) account info revert per address
        // - Merge storage reverts across transitions (earliest per slot via or_insert)
        if !state_root_task && pending_sequence.is_some() && bundle.reverts.len() > 1 {
            let mut reverts_map = HashMap::<Address, AccountRevert>::new();
            for reverts in bundle.reverts.iter() {
                for (addr, new_revert) in reverts {
                    if let Some(revert_entry) = reverts_map.get_mut(addr) {
                        // Merge new storage slots from later transitions and keep the
                        // earliest value per slot (parent-state revert entry).
                        for (slot, slot_revert) in &new_revert.storage {
                            revert_entry.storage.entry(*slot).or_insert(*slot_revert);
                        }
                        // Propagate wipe_storage if any transition triggers it, such as
                        // SELFDESTRUCT in a later flashblock index.
                        revert_entry.wipe_storage |= new_revert.wipe_storage;
                    } else {
                        reverts_map.insert(*addr, new_revert.clone());
                    }
                }
            }
            bundle.reverts = Reverts::new(vec![reverts_map.into_iter().collect()]);
        }

        let output = BlockExecutionOutput { result, state: bundle };
        Ok((output, senders, result_rx, read_cache))
    }

    #[expect(clippy::type_complexity)]
    fn execute_transactions<Executor, Err, T>(
        &self,
        mut executor: Executor,
        pending_sequence: Option<&PendingSequence<N>>,
        transaction_count: usize,
        handle: &mut PayloadHandle<T, Err, N::Receipt>,
        receipt_tx: &crossbeam_channel::Sender<IndexedReceipt<N::Receipt>>,
        execute_height: u64,
        state_root_task: bool,
    ) -> eyre::Result<(Executor, Vec<Address>, Vec<N::Receipt>), BlockExecutionError>
    where
        T: ExecutableTxFor<EvmConfig>
            + ExecutableTxParts<
                <<Executor as BlockExecutor>::Evm as Evm>::Tx,
                <Executor as BlockExecutor>::Transaction,
            >,
        Executor: BlockExecutor<Receipt = N::Receipt>,
        Err: core::error::Error + Send + Sync + 'static,
        N::SignedTx: TxHashRef,
        EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin>
            + 'static,
    {
        // Send all previously executed receipts to the receipt root task for incremental builds.
        let receipt_index_offset = if !state_root_task && let Some(seq) = pending_sequence {
            let prefix_count = seq.pending.receipts.len();
            for (index, receipt) in seq.pending.receipts.iter().enumerate() {
                let _ = receipt_tx.send(IndexedReceipt::new(index, receipt.clone()));
            }
            prefix_count
        } else {
            0
        };

        let mut senders = Vec::with_capacity(transaction_count);
        let mut receipts = Vec::new();
        let mut transactions = handle.iter_transactions();

        // Some executors may execute transactions that do not append receipts during the
        // main loop (e.g., system transactions whose receipts are added during finalization).
        // In that case, invoking the callback on every transaction would resend the previous
        // receipt with the same index and can panic the ordered root builder.
        let mut last_sent_len = 0usize;
        let prefix_gas_used = pending_sequence
            .filter(|_| !state_root_task)
            .map_or(0, |seq| seq.prefix_execution_meta.gas_used);
        loop {
            let Some(tx_result) = transactions.next() else { break };

            let tx = tx_result.map_err(BlockExecutionError::other)?;
            let tx_signer = *tx.signer();
            senders.push(tx_signer);

            trace!(target: "flashblocks::validator", execute_height, txhash = %tx.tx().tx_hash(), "Executing transaction");
            executor.execute_transaction(tx)?;

            let current_len = executor.receipts().len();
            if current_len > last_sent_len {
                last_sent_len = current_len;
                // Send the latest receipt to the background task for incremental root computation.
                if let Some(mut receipt) = executor.receipts().last().cloned() {
                    let tx_index = receipt_index_offset + current_len - 1;
                    receipt.add_cumulative_gas_offset(prefix_gas_used);
                    receipts.push(receipt.clone());
                    let _ = receipt_tx.send(IndexedReceipt::new(tx_index, receipt));
                }
            }
        }
        Ok((executor, senders, receipts))
    }

    /// Determines the state root computation strategy based on configuration.
    ///
    /// Note: Use state root task only if prefix sets are empty, otherwise proof generation is
    /// too expensive because it requires walking all paths in every proof.
    const fn plan_state_root_computation(&self) -> StateRootStrategy {
        if self.tree_config.state_root_fallback() {
            StateRootStrategy::Synchronous
        } else if self.tree_config.use_state_root_task() {
            StateRootStrategy::StateRootTask
        } else {
            StateRootStrategy::Parallel
        }
    }

    fn spawn_payload_processor(
        &mut self,
        env: ExecutionEnv<EvmConfig>,
        txs: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        provider_builder: StateProviderBuilder<N, Provider>,
        overlay_factory: OverlayStateProviderFactory<Provider>,
        strategy: StateRootStrategy,
        bal: Option<Arc<BlockAccessList>>,
    ) -> eyre::Result<
        PayloadHandle<
            impl ExecutableTxFor<EvmConfig> + use<N, Provider, EvmConfig, ChainSpec>,
            impl core::error::Error + Send + Sync + 'static + use<N, Provider, EvmConfig, ChainSpec>,
            N::Receipt,
        >,
    > {
        let tx_iter = Self::flashblock_tx_iterator(txs);
        match strategy {
            StateRootStrategy::StateRootTask => {
                // Use the pre-computed overlay factory for multiproofs
                Ok(self.payload_processor.spawn(
                    env,
                    tx_iter,
                    provider_builder,
                    overlay_factory,
                    &self.tree_config,
                    bal,
                ))
            }
            StateRootStrategy::Parallel | StateRootStrategy::Synchronous => Ok(self
                .payload_processor
                .spawn_cache_exclusive(env, tx_iter, provider_builder, bal)),
        }
    }

    /// Awaits the state root from the background task, with an optional timeout fallback.
    ///
    /// If a timeout is configured (`state_root_task_timeout`), this method first waits for the
    /// state root task up to the timeout duration. If the task doesn't complete in time, a
    /// sequential state root computation is spawned via `spawn_blocking`. Both computations
    /// send results into a single unified channel — one `recv()` returns whichever finishes
    /// first, with no polling.
    ///
    /// If no timeout is configured, this simply awaits the state root task without any fallback.
    ///
    /// Returns `eyre::Result<Result<...>>` where the outer result captures unrecoverable errors
    /// from the sequential fallback (e.g. DB errors), while the inner `Result` captures parallel
    /// state root task errors that can still fall back to serial.
    fn await_state_root_with_timeout<Tx, Err, R: Send + Sync + 'static>(
        &self,
        handle: &mut PayloadHandle<Tx, Err, R>,
        overlay_factory: OverlayStateProviderFactory<Provider>,
        hashed_state: &HashedPostState,
        execute_height: u64,
    ) -> eyre::Result<Result<StateRootComputeOutcome, ParallelStateRootError>> {
        let Some(timeout) = self.tree_config.state_root_task_timeout() else {
            return Ok(handle.state_root());
        };

        let task_rx = handle.take_state_root_rx();

        match task_rx.recv_timeout(timeout) {
            Ok(result) => Ok(result),
            Err(RecvTimeoutError::Disconnected) => {
                Ok(Err(ParallelStateRootError::Other("sparse trie task dropped".to_string())))
            }
            Err(RecvTimeoutError::Timeout) => {
                warn!(
                    target: "flashblocks::validator",
                    execute_height,
                    ?timeout,
                    "State root task timed out, spawning sequential fallback"
                );

                let (race_tx, race_rx) = std::sync::mpsc::channel::<
                    eyre::Result<Result<StateRootComputeOutcome, ParallelStateRootError>>,
                >();

                // Bridge the original task receiver into the unified channel.
                let task_race_tx = race_tx.clone();
                self.payload_processor.executor().spawn_blocking(move || {
                    if let Ok(result) = task_rx.recv() {
                        debug!(
                            target: "flashblocks::validator",
                            source = "task",
                            "State root timeout race won"
                        );
                        let _ = task_race_tx.send(Ok(result));
                    }
                });

                // Spawn the sequential fallback.
                let seq_overlay = overlay_factory;
                let seq_hashed_state = hashed_state.clone();
                self.payload_processor.executor().spawn_blocking(move || {
                    let result = Self::compute_state_root_serial(seq_overlay, &seq_hashed_state);
                    debug!(
                        target: "flashblocks::validator",
                        source = "sequential",
                        "State root timeout race won"
                    );
                    let _ = race_tx.send(match result {
                        Ok((state_root, trie_updates)) => {
                            Ok(Ok(StateRootComputeOutcome { state_root, trie_updates }))
                        }
                        Err(e) => Err(e),
                    });
                });

                race_rx.recv().map_err(|_| {
                    eyre::eyre!(std::io::Error::other("both state root computations failed"))
                })?
            }
        }
    }

    /// Compute state root for the given hashed post state in parallel.
    ///
    /// Uses an overlay factory which provides the state of the parent block, along with the
    /// [`HashedPostState`] containing the changes of this block, to compute the state root and
    /// trie updates for this block.
    ///
    /// # Returns
    ///
    /// Returns `Ok(_)` if computed successfully.
    /// Returns `Err(_)` if error was encountered during computation.
    fn compute_state_root_parallel(
        &self,
        overlay_factory: OverlayStateProviderFactory<Provider>,
        hashed_state: &HashedPostState,
    ) -> eyre::Result<(B256, TrieUpdates), ParallelStateRootError> {
        // The `hashed_state` argument will be taken into account as part of the overlay, but we
        // need to use the prefix sets which were generated from it to indicate to the
        // ParallelStateRoot which parts of the trie need to be recomputed.
        let prefix_sets = hashed_state.construct_prefix_sets().freeze();
        let overlay_factory =
            overlay_factory.with_extended_hashed_state_overlay(hashed_state.clone_into_sorted());
        ParallelStateRoot::new(overlay_factory, prefix_sets, self.runtime.clone())
            .incremental_root_with_updates()
    }

    /// Compute state root for the given hashed post state in serial.
    ///
    /// Uses an overlay factory which provides the state of the parent block, along with the
    /// [`HashedPostState`] containing the changes of this block, to compute the state root and
    /// trie updates for this block.
    fn compute_state_root_serial(
        overlay_factory: OverlayStateProviderFactory<Provider>,
        hashed_state: &HashedPostState,
    ) -> eyre::Result<(B256, TrieUpdates)> {
        // The `hashed_state` argument will be taken into account as part of the overlay, but we
        // need to use the prefix sets which were generated from it to indicate to the
        // StateRoot which parts of the trie need to be recomputed.
        let prefix_sets = hashed_state.construct_prefix_sets().freeze();
        let overlay_factory =
            overlay_factory.with_extended_hashed_state_overlay(hashed_state.clone_into_sorted());

        let provider = overlay_factory.database_provider_ro()?;

        Ok(StateRoot::new(&provider, &provider)
            .with_prefix_sets(prefix_sets)
            .root_with_updates()?)
    }

    fn merge_suffix_results(
        cached_prefix: &PrefixExecutionMeta,
        cached_receipts: Vec<N::Receipt>,
        suffix_result: BlockExecutionResult<N::Receipt>,
    ) -> BlockExecutionResult<N::Receipt> {
        let mut receipts = cached_receipts;
        receipts.extend(suffix_result.receipts);

        // Use only suffix requests: the suffix executor's finish() produces
        // post-execution requests from the complete block state (cached prestate +
        // suffix changes). The cached prefix requests came from an intermediate
        // state and must not be merged.
        let requests = suffix_result.requests;
        BlockExecutionResult {
            receipts,
            requests,
            gas_used: cached_prefix.gas_used.saturating_add(suffix_result.gas_used),
            blob_gas_used: cached_prefix.blob_gas_used.saturating_add(suffix_result.blob_gas_used),
        }
    }

    #[expect(clippy::type_complexity)]
    fn state_provider_builder(
        &self,
        hash: B256,
    ) -> eyre::Result<(
        StateProviderBuilder<N, Provider>,
        SealedHeaderFor<N>,
        Option<(Vec<ExecutedBlock<N>>, B256)>,
    )> {
        // Get overlay data (executed blocks + parent header) from flashblocks
        // state cache and the canonical in-memory cache.
        if let Some((overlay_blocks, header, anchor_hash)) =
            self.flashblocks_state.get_overlay_data(&hash)?
        {
            debug!(
                target: "flashblocks::validator",
                %hash,
                "found state for block in flashblocks cache, creating provider builder");
            return Ok((
                StateProviderBuilder::new(
                    self.provider.clone(),
                    anchor_hash,
                    Some(overlay_blocks.clone()),
                ),
                header,
                Some((overlay_blocks, anchor_hash)),
            ));
        }
        // Check if block is persisted
        if let Some(header) = self.provider.sealed_header_by_hash(hash)? {
            debug!(
                target: "flashblocks::validator",
                %hash,
                "found state for block in database, creating provider builder");
            return Ok((
                StateProviderBuilder::new(self.provider.clone(), hash, None),
                header,
                None,
            ));
        }
        Err(eyre::eyre!("no state found for block {hash}"))
    }

    /// Creates a [`LazyOverlay`] for the parent block without blocking.
    ///
    /// Returns a lazy overlay that will compute the trie input on first access, and the anchor
    /// block hash (the highest persisted ancestor). This allows execution to start immediately
    /// while the trie input computation is deferred until the overlay is actually needed.
    ///
    /// If parent is on disk (no in-memory blocks), returns `(None, tip_hash)`.
    fn get_parent_lazy_overlay(
        overlay_data: Option<&(Vec<ExecutedBlock<N>>, B256)>,
        tip_hash: B256,
    ) -> (Option<LazyOverlay>, B256) {
        let Some((blocks, anchor)) = overlay_data else {
            return (None, tip_hash);
        };
        let anchor_hash = *anchor;

        if blocks.is_empty() {
            debug!(target: "flashblocks::validator", "Parent found on disk, no lazy overlay needed");
            return (None, anchor_hash);
        }

        // Extract deferred trie data handles (non-blocking)
        debug!(
            target: "flashblocks::validator",
            %anchor_hash,
            num_blocks = blocks.len(),
            "Creating lazy overlay for flashblock state cache in-memory blocks"
        );
        let handles: Vec<DeferredTrieData> = blocks.iter().map(|b| b.trie_data_handle()).collect();
        (Some(LazyOverlay::new(anchor_hash, handles)), anchor_hash)
    }

    /// Spawns a background task to compute and sort trie data for the executed block.
    ///
    /// This function creates a [`DeferredTrieData`] handle with fallback inputs and spawns a
    /// blocking task that calls `wait_cloned()` to:
    /// 1. Sort the block's hashed state and trie updates
    /// 2. Merge ancestor overlays and extend with the sorted data
    /// 3. Create an [`AnchoredTrieInput`](reth_chain_state::AnchoredTrieInput) for efficient future
    ///    trie computations
    /// 4. Cache the result so subsequent calls return immediately
    ///
    /// If the background task hasn't completed when `trie_data()` is called, `wait_cloned()`
    /// computes from the stored inputs, eliminating deadlock risk and duplicate computation.
    ///
    /// The validation hot path can return immediately after state root verification,
    /// while consumers (DB writes, overlay providers, proofs) get trie data either
    /// from the completed task or via fallback computation.
    fn spawn_deferred_trie_task(
        &self,
        block: RecoveredBlock<N::Block>,
        execution_outcome: Arc<BlockExecutionOutput<N::Receipt>>,
        hashed_state: HashedPostState,
        trie_output: TrieUpdates,
        overlay_data: Option<(Vec<ExecutedBlock<N>>, B256)>,
        overlay_factory: OverlayStateProviderFactory<Provider>,
    ) -> ExecutedBlock<N> {
        // Capture parent hash and ancestor overlays for deferred trie input construction.
        let (overlay_blocks, anchor_hash) =
            overlay_data.unwrap_or_else(|| (Vec::new(), block.parent_hash()));

        // Collect lightweight ancestor trie data handles. We don't call trie_data() here;
        // the merge and any fallback sorting happens in the compute_trie_input_task.
        let ancestors: Vec<DeferredTrieData> =
            overlay_blocks.iter().rev().map(|b| b.trie_data_handle()).collect();

        // Create deferred handle with fallback inputs in case the background task hasn't completed.
        let deferred_trie_data = DeferredTrieData::pending(
            Arc::new(hashed_state),
            Arc::new(trie_output),
            anchor_hash,
            ancestors,
        );
        let deferred_handle_task = deferred_trie_data.clone();

        // Capture block info and cache handle for changeset computation
        let block_hash = block.hash();
        let block_number = block.number();
        let changeset_cache = self.flashblocks_state.get_changeset_cache();

        // Spawn background task to compute trie data. Calling `wait_cloned` will compute from
        // the stored inputs and cache the result, so subsequent calls return immediately.
        // If this task panics, the `DeferredTrieData` fallback computes from stored inputs
        // on access, and the changeset cache miss is handled by `get_or_compute`.
        let compute_trie_input_task = move || {
            debug!(
                target: "flashblocks::changeset",
                ?block_number,
                "compute_trie_input_task",
            );

            let computed = deferred_handle_task.wait_cloned();
            // Compute and cache changesets using the computed trie_updates.
            // Get a provider from the overlay factory for trie cursor access.
            let changeset_start = Instant::now();
            let changeset_result = overlay_factory.database_provider_ro().and_then(|provider| {
                reth_trie::changesets::compute_trie_changesets(&provider, &computed.trie_updates)
                    .map_err(ProviderError::Database)
            });

            match changeset_result {
                Ok(changesets) => {
                    debug!(
                        target: "flashblocks::changeset",
                        ?block_number,
                        elapsed = ?changeset_start.elapsed(),
                        "Computed and caching changesets"
                    );
                    changeset_cache.insert(block_hash, block_number, Arc::new(changesets));
                }
                Err(e) => {
                    warn!(
                        target: "flashblocks::changeset",
                        ?block_number,
                        ?e,
                        "Failed to compute changesets in deferred trie task"
                    );
                }
            }
        };

        // Spawn task that computes trie data asynchronously.
        self.payload_processor.executor().spawn_blocking(compute_trie_input_task);

        ExecutedBlock::with_deferred_trie_data(
            Arc::new(block),
            execution_outcome,
            deferred_trie_data,
        )
    }

    #[allow(clippy::type_complexity)]
    fn flashblock_tx_iterator<T: Clone + Send + Sync + 'static>(
        transactions: Vec<WithEncoded<Recovered<T>>>,
    ) -> (
        Vec<WithEncoded<Recovered<T>>>,
        fn(WithEncoded<Recovered<T>>) -> Result<Recovered<T>, Infallible>,
    ) {
        (transactions, |tx| Ok(tx.1))
    }
}
