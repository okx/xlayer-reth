//! FR-1 (wiring) — node-builder `PoolBuilder` that installs [`XLayerBlacklistTxValidator`].
//!
//! Reproduces op-reth `OpPoolBuilder::build_pool` (`node/src/node.rs:1063-1140`) verbatim
//! and inserts a single change: the constructed `OpTransactionValidator` is wrapped in
//! [`XLayerBlacklistTxValidator`] before being handed to `TxPoolBuilder::with_validator`.
//! The resulting `Self::Pool` therefore parameterises the task executor on the wrapper
//! validator instead of the bare `OpTransactionValidator`.
//!
//! `PoolBuilder<Node, Evm>` trait — reth `crates/node/builder/src/components/pool.rs:16`:
//! `type Pool: TransactionPool<…> + Unpin + 'static; fn build_pool(self, ctx, evm_config: Evm)`.
//!
//! NOTE: this module touches the widest internal-API surface (blob store, task executor,
//! supervisor client, maintenance tasks) and is the primary compile-verification target for
//! stage 3.1 (it cannot be built in the memory-limited implementation stage). Every call is
//! grounded in the cited op-reth source.

use crate::runtime::BlacklistRuntimeCtx;
use crate::validator::XLayerBlacklistTxValidator;
use reth_chainspec::EthChainSpec;
use reth_node_api::{NodeTypes, TxTy};
use reth_node_builder::{components::PoolBuilder, BuilderContext, FullNodeTypes, PrimitivesTy};
use reth_optimism_forks::{OpHardfork, OpHardforks};
use reth_optimism_node::node::OpPoolBuilder;
use reth_optimism_txpool::{
    supervisor::SupervisorClient, OpPool, OpPooledTx, OpTransactionValidator,
};
use reth_primitives_traits::NodePrimitives;
use reth_transaction_pool::{
    blobstore::DiskFileBlobStore, CoinbaseTipOrdering, EthPoolTransaction, Pool,
    TransactionValidationTaskExecutor, TransactionValidator,
};

/// Node-builder pool component that wraps the OP transaction pool's validator with the
/// chain-level blacklist ingress check (FR-1). Constructed from the chain-keyed runtime
/// context so the validator and pool maintenance task share one snapshot handle.
#[derive(Debug)]
pub struct XLayerBlacklistPoolBuilder<T = reth_optimism_txpool::OpPooledTransaction> {
    inner: OpPoolBuilder<T>,
    ctx: BlacklistRuntimeCtx,
}

impl<T> XLayerBlacklistPoolBuilder<T> {
    /// Wrap an existing [`OpPoolBuilder`] with the blacklist runtime context.
    pub fn new(inner: OpPoolBuilder<T>, ctx: BlacklistRuntimeCtx) -> Self {
        Self { inner, ctx }
    }
}

impl<Node, T, Evm> PoolBuilder<Node, Evm> for XLayerBlacklistPoolBuilder<T>
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: OpHardforks>>,
    T: EthPoolTransaction<Consensus = TxTy<Node::Types>> + OpPooledTx,
    Evm: reth_evm::ConfigureEvm<Primitives = PrimitivesTy<Node::Types>> + Clone + 'static,
    // The inner `OpTransactionValidator` is a `TransactionValidator` over the node's tx and
    // block types; the wrapper's blanket impl then yields the same associated types, so the
    // task executor / pool accept it in place of the bare validator.
    OpTransactionValidator<Node::Provider, T, Evm>: TransactionValidator<
        Transaction = T,
        Block = <PrimitivesTy<Node::Types> as NodePrimitives>::Block,
    >,
{
    // Same pool shape as op-reth's `OpTransactionPool` alias, but with the wrapper validator
    // swapped in for the bare `OpTransactionValidator` inside the task executor.
    type Pool = OpPool<
        Pool<
            TransactionValidationTaskExecutor<
                XLayerBlacklistTxValidator<OpTransactionValidator<Node::Provider, T, Evm>>,
            >,
            CoinbaseTipOrdering<T>,
            DiskFileBlobStore,
        >,
    >;

    async fn build_pool(
        self,
        ctx: &BuilderContext<Node>,
        evm_config: Evm,
    ) -> eyre::Result<Self::Pool> {
        let Self { inner, ctx: bl_ctx } = self;
        let OpPoolBuilder {
            pool_config_overrides, supervisor_http, supervisor_safety_level, ..
        } = inner;

        // supervisor used for interop txpool validation (verbatim from op-reth)
        let supervisor_client = if let Some(url) = supervisor_http.clone() {
            Some(
                SupervisorClient::builder(url, ctx.chain_spec().chain_id())
                    .minimum_safety(supervisor_safety_level)
                    .build()
                    .await,
            )
        } else {
            None
        };

        let blob_store = reth_node_builder::components::create_blob_store(ctx)?;
        let validator =
            TransactionValidationTaskExecutor::eth_builder(ctx.provider().clone(), evm_config)
                .no_eip4844()
                .with_max_tx_input_bytes(ctx.config().txpool.max_tx_input_bytes)
                .kzg_settings(ctx.kzg_settings()?)
                .set_tx_fee_cap(ctx.config().rpc.rpc_tx_fee_cap)
                .with_max_tx_gas_limit(ctx.config().txpool.max_tx_gas_limit)
                .with_minimum_priority_fee(ctx.config().txpool.minimum_priority_fee)
                .with_additional_tasks(
                    pool_config_overrides
                        .additional_validation_tasks
                        .unwrap_or_else(|| ctx.config().txpool.additional_validation_tasks),
                )
                .build_with_tasks(ctx.task_executor().clone(), blob_store.clone())
                .map(|validator| {
                    let v = OpTransactionValidator::new(validator)
                        .require_l1_data_gas_fee(!ctx.config().dev.dev);
                    let op_validator = if let Some(client) = supervisor_client.clone() {
                        v.with_supervisor(client)
                    } else {
                        v
                    };
                    // FR-1: the single XLayer change — wrap the OP validator with the blacklist
                    // ingress check, sharing the chain-keyed snapshot handle + metrics.
                    XLayerBlacklistTxValidator::new(op_validator, bl_ctx.clone())
                });

        let final_pool_config = pool_config_overrides.apply(ctx.pool_config());
        let inner_pool = reth_node_builder::components::TxPoolBuilder::new(ctx)
            .with_validator(validator)
            .build(blob_store, final_pool_config.clone());

        let interop_filter_enabled = ctx.chain_spec().op_fork_activation(OpHardfork::Interop)
            != reth_chainspec::ForkCondition::Never;
        let transaction_pool =
            reth_optimism_txpool::OpPool::new(inner_pool, interop_filter_enabled);

        reth_node_builder::components::spawn_maintenance_tasks(
            ctx,
            transaction_pool.clone(),
            &final_pool_config,
        )?;

        Ok(transaction_pool)
    }
}
