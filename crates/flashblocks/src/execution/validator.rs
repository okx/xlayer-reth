use crate::{
    cache::{CachedTxInfo, FlashblockStateCache, PendingSequence},
    execution::{
        processor::{FlashblockSequenceProcessor, IncrementalPrestate, ProcessorOutcome},
        BuildArgs,
    },
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
use reth_evm::ConfigureEvm;
use reth_execution_types::BlockExecutionOutput;
use reth_primitives_traits::{
    transaction::TxHashRef, BlockBody, HeaderTy, NodePrimitives, Recovered,
};
use reth_rpc_eth_types::PendingBlock;
use reth_storage_api::{HeaderProvider, StateProviderFactory};

/// Builds the [`PendingSequence`]s from the accumulated flashblock transaction sequences.
/// Commits results directly to [`FlashblockStateCache`] via `handle_pending_sequence()`.
///
/// Supports two execution modes:
/// - **Fresh**: Full execution for a new block height.
/// - **Incremental**: Suffix-only execution reusing cached prefix state from an existing
///   pending sequence at the same height.
///
/// Delegates transaction execution and block assembly to
/// [`FlashblockSequenceProcessor`].
#[derive(Debug)]
pub(crate) struct FlashblockSequenceValidator<N: NodePrimitives, EvmConfig, Provider>
where
    N::Receipt: FlashblockCachedReceipt,
{
    /// Handles transaction execution, state root computation, and block assembly.
    processor: FlashblockSequenceProcessor<N, EvmConfig>,
    /// The state provider factory for resolving canonical and historical state.
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
        Self {
            processor: FlashblockSequenceProcessor::new(evm_config),
            provider,
            flashblocks_state,
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
    /// Resolves the state provider and parent header, then delegates execution to the
    /// processor.
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

        let outcome = self.processor.process_fresh(
            &base,
            &transactions,
            state_provider.as_ref(),
            &parent_header,
        )?;

        self.commit_pending_sequence(base, transactions, last_flashblock_index, outcome)
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
        // from canonical height up to the parent hash.
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

        let outcome = self.processor.process_incremental(
            &base,
            &transactions,
            state_provider.as_ref(),
            &parent_header,
            prestate,
        )?;

        self.commit_pending_sequence(base, transactions, last_flashblock_index, outcome)
    }

    /// Builds a [`PendingSequence`] from a [`ProcessorOutcome`] and commits it to the
    /// flashblocks state cache.
    fn commit_pending_sequence(
        &self,
        base: OpFlashblockPayloadBase,
        transactions: Vec<WithEncoded<Recovered<N::SignedTx>>>,
        last_flashblock_index: u64,
        outcome: ProcessorOutcome<N>,
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
