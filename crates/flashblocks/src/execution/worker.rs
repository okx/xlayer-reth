use crate::{
    cache::{CachedTxInfo, FlashblockStateCache, PendingSequence},
    execution::BuildArgs,
    FlashblockCachedReceipt,
};
use alloy_eips::eip2718::WithEncoded;
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::*;

use reth_chain_state::{ComputedTrieData, ExecutedBlock};
use reth_errors::RethError;
use reth_evm::{
    execute::{
        BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, BlockExecutor,
    },
    ConfigureEvm, Evm,
};
use reth_execution_types::{BlockExecutionOutput, BlockExecutionResult};
use reth_primitives_traits::{
    transaction::TxHashRef, BlockBody, HeaderTy, NodePrimitives, Recovered, RecoveredBlock,
};
use reth_revm::{
    cached::CachedReads,
    database::StateProviderDatabase,
    db::{states::bundle_state::BundleRetention, BundleState, State},
};
use reth_rpc_eth_types::PendingBlock;
use reth_storage_api::{
    HashedPostStateProvider, HeaderProvider, StateProviderFactory, StateRootProvider,
};
use reth_trie_common::HashedPostState;

/// Builds the [`PendingSequence`]s from the accumulated flashblock transaction sequences.
/// Commits results directly to [`FlashblockStateCache`] via `handle_pending_sequence()`.
///
/// Supports two execution modes:
/// - **Fresh**: Full execution for a new block height.
/// - **Incremental**: Suffix-only execution reusing cached prefix state from an existing
///   pending sequence at the same height.
#[derive(Debug)]
pub(crate) struct FlashblockSequenceValidator<N: NodePrimitives, EvmConfig, Provider>
where
    N::Receipt: FlashblockCachedReceipt,
{
    /// The EVM configuration used to build the flashblocks.
    evm_config: EvmConfig,
    /// The canonical chainstate provider.
    provider: Provider,
    /// The flashblocks state cache containing the flashblocks state cache layer.
    flashblocks_state: FlashblockStateCache<N>,
}

impl<N: NodePrimitives, EvmConfig, Provider> FlashblockSequenceValidator<N, EvmConfig, Provider>
where
    N::Receipt: FlashblockCachedReceipt,
{
    pub(crate) fn new(
        evm_config: EvmConfig,
        provider: Provider,
        flashblocks_state: FlashblockStateCache<N>,
    ) -> Self {
        Self { evm_config, provider, flashblocks_state }
    }

    pub(crate) const fn provider(&self) -> &Provider {
        &self.provider
    }
}

