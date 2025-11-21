//! XLayer payload builder implementation
//!
//! This module provides XLayer-specific payload building functionality that extends
//! Optimism's payload builder with custom transaction execution logic.

use alloy_consensus::BlockHeader;
use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, BuildOutcomeKind, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig,
};
use reth_chainspec::{ChainSpecProvider, EthChainSpec};
use reth_evm::{execute::BlockBuilder, Database, Evm};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{OpBuiltPayload, OpPayloadPrimitives};
use reth_optimism_payload_builder::builder::{
    ExecutionInfo, OpPayloadBuilder, OpPayloadBuilderCtx, OpPayloadTransactions,
};
use reth_optimism_payload_builder::{config::OpBuilderConfig, OpAttributes};
use reth_optimism_txpool::OpPooledTx;
use reth_payload_primitives::{BuildNextEnv, PayloadBuilderError};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_primitives_traits::NodePrimitives;
use reth_revm::database::StateProviderDatabase;
use reth_storage_api::StateProviderFactory;
use reth_transaction_pool::{PoolTransaction, TransactionPool};
use tracing::{debug, warn};

/// XLayer's payload builder - wraps OpPayloadBuilder functionality with custom transaction execution
#[derive(Debug)]
pub struct XLayerPayloadBuilder<
    Pool,
    Client,
    Evm,
    Txs = (),
    Attrs = reth_optimism_payload_builder::OpPayloadBuilderAttributes<
        reth_node_api::TxTy<<Evm as reth_evm::ConfigureEvm>::Primitives>,
    >,
> {
    inner: OpPayloadBuilder<Pool, Client, Evm, Txs, Attrs>,
}

