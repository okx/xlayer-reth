use alloy_consensus::{transaction::TxHashRef, BlockHeader};
use alloy_evm::block::CommitChanges;
use alloy_primitives::TxHash;
use reth::builder::components::PayloadServiceBuilder;
use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, BuildOutcomeKind, HeaderForPayload, MissingPayloadBehaviour,
    PayloadBuilder, PayloadConfig,
};
use reth_chainspec::ChainSpecProvider;
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome, BlockExecutionError, BlockExecutor, ExecutorTx},
    op_revm::constants::L1_BLOCK_CONTRACT,
    ConfigureEvm, Database, Evm as EvmTrait,
};
use reth_execution_types::BlockExecutionOutput;
use reth_node_api::NodeTypes;
use reth_node_builder::{
    components::{BasicPayloadServiceBuilder, PayloadBuilderBuilder},
    BuilderContext, FullNodeTypes, PayloadBuilderFor,
};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::node::OpPayloadBuilder;
use reth_optimism_payload_builder::{
    builder::{OpPayloadBuilderCtx, OpPayloadTransactions},
    config::{OpDAConfig, OpGasLimitConfig},
    OpAttributes, OpBuiltPayload, OpPayloadPrimitives,
};
use reth_optimism_txpool::OpPooledTx;
use reth_payload_builder_primitives::PayloadBuilderError;
use reth_payload_primitives::{BuildNextEnv, BuiltPayloadExecutedBlock};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_storage_api::{errors::ProviderError, StateProvider, StateProviderFactory};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::context::result::ExecutionResult;
use std::sync::Arc;
use tracing::{debug, warn};
use xlayer_bridge_intercept::{intercept_bridge_transaction_if_need, BridgeInterceptConfig};
use xlayer_builder::{
    args::BuilderArgs,
    flashblocks::{BuilderConfig, FlashblocksServiceBuilder},
    traits::{NodeBounds, PoolBounds},
};

// ---------------------------------------------------------------------------
// 1. XLayerBlockBuilder – wraps any `BlockBuilder` to intercept tx execution
// ---------------------------------------------------------------------------

/// A wrapper around a [`BlockBuilder`] that intercepts transaction execution
/// calls during payload building. This allows X Layer to add custom logic
/// (bridge interception) around transaction execution, recording intercepted
/// transaction hashes for permanent pool removal.
pub struct XLayerBlockBuilder<B> {
    inner: B,
    bridge_config: BridgeInterceptConfig,
    intercepted_hashes: Vec<TxHash>,
}

impl<B> XLayerBlockBuilder<B> {
    /// Create a new [`XLayerBlockBuilder`] wrapping the given builder.
    pub fn new(inner: B, bridge_config: BridgeInterceptConfig) -> Self {
        Self { inner, bridge_config, intercepted_hashes: Vec::new() }
    }

    /// Drain and return all transaction hashes that were intercepted (bridge
    /// transactions blocked during this build). Callers should remove these
    /// from the transaction pool permanently.
    pub fn take_intercepted_hashes(&mut self) -> Vec<TxHash> {
        core::mem::take(&mut self.intercepted_hashes)
    }
}

impl<B: BlockBuilder> BlockBuilder for XLayerBlockBuilder<B>
where
    B::Executor: BlockExecutor<Transaction: TxHashRef>,
{
    type Primitives = B::Primitives;
    type Executor = B::Executor;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(
            &ExecutionResult<
                <<Self::Executor as BlockExecutor>::Evm as EvmTrait>::HaltReason,
            >,
        ) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        let (_tx_env, recovered_tx) = tx.into_parts();
        let tx_hash = *recovered_tx.tx_hash();
        let signer = recovered_tx.signer();
        let bridge_config = self.bridge_config.clone();
        let mut intercepted = false;

        let result = self.inner.execute_transaction_with_commit_condition(
            recovered_tx,
            |exec_result| {
                let commit = f(exec_result);
                if commit == CommitChanges::No {
                    return CommitChanges::No;
                }
                // ExecutionResult::logs() returns &[] for Revert/Halt, so no
                // special-casing needed here.
                if intercept_bridge_transaction_if_need(
                    exec_result.logs(),
                    signer,
                    &bridge_config,
                )
                .is_err()
                {
                    intercepted = true;
                    return CommitChanges::No;
                }
                commit
            },
        );

        if intercepted {
            self.intercepted_hashes.push(tx_hash);
        }
        result
    }

    fn finish(
        self,
        state_provider: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<Self::Primitives>, BlockExecutionError> {
        self.inner.finish(state_provider)
    }

    fn executor_mut(&mut self) -> &mut Self::Executor {
        self.inner.executor_mut()
    }

    fn executor(&self) -> &Self::Executor {
        self.inner.executor()
    }

    fn into_executor(self) -> Self::Executor {
        self.inner.into_executor()
    }
}

