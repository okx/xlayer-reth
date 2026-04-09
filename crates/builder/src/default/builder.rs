//! Default payload builder wrapper with flashblocks cache replay.
//!
//! Wraps [`reth_optimism_payload_builder::OpPayloadBuilder`] and intercepts
//! `try_build` to check the [`FlashblockPayloadsCache`] before delegating.
//! On cache hit, cached transactions are replayed via `BlockBuilder::execute_transaction`
//! and the payload is returned as `Freeze` (deterministic, no re-building).

use crate::flashblocks::utils::cache::FlashblockPayloadsCache;

use alloy_consensus::{transaction::TxHashRef, Transaction, Typed2718};
use alloy_eips::eip2718::WithEncoded;
use alloy_evm::Evm as _;
use alloy_primitives::U256;
use op_alloy_consensus::OpTransaction;
use reth_basic_payload_builder::*;
use reth_chainspec::ChainSpecProvider;
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome, BlockExecutionError},
    op_revm::constants::L1_BLOCK_CONTRACT,
    ConfigureEvm,
};
use reth_execution_types::BlockExecutionOutput;
use reth_optimism_forks::OpHardforks;
use reth_optimism_payload_builder::{
    builder::OpPayloadBuilderCtx, builder::OpPayloadTransactions, error::OpPayloadBuilderError,
    payload::OpBuiltPayload, OpAttributes, OpPayloadPrimitives,
};
use reth_optimism_txpool::OpPooledTx;
use reth_payload_builder_primitives::PayloadBuilderError;
use reth_payload_primitives::{BuildNextEnv, BuiltPayloadExecutedBlock};
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_storage_api::StateProviderFactory;
use reth_transaction_pool::TransactionPool;
use revm::context::Block as _;
use std::sync::Arc;
use tracing::{info, warn};

/// The default optimism payload builder that wraps the additional builder p2p node
/// with flashblocks cache replay support. This is for flashblocks reorg protection
/// on failover to the default builder (when `flashblocks.enabled` = false).
///
/// When the P2P flashblocks sequence cache matches the current parent hash, cached
/// transactions are replayed instead of pulling from the txpool. Otherwise, payload
/// building is handled by the [`reth_optimism_payload_builder::OpPayloadBuilder`].
pub struct DefaultPayloadBuilder<Pool, Client, Evm, Txs, Attrs> {
    /// The inner default OP payload builder.
    inner: reth_optimism_payload_builder::OpPayloadBuilder<Pool, Client, Evm, Txs, Attrs>,
    /// Cache of flashblock payloads received via P2P, keyed by parent hash.
    p2p_cache: FlashblockPayloadsCache,
}

impl<Pool, Client, Evm, Txs, Attrs> DefaultPayloadBuilder<Pool, Client, Evm, Txs, Attrs> {
    /// Creates a new [`DefaultPayloadBuilder`] wrapping the given inner builder.
    pub fn new(
        inner: reth_optimism_payload_builder::OpPayloadBuilder<Pool, Client, Evm, Txs, Attrs>,
        p2p_cache: FlashblockPayloadsCache,
    ) -> Self {
        Self { inner, p2p_cache }
    }
}

impl<Pool: Clone, Client: Clone, Evm: ConfigureEvm + Clone, Txs: Clone, Attrs> Clone
    for DefaultPayloadBuilder<Pool, Client, Evm, Txs, Attrs>
{
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), p2p_cache: self.p2p_cache.clone() }
    }
}

impl<Pool, Client, Evm, N, Txs, Attrs> PayloadBuilder
    for DefaultPayloadBuilder<Pool, Client, Evm, Txs, Attrs>