impl<Pool, Client, Evm, Txs, Attrs> Clone for XLayerPayloadBuilder<Pool, Client, Evm, Txs, Attrs>
where
    Pool: Clone,
    Client: Clone,
    Evm: reth_evm::ConfigureEvm + Clone,
    Txs: Clone,
{
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<Pool, Client, Evm, Attrs> XLayerPayloadBuilder<Pool, Client, Evm, (), Attrs> {
    /// Creates a new XLayerPayloadBuilder with the given config
    pub const fn with_builder_config(
        pool: Pool,
        client: Client,
        evm_config: Evm,
        config: OpBuilderConfig,
    ) -> Self {
        Self { inner: OpPayloadBuilder::with_builder_config(pool, client, evm_config, config) }
    }
}

impl<Pool, Client, Evm, Txs, Attrs> XLayerPayloadBuilder<Pool, Client, Evm, Txs, Attrs> {
    /// Sets the rollup's compute pending block configuration option.
    pub fn set_compute_pending_block(mut self, compute_pending_block: bool) -> Self {
        self.inner = self.inner.set_compute_pending_block(compute_pending_block);
        self
    }

    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    #[allow(dead_code)]
    pub fn with_transactions<T>(
        self,
        best_transactions: T,
    ) -> XLayerPayloadBuilder<Pool, Client, Evm, T, Attrs> {
        XLayerPayloadBuilder { inner: self.inner.with_transactions(best_transactions) }
    }
}

impl<Pool, Client, Evm, T, Attrs> XLayerPayloadBuilder<Pool, Client, Evm, T, Attrs>
where
    Pool: TransactionPool<
        Transaction: OpPooledTx<Consensus = <Evm::Primitives as NodePrimitives>::SignedTx>,
    >,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: OpHardforks>,
    Evm: reth_evm::ConfigureEvm<
        Primitives: OpPayloadPrimitives,
        NextBlockEnvCtx: BuildNextEnv<
            Attrs,
            <Evm::Primitives as NodePrimitives>::BlockHeader,
            Client::ChainSpec,
        >,
    >,
    Attrs: OpAttributes<Transaction = <Evm::Primitives as NodePrimitives>::SignedTx>,
{
    /// Constructs an XLayer payload using custom transaction execution logic
    fn build_payload<'a, Txs>(
        &self,
        args: BuildArguments<Attrs, OpBuiltPayload<Evm::Primitives>>,
        best: impl FnOnce(reth_transaction_pool::BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    ) -> Result<BuildOutcome<OpBuiltPayload<Evm::Primitives>>, PayloadBuilderError>
    where
        Txs: PayloadTransactions<
            Transaction: PoolTransaction<
                Consensus = <Evm::Primitives as NodePrimitives>::SignedTx,
            > + OpPooledTx,
        >,
    {
        let BuildArguments { mut cached_reads, config, cancel, best_payload } = args;

        let ctx = OpPayloadBuilderCtx {
            evm_config: self.inner.evm_config.clone(),
            builder_config: self.inner.config.clone(),
            chain_spec: self.inner.client.chain_spec(),
            config,
            cancel,
            best_payload,
        };

        let builder = XLayerBuilder::new(best);

        let state_provider = self.inner.client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);

        if ctx.attributes().no_tx_pool() {
            builder.build(state, &state_provider, ctx)
        } else {
            // sequencer mode we can reuse cached reads from previous runs
            builder.build(cached_reads.as_db_mut(state), &state_provider, ctx)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }
}

/// Implementation of the [`PayloadBuilder`] trait for [`XLayerPayloadBuilder`].
impl<Pool, Client, Evm, Txs, Attrs> PayloadBuilder
    for XLayerPayloadBuilder<Pool, Client, Evm, Txs, Attrs>
where
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: OpHardforks> + Clone,
    Pool: TransactionPool<
        Transaction: OpPooledTx<Consensus = <Evm::Primitives as NodePrimitives>::SignedTx>,
    >,
    Evm: reth_evm::ConfigureEvm<
        Primitives: OpPayloadPrimitives,
        NextBlockEnvCtx: BuildNextEnv<
            Attrs,
            <Evm::Primitives as NodePrimitives>::BlockHeader,
            Client::ChainSpec,
        >,
    >,
    Txs: OpPayloadTransactions<Pool::Transaction>,
    Attrs: OpAttributes<Transaction = <Evm::Primitives as NodePrimitives>::SignedTx>,
{
    type Attributes = Attrs;
    type BuiltPayload = OpBuiltPayload<Evm::Primitives>;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        let pool = self.inner.pool.clone();
        self.build_payload(args, |attrs| {
            self.inner.best_transactions.best_transactions(pool, attrs)
        })
    }

    fn on_missing_payload(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        // we want to await the job that's already in progress
        MissingPayloadBehaviour::AwaitInProgress
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, <Evm::Primitives as NodePrimitives>::BlockHeader>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        let args = BuildArguments {
            config,
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };
        self.build_payload(args, |_| NoopPayloadTransactions::<Pool::Transaction>::default())?
            .into_payload()
            .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

/// XLayer-specific builder that extends OpBuilder with custom transaction execution
struct XLayerBuilder<'a, Txs> {
    /// Yields the best transaction to include if transactions from the mempool are allowed.
    best: Box<dyn FnOnce(reth_transaction_pool::BestTransactionsAttributes) -> Txs + 'a>,
}

impl<'a, Txs> XLayerBuilder<'a, Txs> {
    /// Creates a new [`XLayerBuilder`].
    fn new(
        best: impl FnOnce(reth_transaction_pool::BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    ) -> Self {
        Self { best: Box::new(best) }
    }
}

impl<Txs> XLayerBuilder<'_, Txs> {
    /// Builds the payload on top of the state using XLayer-specific transaction execution.
    ///
    /// This is based on the OpBuilder::build implementation but uses execute_best_transactions_xlayer
    /// instead of execute_best_transactions.
    fn build<Evm, ChainSpec, N, Attrs>(
        self,
        db: impl Database<Error = reth_storage_api::errors::ProviderError>,
        state_provider: impl reth_storage_api::StateProvider,
        ctx: OpPayloadBuilderCtx<Evm, ChainSpec, Attrs>,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        Evm: reth_evm::ConfigureEvm<
            Primitives = N,
            NextBlockEnvCtx: BuildNextEnv<Attrs, N::BlockHeader, ChainSpec>,
        >,
        ChainSpec: EthChainSpec + OpHardforks,
        N: OpPayloadPrimitives,
        Txs:
            PayloadTransactions<Transaction: PoolTransaction<Consensus = N::SignedTx> + OpPooledTx>,
        Attrs: OpAttributes<Transaction = N::SignedTx>,
    {
        let Self { best } = self;
        debug!(target: "payload_builder", id=%ctx.payload_id(), parent_header = ?ctx.parent().hash(), "building new XLayer payload");

        let mut db = reth_revm::db::State::builder().with_database(db).with_bundle_update().build();

        // Load the L1 block contract into the database cache
        db.load_cache_account(reth_evm::op_revm::constants::L1_BLOCK_CONTRACT)
            .map_err(reth_evm::execute::BlockExecutionError::other)?;

        let mut builder = ctx.block_builder(&mut db)?;

        // 1. apply pre-execution changes
        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
            PayloadBuilderError::Internal(err.into())
        })?;

        // 2. execute sequencer transactions
        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;

        // 3. if mem pool transactions are requested we execute them with XLayer logic
        if !ctx.attributes().no_tx_pool() {
            let best_txs = best(ctx.best_transaction_attributes(builder.evm_mut().block()));

            // For XLayer, we execute the best transactions with XLayer-specific logic
            if ctx.execute_best_transactions_xlayer(&mut info, &mut builder, best_txs)?.is_some() {
                return Ok(BuildOutcomeKind::Cancelled);
            }

            // check if the new payload is even more valuable
            if !ctx.is_better_payload(info.total_fees) {
                // can skip building the block
                return Ok(BuildOutcomeKind::Aborted { fees: info.total_fees });
            }
        }

        let reth_evm::execute::BlockBuilderOutcome {
            execution_result,
            hashed_state,
            trie_updates,
            block,
        } = builder.finish(state_provider)?;

        let sealed_block = std::sync::Arc::new(block.sealed_block().clone());
        debug!(target: "payload_builder", id=%ctx.attributes().payload_id(), sealed_block_header = ?sealed_block.header(), "sealed built XLayer block");

        let execution_outcome = reth_execution_types::ExecutionOutcome::new(
            db.take_bundle(),
            vec![execution_result.receipts],
            block.number(),
            Vec::new(),
        );

        // create the executed block data
        let executed: reth_chain_state::ExecutedBlock<N> = reth_chain_state::ExecutedBlock {
            recovered_block: std::sync::Arc::new(block),
            execution_output: std::sync::Arc::new(execution_outcome),
            hashed_state: std::sync::Arc::new(hashed_state),
            trie_updates: std::sync::Arc::new(trie_updates),
        };

        let no_tx_pool = ctx.attributes().no_tx_pool();

        let payload =
            OpBuiltPayload::new(ctx.payload_id(), sealed_block, info.total_fees, Some(executed));

        if no_tx_pool {
            // if `no_tx_pool` is set only transactions from the payload attributes will be included
            // in the payload. In other words, the payload is deterministic and we can
            // freeze it once we've successfully built it.
            Ok(BuildOutcomeKind::Freeze(payload))
        } else {
            Ok(BuildOutcomeKind::Better { payload })
        }
    }
}