// ---------------------------------------------------------------------------
// 2. XLayerPayloadBuilder – wraps OpPayloadBuilder, uses XLayerBlockBuilder
// ---------------------------------------------------------------------------

/// A payload builder that wraps [`reth_optimism_payload_builder::OpPayloadBuilder`]
/// and injects an [`XLayerBlockBuilder`] into the build flow to intercept
/// individual transaction execution.
#[derive(Debug)]
pub struct XLayerPayloadBuilder<Inner> {
    inner: Inner,
    bridge_config: BridgeInterceptConfig,
}

impl<Inner: Clone> Clone for XLayerPayloadBuilder<Inner> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), bridge_config: self.bridge_config.clone() }
    }
}

impl<Inner> XLayerPayloadBuilder<Inner> {
    /// Creates a new wrapper around the given payload builder.
    pub fn new(inner: Inner, bridge_config: BridgeInterceptConfig) -> Self {
        Self { inner, bridge_config }
    }
}

/// [`PayloadBuilder`] implementation for [`XLayerPayloadBuilder`] wrapping
/// [`reth_optimism_payload_builder::OpPayloadBuilder`].
///
/// This replicates the `OpBuilder::build()` flow but wraps the [`BlockBuilder`]
/// with [`XLayerBlockBuilder`] to intercept transaction execution.
impl<Pool, Client, Evm, N, Txs, Attrs> PayloadBuilder
    for XLayerPayloadBuilder<
        reth_optimism_payload_builder::OpPayloadBuilder<Pool, Client, Evm, Txs, Attrs>,
    >
where
    N: OpPayloadPrimitives,
    N::SignedTx: TxHashRef,
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
        let pool = self.inner.pool.clone();
        self.xlayer_build_payload(args, |attrs| {
            self.inner.best_transactions.best_transactions(pool, attrs)
        })
    }

    fn on_missing_payload(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        MissingPayloadBehaviour::AwaitInProgress
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, HeaderForPayload<Self::BuiltPayload>>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        let args = BuildArguments {
            config,
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };
        self.xlayer_build_payload(args, |_| {
            NoopPayloadTransactions::<Pool::Transaction>::default()
        })?
        .into_payload()
        .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

// Private build methods for XLayerPayloadBuilder
impl<Pool, Client, Evm, N, Txs, Attrs>
    XLayerPayloadBuilder<
        reth_optimism_payload_builder::OpPayloadBuilder<Pool, Client, Evm, Txs, Attrs>,
    >
where
    N: OpPayloadPrimitives,
    N::SignedTx: TxHashRef,
    Pool: TransactionPool,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: OpHardforks>,
    Evm: ConfigureEvm<
        Primitives = N,
        NextBlockEnvCtx: BuildNextEnv<Attrs, N::BlockHeader, Client::ChainSpec>,
    >,
    Attrs: OpAttributes<Transaction = N::SignedTx>,
{
    /// Mirrors `OpPayloadBuilder::build_payload` but uses [`XLayerBlockBuilder`].
    fn xlayer_build_payload<'a, BestTxs>(
        &self,
        args: BuildArguments<Attrs, OpBuiltPayload<N>>,
        best: impl FnOnce(BestTransactionsAttributes) -> BestTxs + Send + Sync + 'a,
    ) -> Result<BuildOutcome<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        BestTxs: PayloadTransactions<
            Transaction: PoolTransaction<Consensus = N::SignedTx> + OpPooledTx,
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

        let state_provider = self.inner.client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);

        if ctx.attributes().no_tx_pool() {
            self.xlayer_build(state, &state_provider, ctx, best)
        } else {
            self.xlayer_build(cached_reads.as_db_mut(state), &state_provider, ctx, best)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }

    /// Mirrors `OpBuilder::build` but wraps the [`BlockBuilder`] with
    /// [`XLayerBlockBuilder`] to intercept transaction execution and removes
    /// intercepted bridge transactions from the pool after building.
    fn xlayer_build<BestTxs>(
        &self,
        db: impl Database<Error = ProviderError>,
        state_provider: &impl StateProvider,
        ctx: OpPayloadBuilderCtx<Evm, Client::ChainSpec, Attrs>,
        best: impl FnOnce(BestTransactionsAttributes) -> BestTxs,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        BestTxs: PayloadTransactions<
            Transaction: PoolTransaction<Consensus = N::SignedTx> + OpPooledTx,
        >,
    {
        debug!(
            target: "xlayer::payload_builder",
            id=%ctx.payload_id(),
            parent_header = ?ctx.parent().hash(),
            parent_number = ctx.parent().number(),
            "building xlayer payload with wrapped block builder"
        );

        let mut db = State::builder().with_database(db).with_bundle_update().build();

        db.load_cache_account(L1_BLOCK_CONTRACT).map_err(BlockExecutionError::other)?;

        // Create the block builder and wrap it with XLayerBlockBuilder
        let inner_builder = ctx.block_builder(&mut db)?;
        let mut builder = XLayerBlockBuilder::new(inner_builder, self.bridge_config.clone());

        // 1. apply pre-execution changes
        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
            PayloadBuilderError::Internal(err.into())
        })?;

        // 2. execute sequencer transactions (through the wrapped builder)
        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;

        // 3. if mem pool transactions are requested we execute them
        if !ctx.attributes().no_tx_pool() {
            let best_txs = best(ctx.best_transaction_attributes(builder.evm_mut().block()));
            if ctx.execute_best_transactions(&mut info, &mut builder, best_txs)?.is_some() {
                return Ok(BuildOutcomeKind::Cancelled);
            }

            // Remove intercepted bridge transactions from the pool permanently so they
            // are not re-included in future blocks.
            let intercepted = builder.take_intercepted_hashes();
            if !intercepted.is_empty() {
                self.inner.pool.remove_transactions_and_descendants(intercepted);
            }

            if !ctx.is_better_payload(info.total_fees) {
                return Ok(BuildOutcomeKind::Aborted { fees: info.total_fees });
            }
        }

        let BlockBuilderOutcome { execution_result, hashed_state, trie_updates, block, .. } =
            builder.finish(state_provider)?;

        let sealed_block = Arc::new(block.sealed_block().clone());
        debug!(
            target: "payload_builder",
            id=%ctx.attributes().payload_id(),
            sealed_block_header = ?sealed_block.header(),
            "sealed built block"
        );

        let execution_outcome =
            BlockExecutionOutput { state: db.take_bundle(), result: execution_result };

        let executed: BuiltPayloadExecutedBlock<N> = BuiltPayloadExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_outcome),
            hashed_state: either::Either::Left(Arc::new(hashed_state)),
            trie_updates: either::Either::Left(Arc::new(trie_updates)),
        };

        let no_tx_pool = ctx.attributes().no_tx_pool();

        let payload =
            OpBuiltPayload::new(ctx.payload_id(), sealed_block, info.total_fees, Some(executed));

        if no_tx_pool {
            Ok(BuildOutcomeKind::Freeze(payload))
        } else {
            Ok(BuildOutcomeKind::Better { payload })
        }
    }
}