where
    N: OpPayloadPrimitives,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: OpHardforks> + Clone,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = N::SignedTx>>,
    Evm: ConfigureEvm<
        Primitives = N,
        NextBlockEnvCtx: BuildNextEnv<Attrs, N::BlockHeader, Client::ChainSpec>,
    >,
    Txs: OpPayloadTransactions<Pool::Transaction>,
    Attrs: OpAttributes<Transaction = N::SignedTx>,
{
    type Attributes = Attrs;
    type BuiltPayload = OpBuiltPayload<N>;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        let parent_hash = args.config.parent_header.hash();

        // Check P2P cache for a flashblocks sequence matching this parent
        if let Some(cached_txs) = self
            .p2p_cache
            .get_flashblocks_sequence_txs::<N::SignedTx>(parent_hash)
            .filter(|txs| !txs.is_empty())
        {
            info!(
                target: "payload_builder",
                parent_hash = %parent_hash,
                cached_tx_count = cached_txs.len(),
                "Cache hit: replaying cached flashblocks transactions in default builder"
            );
            return self.build_with_cached_txs(args, cached_txs);
        }

        // Cache miss: delegate entirely to the inner OP builder
        self.inner.try_build(args)
    }

    fn on_missing_payload(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        self.inner.on_missing_payload(args)
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, N::BlockHeader>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        self.inner.build_empty_payload(config)
    }
}

impl<Pool, Client, Evm, N, Txs, Attrs> DefaultPayloadBuilder<Pool, Client, Evm, Txs, Attrs>
where
    N: OpPayloadPrimitives,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: OpHardforks> + Clone,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = N::SignedTx>>,
    Evm: ConfigureEvm<
        Primitives = N,
        NextBlockEnvCtx: BuildNextEnv<Attrs, N::BlockHeader, Client::ChainSpec>,
    >,
    Txs: OpPayloadTransactions<Pool::Transaction>,
    Attrs: OpAttributes<Transaction = N::SignedTx>,
{
    /// Builds a payload by replaying cached flashblocks transactions.
    ///
    /// Replicates the [`OpBuilder::build`] flow but replaces `execute_best_transactions`
    /// with direct cached transaction execution via [`BlockBuilder::execute_transaction`].
    fn build_with_cached_txs(
        &self,
        args: BuildArguments<Attrs, OpBuiltPayload<N>>,
        cached_txs: Vec<WithEncoded<alloy_consensus::transaction::Recovered<N::SignedTx>>>,
    ) -> Result<BuildOutcome<OpBuiltPayload<N>>, PayloadBuilderError> {
        let BuildArguments { mut cached_reads, config, cancel, best_payload } = args;

        let ctx = OpPayloadBuilderCtx {
            evm_config: self.inner.evm_config.clone(),
            builder_config: self.inner.config.clone(),
            chain_spec: self.inner.client.chain_spec(),
            config,
            cancel,
            best_payload,
        };

        let state_provider = self.inner.client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);
        let mut db = State::builder()
            .with_database(cached_reads.as_db_mut(state))
            .with_bundle_update()
            .build();

        // Pre-load L1 block contract into cache (required for DA footprint gas scalar)
        db.load_cache_account(L1_BLOCK_CONTRACT).map_err(BlockExecutionError::other)?;

        let mut builder = ctx.block_builder(&mut db)?;

        // 1. Apply pre-execution changes (system calls)
        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes during cache replay");
            PayloadBuilderError::Internal(err.into())
        })?;

        // 2. Execute sequencer transactions from payload attributes
        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;

        // 3. Execute cached transactions (replaces execute_best_transactions)
        let mut block_gas_limit = builder.evm_mut().block().gas_limit();
        if let Some(gas_limit_config) = ctx.builder_config.gas_limit_config.gas_limit() {
            // If a gas limit is configured, use that limit as target if it's smaller, otherwise use
            // the block's actual gas limit.
            block_gas_limit = gas_limit_config.min(block_gas_limit);
        }
        let block_da_limit = ctx.builder_config.da_config.max_da_block_size();
        let tx_da_limit = ctx.builder_config.da_config.max_da_tx_size();
        let base_fee = builder.evm_mut().block().basefee();

        // The execution result is discarded here since even on replay errors, we
        // will resolve the payload tillwhichever point the replay failed.
        let _ = execute_cached_transactions::<N, _>(
            &mut info,
            &mut builder,
            cached_txs,
            base_fee,
            block_gas_limit,
            tx_da_limit,
            block_da_limit,
        )
        .inspect_err(|e| {
            warn!(
                target: "payload_builder",
                "Failed replaying cached flashblocks sequence fully, error: {e}",
            );
        });

        // 4. Finish: compute state root, transaction root, receipt root
        let BlockBuilderOutcome { execution_result, hashed_state, trie_updates, block } =
            builder.finish(&state_provider)?;

        let sealed_block = Arc::new(block.sealed_block().clone());

        let execution_outcome =
            BlockExecutionOutput { state: db.take_bundle(), result: execution_result };

        let executed: BuiltPayloadExecutedBlock<N> = BuiltPayloadExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_outcome),
            hashed_state: either::Either::Left(Arc::new(hashed_state)),
            trie_updates: either::Either::Left(Arc::new(trie_updates)),
        };

        let payload =
            OpBuiltPayload::new(ctx.payload_id(), sealed_block, info.total_fees, Some(executed));

        info!(
            target: "payload_builder",
            id = %ctx.payload_id(),
            block_hash = %payload.block().hash(),
            "Successfully built payload from cached flashblocks transactions"
        );

        // Freeze: deterministic payload from cached sequence, no re-building needed
        Ok(BuildOutcomeKind::Freeze(payload).with_cached_reads(cached_reads))
    }
}

