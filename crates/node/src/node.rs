//! XLayer Node implementation

use crate::payload_builder::XLayerPayloadBuilder as XLayerPayloadBuilderImpl;
use reth::{
    builder::{
        components::{BasicPayloadServiceBuilder, ComponentsBuilder, PayloadBuilderBuilder},
        rpc::BasicEngineValidatorBuilder,
        BuilderContext, FullNodeTypes, Node, NodeTypes,
    },
    providers::ChainSpecProvider,
};
use reth_node_api::{PayloadTypes, PrimitivesTy};
use reth_optimism_node::{
    args::RollupArgs,
    node::{
        OpAddOns, OpConsensusBuilder, OpExecutorBuilder, OpFullNodeTypes, OpNetworkBuilder, OpNode,
        OpNodeTypes, OpPoolBuilder,
    },
    OpBuiltPayload, OpDAConfig, OpEngineApiBuilder, OpPayloadPrimitives, OpStorage,
};
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpGasLimitConfig};
use reth_optimism_rpc::eth::OpEthApiBuilder;
use reth_transaction_pool::TransactionPool;

/// XLayer's payload builder - wraps OpPayloadBuilder with custom transaction execution
#[derive(Debug, Clone)]
pub struct XLayerPayloadBuilder<Txs = ()> {
    /// By default the pending block equals the latest block
    /// to save resources and not leak txs from the tx-pool,
    /// this flag enables computing of the pending block
    /// from the tx-pool instead.
    ///
    /// If `compute_pending_block` is not enabled, the payload builder
    /// will use the payload attributes from the latest block. Note
    /// that this flag is not yet functional.
    pub compute_pending_block: bool,
    /// The type responsible for yielding the best transactions for the payload if mempool
    /// transactions are allowed.
    pub best_transactions: Txs,
    /// This data availability configuration specifies constraints for the payload builder
    /// when assembling payloads
    pub da_config: OpDAConfig,
    /// Gas limit configuration for the OP builder.
    /// This is used to configure gas limit related constraints for the payload builder.
    pub gas_limit_config: OpGasLimitConfig,
}

impl XLayerPayloadBuilder {
    /// Create a new instance with the given `compute_pending_block` flag and data availability
    /// config.
    pub fn new(compute_pending_block: bool) -> Self {
        Self {
            compute_pending_block,
            best_transactions: (),
            da_config: OpDAConfig::default(),
            gas_limit_config: OpGasLimitConfig::default(),
        }
    }

    /// Configure the data availability configuration for the OP payload builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = da_config;
        self
    }

    /// Configure the gas limit configuration for the OP payload builder.
    pub fn with_gas_limit_config(mut self, gas_limit_config: OpGasLimitConfig) -> Self {
        self.gas_limit_config = gas_limit_config;
        self
    }
}

impl<Txs> XLayerPayloadBuilder<Txs> {
    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    #[allow(dead_code)]
    pub fn with_transactions<T>(self, best_transactions: T) -> XLayerPayloadBuilder<T> {
        let Self { compute_pending_block, da_config, gas_limit_config, .. } = self;
        XLayerPayloadBuilder {
            compute_pending_block,
            best_transactions,
            da_config,
            gas_limit_config,
        }
    }
}

/// XLayer Node type
#[derive(Debug, Clone, Default)]
pub struct XLayerNode {
    // inner for wrapping OpNode
    inner: OpNode,
}

impl XLayerNode {
    /// Creates a new XLayer node with the given rollup arguments
    pub fn new(args: RollupArgs) -> Self {
        Self { inner: OpNode::new(args.clone()) }
    }
}

impl<N> Node<N> for XLayerNode
where
    N: FullNodeTypes<Types: OpFullNodeTypes + OpNodeTypes>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        OpPoolBuilder,
        BasicPayloadServiceBuilder<XLayerPayloadBuilder>,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >;

    type AddOns = OpAddOns<
        reth::builder::NodeAdapter<
            N,
            <Self::ComponentsBuilder as reth::builder::NodeComponentsBuilder<N>>::Components,
        >,
        OpEthApiBuilder,
        reth_optimism_node::node::OpEngineValidatorBuilder,
        OpEngineApiBuilder<reth_optimism_node::node::OpEngineValidatorBuilder>,
        BasicEngineValidatorBuilder<reth_optimism_node::node::OpEngineValidatorBuilder>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        // Build components similar to OpNode but with XLayer payload builder
        let disable_txpool_gossip = self.inner.args.disable_txpool_gossip;
        let compute_pending_block = self.inner.args.clone().compute_pending_block;
        let discovery_v4 = self.inner.args.clone().discovery_v4;

        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(
                OpPoolBuilder::default()
                    .with_enable_tx_conditional(self.inner.args.enable_tx_conditional)
                    .with_supervisor(
                        self.inner.args.supervisor_http.clone(),
                        self.inner.args.supervisor_safety_level,
                    ),
            )
            .executor(OpExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::new(
                XLayerPayloadBuilder::new(compute_pending_block)
                    .with_da_config(self.inner.da_config.clone())
                    .with_gas_limit_config(self.inner.gas_limit_config.clone()),
            ))
            .network(OpNetworkBuilder::new(disable_txpool_gossip, !discovery_v4))
            .consensus(OpConsensusBuilder::default())
    }

    fn add_ons(&self) -> Self::AddOns {
        self.inner.add_ons()
    }
}

impl NodeTypes for XLayerNode {
    type Primitives = reth_optimism_primitives::OpPrimitives;
    type ChainSpec = reth_optimism_chainspec::OpChainSpec;
    type Storage = OpStorage;
    type Payload = reth_optimism_node::OpEngineTypes;
}

// Implement PayloadBuilderBuilder for XLayerPayloadBuilder
impl<Node, Pool, Evm, Attrs> PayloadBuilderBuilder<Node, Pool, Evm> for XLayerPayloadBuilder
where
    Node: FullNodeTypes<
        Provider: ChainSpecProvider<ChainSpec: reth_optimism_forks::OpHardforks>,
        Types: NodeTypes<
            Primitives: OpPayloadPrimitives,
            Payload: PayloadTypes<
                BuiltPayload = OpBuiltPayload<PrimitivesTy<Node::Types>>,
                PayloadBuilderAttributes = Attrs,
            >,
        >,
    >,
    Evm: reth::builder::ConfigureEvm<
            Primitives = PrimitivesTy<Node::Types>,
            NextBlockEnvCtx: reth_payload_primitives::BuildNextEnv<
                Attrs,
                reth_node_api::HeaderTy<Node::Types>,
                <Node::Types as NodeTypes>::ChainSpec,
            >,
        > + 'static,
    Pool: TransactionPool<
            Transaction: reth_optimism_txpool::OpPooledTx<
                Consensus = reth_node_api::TxTy<Node::Types>,
            >,
        > + Unpin
        + 'static,
    Attrs:
        reth_optimism_payload_builder::OpAttributes<Transaction = reth_node_api::TxTy<Node::Types>>,
{
    type PayloadBuilder = XLayerPayloadBuilderImpl<Pool, Node::Provider, Evm, (), Attrs>;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: Evm,
    ) -> eyre::Result<Self::PayloadBuilder> {
        let payload_builder = XLayerPayloadBuilderImpl::with_builder_config(
            pool,
            ctx.provider().clone(),
            evm_config,
            OpBuilderConfig {
                da_config: self.da_config.clone(),
                gas_limit_config: self.gas_limit_config.clone(),
            },
        )
        .with_transactions(self.best_transactions.clone())
        .set_compute_pending_block(self.compute_pending_block);
        Ok(payload_builder)
    }
}