// ---------------------------------------------------------------------------
// 3. XLayerPayloadBuilderConfig – produces XLayerPayloadBuilder
// ---------------------------------------------------------------------------

/// Configuration for the X Layer payload builder. Implements [`PayloadBuilderBuilder`]
/// to construct [`XLayerPayloadBuilder`] instances.
pub struct XLayerPayloadBuilderConfig {
    /// Whether to compute the pending block.
    pub compute_pending_block: bool,
    /// The DA config for the payload builder.
    pub da_config: OpDAConfig,
    /// Gas limit configuration for the payload builder.
    pub gas_limit_config: OpGasLimitConfig,
    /// Bridge intercept configuration for transaction filtering.
    pub bridge_config: BridgeInterceptConfig,
}

impl XLayerPayloadBuilderConfig {
    /// Create a new config with the given pending block flag.
    pub fn new(compute_pending_block: bool) -> Self {
        Self {
            compute_pending_block,
            da_config: OpDAConfig::default(),
            gas_limit_config: OpGasLimitConfig::default(),
            bridge_config: BridgeInterceptConfig::default(),
        }
    }

    /// Set the DA config.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = da_config;
        self
    }

    /// Set the gas limit config.
    pub fn with_gas_limit_config(mut self, gas_limit_config: OpGasLimitConfig) -> Self {
        self.gas_limit_config = gas_limit_config;
        self
    }

}

impl<Node, Pool, Evm> PayloadBuilderBuilder<Node, Pool, Evm> for XLayerPayloadBuilderConfig
where
    Node: FullNodeTypes,
    Pool: TransactionPool,
    Evm: Send,
    OpPayloadBuilder: PayloadBuilderBuilder<Node, Pool, Evm>,
    XLayerPayloadBuilder<
        <OpPayloadBuilder as PayloadBuilderBuilder<Node, Pool, Evm>>::PayloadBuilder,
    >: PayloadBuilderFor<Node::Types> + Unpin + 'static,
{
    type PayloadBuilder = XLayerPayloadBuilder<
        <OpPayloadBuilder as PayloadBuilderBuilder<Node, Pool, Evm>>::PayloadBuilder,
    >;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm: Evm,
    ) -> eyre::Result<Self::PayloadBuilder> {
        let inner = OpPayloadBuilder::new(self.compute_pending_block)
            .with_da_config(self.da_config)
            .with_gas_limit_config(self.gas_limit_config);
        let inner_pb = inner.build_payload_builder(ctx, pool, evm).await?;
        Ok(XLayerPayloadBuilder::new(inner_pb, self.bridge_config))
    }
}