/// Executes cached flashblocks transactions via [`BlockBuilder::execute_transaction`].
fn execute_cached_transactions<N, Builder>(
    info: &mut reth_optimism_payload_builder::builder::ExecutionInfo,
    builder: &mut Builder,
    cached_txs: Vec<WithEncoded<alloy_consensus::transaction::Recovered<N::SignedTx>>>,
    base_fee: u64,
    block_gas_limit: u64,
    tx_da_limit: Option<u64>,
    block_da_limit: Option<u64>,
) -> Result<(), PayloadBuilderError>
where
    N: OpPayloadPrimitives,
    Builder: BlockBuilder<Primitives = N>,
{
    for tx_encoded in cached_txs {
        let (encoded_bytes, recovered_tx) = tx_encoded.split();

        // ensure transaction is valid
        let tx_da_size = op_alloy_flz::tx_estimated_size_fjord_bytes(encoded_bytes.as_ref());
        if info.is_tx_over_limits(
            tx_da_size,
            block_gas_limit,
            tx_da_limit,
            block_da_limit,
            recovered_tx.gas_limit(),
            None, // DA footprint gas scalar — skipped (requires L1BlockInfo DB fetch)
        ) {
            return Err(PayloadBuilderError::Other(
                eyre::eyre!(
                    "invalid flashblocks sequence, tx {} over block limits",
                    recovered_tx.tx_hash(),
                )
                .into(),
            ));
        }
        if recovered_tx.is_eip4844() {
            return Err(PayloadBuilderError::other(OpPayloadBuilderError::BlobTransactionRejected));
        }
        if recovered_tx.is_deposit() {
            return Err(PayloadBuilderError::Other(
                eyre::eyre!("invalid flashblocks sequence, deposit transaction rejected").into(),
            ));
        }

        // Ensure transaction execution is valid
        let gas_used = builder
            .execute_transaction(recovered_tx.clone())
            .map_err(|err| PayloadBuilderError::EvmExecutionError(Box::new(err)))?;

        // Track fees for payload metadata
        let miner_fee = recovered_tx
            .effective_tip_per_gas(base_fee)
            .expect("fee is always valid; execution succeeded");
        info.total_fees += U256::from(miner_fee) * U256::from(gas_used);
    }

    Ok(())
}
