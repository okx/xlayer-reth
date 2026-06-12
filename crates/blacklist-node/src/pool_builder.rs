//! FR-1 (wiring) — node-builder `PoolBuilder` that installs the blacklist ingress filter on
//! the standard `OpTransactionValidator` WITHOUT changing the pool/validator type.
//!
//! Reproduces op-reth `OpPoolBuilder::build_pool` (`node/src/node.rs:1063-1140`) verbatim and
//! inserts a single change: the constructed `OpTransactionValidator` gets the blacklist
//! ingress filter attached via `with_ingress_blacklist`. The resulting `Self::Pool` is the
//! SAME type as the upstream `OpTransactionPool` (no wrapper validator), so `N::Pool` is
//! unchanged and the OpAddOns / RPC stack still resolves (avoids the type-pinning wall, see
//! [[upstream-component-type-pinning]]).
//!
//! NOTE: this module touches the widest internal-API surface (blob store, task executor,
//! supervisor client, maintenance tasks). When bumping op-reth, diff this against the current
//! `OpPoolBuilder::build_pool` and re-sync.

use crate::runtime::BlacklistRuntimeCtx;
use crate::validator::XLayerIngressFilter;
use reth_chainspec::EthChainSpec;
use reth_node_api::{NodeTypes, TxTy};
use reth_node_builder::{components::PoolBuilder, BuilderContext, FullNodeTypes, PrimitivesTy};
use reth_optimism_forks::{OpHardfork, OpHardforks};
use reth_optimism_node::node::OpPoolBuilder;
use reth_optimism_txpool::{
    supervisor::SupervisorClient, IngressBlacklistFilter, OpPool, OpPooledTx,
    OpTransactionValidator,
};
use reth_primitives_traits::NodePrimitives;
use reth_transaction_pool::{
    blobstore::DiskFileBlobStore, CoinbaseTipOrdering, EthPoolTransaction, Pool,
    TransactionValidationTaskExecutor, TransactionValidator,
};
use std::sync::Arc;

/// Node-builder pool component that attaches the chain-level blacklist ingress filter to the
/// OP transaction pool's validator (FR-1), keeping the upstream pool/validator type.
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
    OpTransactionValidator<Node::Provider, T, Evm>: TransactionValidator<
        Transaction = T,
        Block = <PrimitivesTy<Node::Types> as NodePrimitives>::Block,
    >,
{
    // SAME pool shape as op-reth's `OpTransactionPool` alias — the bare `OpTransactionValidator`
    // (no wrapper), with the ingress filter attached as a field.
    type Pool = OpPool<
        Pool<
            TransactionValidationTaskExecutor<OpTransactionValidator<Node::Provider, T, Evm>>,
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
                    // FR-1: the single XLayer change — attach the blacklist ingress filter to
                    // the standard validator (same type, no wrapper), sharing the chain-keyed
                    // snapshot handle + metrics.
                    let filter: Arc<dyn IngressBlacklistFilter> =
                        Arc::new(XLayerIngressFilter::new(bl_ctx.clone()));
                    op_validator.with_ingress_blacklist(Some(filter))
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