// ---------------------------------------------------------------------------
// 4. XLayerPayloadServiceBuilder – delegates to flashblocks or default
// ---------------------------------------------------------------------------

/// Payload builder strategy for X Layer.
enum XLayerPayloadServiceBuilderInner {
    /// Uses [`FlashblocksServiceBuilder`] for sequencer nodes producing flashblocks.
    Flashblocks(Box<FlashblocksServiceBuilder>),
    /// Stores an [`XLayerPayloadBuilderConfig`] for follower/RPC nodes; wrapped
    /// in [`BasicPayloadServiceBuilder`] at spawn time.
    Default(XLayerPayloadBuilderConfig),
}

/// The X Layer payload service builder that delegates to either [`FlashblocksServiceBuilder`]
/// or the default [`BasicPayloadServiceBuilder`] with an [`XLayerPayloadBuilder`].
pub struct XLayerPayloadServiceBuilder {
    builder: XLayerPayloadServiceBuilderInner,
}

impl XLayerPayloadServiceBuilder {
    pub fn new(
        xlayer_builder_args: BuilderArgs,
        compute_pending_block: bool,
    ) -> eyre::Result<Self> {
        Self::with_config(
            xlayer_builder_args,
            compute_pending_block,
            OpDAConfig::default(),
            OpGasLimitConfig::default(),
        )
    }

    pub fn with_config(
        xlayer_builder_args: BuilderArgs,
        compute_pending_block: bool,
        da_config: OpDAConfig,
        gas_limit_config: OpGasLimitConfig,
    ) -> eyre::Result<Self> {
        let builder = if xlayer_builder_args.flashblocks.enabled {
            let builder_config = BuilderConfig::try_from(xlayer_builder_args)?;
            XLayerPayloadServiceBuilderInner::Flashblocks(Box::new(FlashblocksServiceBuilder {
                config: builder_config,
                bridge_intercept: Default::default(),
            }))
        } else {
            let xlayer_config = XLayerPayloadBuilderConfig::new(compute_pending_block)
                .with_da_config(da_config)
                .with_gas_limit_config(gas_limit_config);
            XLayerPayloadServiceBuilderInner::Default(xlayer_config)
        };

        Ok(Self { builder })
    }

    /// Apply a [`BridgeInterceptConfig`] to the appropriate inner builder.
    /// For the default path the config is stored in [`XLayerPayloadBuilderConfig`].
    /// For the flashblocks path the config will be applied when T3 wires it in.
    pub fn with_bridge_config(mut self, config: BridgeInterceptConfig) -> Self {
        match &mut self.builder {
            XLayerPayloadServiceBuilderInner::Default(xlayer_config) => {
                xlayer_config.bridge_config = config;
            }
            XLayerPayloadServiceBuilderInner::Flashblocks(flashblocks_builder) => {
                flashblocks_builder.with_bridge_intercept(config);
            }
        }
        self
    }
}

impl<Node, Pool> PayloadServiceBuilder<Node, Pool, OpEvmConfig> for XLayerPayloadServiceBuilder
where
    Node: NodeBounds,
    Pool: PoolBounds,
    OpPayloadBuilder: PayloadBuilderBuilder<Node, Pool, OpEvmConfig>,
    XLayerPayloadBuilder<
        <OpPayloadBuilder as PayloadBuilderBuilder<Node, Pool, OpEvmConfig>>::PayloadBuilder,
    >: PayloadBuilderFor<Node::Types> + Unpin + 'static,
{
    async fn spawn_payload_builder_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: OpEvmConfig,
    ) -> eyre::Result<reth_payload_builder::PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>>
    {
        match self.builder {
            XLayerPayloadServiceBuilderInner::Flashblocks(flashblocks_builder) => {
                flashblocks_builder.spawn_payload_builder_service(ctx, pool, evm_config).await
            }
            XLayerPayloadServiceBuilderInner::Default(xlayer_config) => {
                BasicPayloadServiceBuilder::new(xlayer_config)
                    .spawn_payload_builder_service(ctx, pool, evm_config)
                    .await
            }
        }
    }
}