/// Extension trait for OpPayloadBuilderCtx to add XLayer-specific transaction execution
trait XLayerPayloadBuilderExt<Evm, ChainSpec, Attrs>
where
    Evm: reth_evm::ConfigureEvm,
    Evm::Primitives: NodePrimitives,
{
    /// Executes the given best transactions with XLayer-specific logic
    ///
    /// This is a placeholder implementation that currently delegates to the standard
    /// execute_best_transactions. In the future, this will contain XLayer-specific
    /// transaction execution logic such as:
    /// - Bridge intercept handling
    /// - Apollo transaction processing
    /// - Inner transaction tracking
    fn execute_best_transactions_xlayer<Builder>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        best_txs: impl PayloadTransactions<
            Transaction: PoolTransaction<
                Consensus = <Evm::Primitives as NodePrimitives>::SignedTx,
            > + OpPooledTx,
        >,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Builder: BlockBuilder<Primitives = Evm::Primitives>,
        <<Builder::Executor as reth_evm::execute::BlockExecutor>::Evm as reth_evm::Evm>::DB:
            reth_evm::Database;
}

impl<Evm, ChainSpec, Attrs> XLayerPayloadBuilderExt<Evm, ChainSpec, Attrs>
    for OpPayloadBuilderCtx<Evm, ChainSpec, Attrs>
where
    Evm: reth_evm::ConfigureEvm<
        Primitives: OpPayloadPrimitives,
        NextBlockEnvCtx: BuildNextEnv<
            Attrs,
            <Evm::Primitives as NodePrimitives>::BlockHeader,
            ChainSpec,
        >,
    >,
    ChainSpec: EthChainSpec + OpHardforks,
    Attrs: OpAttributes<Transaction = <Evm::Primitives as NodePrimitives>::SignedTx>,
{
    fn execute_best_transactions_xlayer<Builder>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        best_txs: impl PayloadTransactions<
            Transaction: PoolTransaction<
                Consensus = <Evm::Primitives as NodePrimitives>::SignedTx,
            > + OpPooledTx,
        >,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Builder: BlockBuilder<Primitives = Evm::Primitives>,
        <<Builder::Executor as reth_evm::execute::BlockExecutor>::Evm as reth_evm::Evm>::DB:
            reth_evm::Database,
    {
        // TODO: implement this according with: https://github.com/okx/reth/pull/21/files#diff-710e4d4d994e5f8e1f40327d4fe951470ac0e0fba52df866414e9135113a654fR56
        self.execute_best_transactions(info, builder, best_txs)
    }
}