impl<N, EvmConfig, Provider> FlashblockSequenceValidator<N, EvmConfig, Provider>
where
    N: NodePrimitives,
    N::Receipt: FlashblockCachedReceipt,
    EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin>,
    Provider: StateProviderFactory + HeaderProvider<Header = HeaderTy<N>> + Unpin,
{
    /// Executes a flashblock transaction sequence and commits the result to the flashblocks
    /// state cache. Note that the flashblocks sequence validator should be the only handle
    /// that advances the flashblocks state cache tip.
    ///
    /// Determines execution mode from the current pending state:
    /// - No pending sequence exists, cache not yet initialized → fresh build.
    /// - Pending is at a different height → fresh build.
    /// - If pending exists at the same height → incremental build.
    pub(crate) fn execute<I: IntoIterator<Item = WithEncoded<Recovered<N::SignedTx>>>>(
        &mut self,
        args: BuildArgs<I>,
    ) -> eyre::Result<()> {
        let block_number = args.base.block_number;
        let transactions: Vec<_> = args.transactions.into_iter().collect();

        // Determine execution mode from pending state
        let pending = self.flashblocks_state.get_pending_sequence();
        let pending_height = pending.as_ref().map(|p| p.get_height());
        let incremental = pending_height == Some(block_number);

        // Validate height continuity
        if let Some(pending_height) = pending_height
            && block_number != pending_height
            && block_number != pending_height + 1
        {
            // State cache is polluted
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
    fn execute_fresh(
        &self,
        base: OpFlashblockPayloadBase,
        transactions: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        last_flashblock_index: u64,
    ) -> eyre::Result<()> {
        let parent_hash = base.parent_hash;

        // Prioritize trying to get parent hash state from canonical provider first. If
        // the parent is not in the canonical chain, then try building fresh on top of
        // the current pending sequence (current pending promoted to confirm, incoming
        // sequence is the next height). Fall back to the flashblocks overlay via
        // `get_pending_state_provider`.
        let (state_provider, parent_header) = match self.provider.history_by_block_hash(parent_hash)
        {
            Ok(canon_provider) => {
                let header = self
                    .provider
                    .sealed_header_by_hash(parent_hash)?
                    .ok_or_else(|| eyre::eyre!("parent header not found for hash {parent_hash}"))?;
                (canon_provider, header)
            }
            Err(err) => {
                trace!(
                    target: "flashblocks",
                    error = %err,
                    "parent not in canonical chain, try getting state from pending state",
                );
                let canonical_state = self.provider.latest()?;
                self.flashblocks_state.get_pending_state_provider(canonical_state).ok_or_else(
                    || {
                        eyre::eyre!(
                            "parent {parent_hash} not in canonical chain and no \
                             pending state available for overlay"
                        )
                    },
                )?
            }
        };

        let mut request_cache = CachedReads::default();
        let cached_db =
            request_cache.as_db_mut(StateProviderDatabase::new(state_provider.as_ref()));
        let mut state = State::builder().with_database(cached_db).with_bundle_update().build();

        let mut builder = self
            .evm_config
            .builder_for_next_block(&mut state, &parent_header, base.clone().into())
            .map_err(RethError::other)?;
        builder.apply_pre_execution_changes()?;
        for tx in &transactions {
            builder.execute_transaction(tx.clone())?;
        }
        let BlockBuilderOutcome { execution_result, block, hashed_state, .. } =
            builder.finish(state_provider.as_ref())?;
        let bundle = state.take_bundle();

        self.commit_pending_sequence(
            base,
            transactions,
            last_flashblock_index,
            execution_result,
            block,
            hashed_state,
            bundle,
            request_cache,
        )
    }

    /// Incremental execution for the same block height as the current pending. Reuses
    /// the pending sequence's `BundleState` as prestate and its warm `CachedReads`,
    /// executing only new unexecuted transactions from incremental flashblock payloads.
    fn execute_incremental(
        &self,
        base: OpFlashblockPayloadBase,
        transactions: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        last_flashblock_index: u64,
        pending: PendingSequence<N>,
    ) -> eyre::Result<()> {
        if pending.last_flashblock_index != last_flashblock_index {
            // State cache is polluted
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

        // Get latest canonical state, then overlay flashblocks state cache blocks
        // from canonical height up to the parent hash. This handles the case where
        // the parent is a flashblocks-confirmed block ahead of canonical.
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
        let prestate_bundle = exec_output.state.clone();
        let cached_tx_count =
            pending.pending.executed_block.recovered_block.body().transaction_count();
        let cached_receipts = exec_output.result.receipts.clone();
        let cached_gas_used = exec_output.result.gas_used;
        let cached_blob_gas_used = exec_output.result.blob_gas_used;

        // Set up state DB with pending's warm CachedReads + prestate bundle
        let mut request_cache = pending.cached_reads;
        let cached_db =
            request_cache.as_db_mut(StateProviderDatabase::new(state_provider.as_ref()));
        let mut state = State::builder()
            .with_database(cached_db)
            .with_bundle_prestate(prestate_bundle)
            .with_bundle_update()
            .build();

        let attrs = base.clone().into();
        let evm_env =
            self.evm_config.next_evm_env(&parent_header, &attrs).map_err(RethError::other)?;
        let execution_ctx = self
            .evm_config
            .context_for_next_block(&parent_header, attrs)
            .map_err(RethError::other)?;

        // Skip apply_pre_execution_changes (already applied in the original fresh build).
        // The only pre-execution effect we need is set_state_clear_flag, which configures EVM
        // empty-account handling (OP Stack chains activate Spurious Dragon at genesis, so
        // this is always true).
        state.set_state_clear_flag(true);
        let evm = self.evm_config.evm_with_env(&mut state, evm_env);
        let mut executor = self.evm_config.create_executor(evm, execution_ctx.clone());

        for tx in transactions.iter().skip(cached_tx_count).cloned() {
            executor.execute_transaction(tx)?;
        }

        let (evm, execution_result) = executor.finish()?;
        let (db, evm_env) = evm.finish();
        db.merge_transitions(BundleRetention::Reverts);

        let execution_result = Self::merge_cached_block_execution_results(
            cached_receipts,
            cached_gas_used,
            cached_blob_gas_used,
            execution_result,
        );

        // Compute state root via sparse trie
        let hashed_state = state_provider.hashed_post_state(&db.bundle_state);
        let (state_root, _) = state_provider
            .state_root_with_updates(hashed_state.clone())
            .map_err(RethError::other)?;
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
                &parent_header,
                block_transactions,
                &execution_result,
                &bundle,
                state_provider.as_ref(),
                state_root,
            ))
            .map_err(RethError::other)?;
        let block = RecoveredBlock::new_unhashed(block, senders);

        self.commit_pending_sequence(
            base,
            transactions,
            last_flashblock_index,
            execution_result,
            block,
            hashed_state,
            bundle,
            request_cache,
        )
    }

    /// Builds a [`PendingSequence`] and commits it to the flashblocks state cache.
    #[expect(clippy::too_many_arguments)]
    fn commit_pending_sequence(
        &self,
        base: OpFlashblockPayloadBase,
        transactions: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        last_flashblock_index: u64,
        execution_result: BlockExecutionResult<N::Receipt>,
        block: RecoveredBlock<N::Block>,
        hashed_state: HashedPostState,
        bundle: BundleState,
        request_cache: CachedReads,
    ) -> eyre::Result<()> {
        let block_hash = block.hash();
        let parent_hash = base.parent_hash;

        // Build pending execution block
        let execution_outcome =
            Arc::new(BlockExecutionOutput { state: bundle, result: execution_result });
        let executed_block = ExecutedBlock::new(
            block.into(),
            execution_outcome.clone(),
            ComputedTrieData::without_trie_input(
                Arc::new(hashed_state.into_sorted()),
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
            cached_reads: request_cache,
            block_hash,
            parent_hash,
            last_flashblock_index,
        })
    }

    fn merge_cached_block_execution_results(
        cached_receipts: Vec<N::Receipt>,
        cached_gas_used: u64,
        cached_blob_gas_used: u64,
        mut execution_result: BlockExecutionResult<N::Receipt>,
    ) -> BlockExecutionResult<N::Receipt> {
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
}
