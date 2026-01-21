//! X Layer Engine API

use xlayer_monitor::{BlockInfo, XLayerMonitor};

use async_trait::async_trait;
use jsonrpsee::core::{server::RpcModule, RpcResult};
use std::sync::Arc;
use tracing::info;

use alloy_eips::eip7685::Requests;
use alloy_primitives::{BlockHash, B256, U64};
use alloy_rpc_types_engine::{
    ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadInputV2, ExecutionPayloadV3,
    ForkchoiceState, ForkchoiceUpdated, PayloadId, PayloadStatus,
};
use op_alloy_rpc_types_engine::{
    OpExecutionData, OpExecutionPayloadV4, ProtocolVersion, SuperchainSignal,
};
use reth_chainspec::EthereumHardforks;
use reth_node_api::{
    AddOnsContext, EngineApiValidator, EngineTypes, FullNodeComponents, NodeTypes,
};
use reth_node_builder::rpc::{EngineApiBuilder, PayloadValidatorBuilder};
use reth_optimism_node::OpEngineApiBuilder;
use reth_optimism_node::OpEngineValidatorBuilder;
use reth_optimism_rpc::{OpEngineApi, OpEngineApiServer};
use reth_rpc_api::IntoEngineApiRpcModule;
use reth_storage_api::{BlockReader, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;

/// Type alias for the inner OpEngineApi to reduce type complexity.
type InnerOpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec> =
    Arc<OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>>;

/// The X Layer Engine API handler that wraps OpEngineApi and adds custom logic for pre / post handling.
#[derive(Clone)]
pub struct XLayerEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
{
    /// The inner OpEngineApi.
    inner: InnerOpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>,
    /// The custom X Layer full link monitor.
    monitor: Arc<XLayerMonitor>,
}

impl<Provider, EngineT, Pool, Validator, ChainSpec>
    XLayerEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
{
    /// Create a new X Layer Engine API.
    pub fn new(
        inner: Arc<OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>>,
        monitor: Arc<XLayerMonitor>,
    ) -> Self {
        info!(target: "xlayer::engine-api", "XLayer Engine API initialized with monitor middleware");
        Self { inner, monitor }
    }
}

#[async_trait]
impl<Provider, EngineT, Pool, Validator, ChainSpec> OpEngineApiServer<EngineT>
    for XLayerEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
{
    async fn new_payload_v2(&self, payload: ExecutionPayloadInputV2) -> RpcResult<PayloadStatus> {
        // Call the full link monitor before execution
        let block_info = BlockInfo {
            block_number: payload.execution_payload.block_number,
            block_hash: payload.execution_payload.block_hash,
        };
        self.monitor.on_new_payload("v2", &block_info);

        self.inner.new_payload_v2(payload).await
    }

    async fn new_payload_v3(
        &self,
        payload: ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_beacon_block_root: B256,
    ) -> RpcResult<PayloadStatus> {
        // Call the full link monitor before execution
        let block_info = BlockInfo {
            block_number: payload.payload_inner.payload_inner.block_number,
            block_hash: payload.payload_inner.payload_inner.block_hash,
        };
        self.monitor.on_new_payload("v3", &block_info);

        self.inner.new_payload_v3(payload, versioned_hashes, parent_beacon_block_root).await
    }

    async fn new_payload_v4(
        &self,
        payload: OpExecutionPayloadV4,
        versioned_hashes: Vec<B256>,
        parent_beacon_block_root: B256,
        execution_requests: Requests,
    ) -> RpcResult<PayloadStatus> {
        // Call the full link monitor before execution
        let block_info = BlockInfo {
            block_number: payload.payload_inner.payload_inner.payload_inner.block_number,
            block_hash: payload.payload_inner.payload_inner.payload_inner.block_hash,
        };
        self.monitor.on_new_payload("v4", &block_info);

        self.inner
            .new_payload_v4(payload, versioned_hashes, parent_beacon_block_root, execution_requests)
            .await
    }

    async fn fork_choice_updated_v1(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        // Call the full link monitor before execution
        self.monitor.on_fork_choice_updated::<EngineT>(
            "v1",
            &fork_choice_state,
            &payload_attributes,
        );

        self.inner.fork_choice_updated_v1(fork_choice_state, payload_attributes).await
    }

    async fn fork_choice_updated_v2(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        // Call the full link monitor before execution
        self.monitor.on_fork_choice_updated::<EngineT>(
            "v2",
            &fork_choice_state,
            &payload_attributes,
        );

        self.inner.fork_choice_updated_v2(fork_choice_state, payload_attributes).await
    }

    async fn fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        // Call the full link monitor before execution
        self.monitor.on_fork_choice_updated::<EngineT>(
            "v3",
            &fork_choice_state,
            &payload_attributes,
        );

        self.inner.fork_choice_updated_v3(fork_choice_state, payload_attributes).await
    }

    async fn get_payload_v2(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<EngineT::ExecutionPayloadEnvelopeV2> {
        self.inner.get_payload_v2(payload_id).await
    }

    async fn get_payload_v3(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<EngineT::ExecutionPayloadEnvelopeV3> {
        self.inner.get_payload_v3(payload_id).await
    }

    async fn get_payload_v4(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<EngineT::ExecutionPayloadEnvelopeV4> {
        self.inner.get_payload_v4(payload_id).await
    }

    async fn get_payload_bodies_by_hash_v1(
        &self,
        block_hashes: Vec<BlockHash>,
    ) -> RpcResult<ExecutionPayloadBodiesV1> {
        self.inner.get_payload_bodies_by_hash_v1(block_hashes).await
    }

    async fn get_payload_bodies_by_range_v1(
        &self,
        start: U64,
        count: U64,
    ) -> RpcResult<ExecutionPayloadBodiesV1> {
        self.inner.get_payload_bodies_by_range_v1(start, count).await
    }

    async fn signal_superchain_v1(&self, signal: SuperchainSignal) -> RpcResult<ProtocolVersion> {
        self.inner.signal_superchain_v1(signal).await
    }

    async fn get_client_version_v1(
        &self,
        client: ClientVersionV1,
    ) -> RpcResult<Vec<ClientVersionV1>> {
        self.inner.get_client_version_v1(client).await
    }

    async fn exchange_capabilities(&self, capabilities: Vec<String>) -> RpcResult<Vec<String>> {
        self.inner.exchange_capabilities(capabilities).await
    }
}

impl<Provider, EngineT, Pool, Validator, ChainSpec> IntoEngineApiRpcModule
    for XLayerEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Self: OpEngineApiServer<EngineT>,
{
    fn into_rpc_module(self) -> RpcModule<()> {
        self.into_rpc().remove_context()
    }
}

pub struct XLayerEngineApiBuilder {
    pub monitor: Arc<XLayerMonitor>,
}

impl XLayerEngineApiBuilder {
    pub fn new(monitor: Arc<XLayerMonitor>) -> Self {
        Self { monitor }
    }
}

// Implement EngineApiBuilder to build XLayerEngineApi with OpEngineApi composed inside the handler.
// This eliminates the need for a separate middleware wrapper crate
//
// We use OpEngineValidatorBuilder directly here since we need a concrete type
impl<N> EngineApiBuilder<N> for XLayerEngineApiBuilder
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec: EthereumHardforks,
            Payload: EngineTypes<ExecutionData = OpExecutionData>,
        >,
    >,
    N::Provider: HeaderProvider + BlockReader + StateProviderFactory + Clone + Unpin + 'static,
    N::Pool: TransactionPool + 'static,
    OpEngineValidatorBuilder: PayloadValidatorBuilder<N>,
    <OpEngineValidatorBuilder as PayloadValidatorBuilder<N>>::Validator:
        EngineApiValidator<<N::Types as NodeTypes>::Payload>,
{
    type EngineApi = XLayerEngineApi<
        N::Provider,
        <N::Types as NodeTypes>::Payload,
        N::Pool,
        <OpEngineValidatorBuilder as PayloadValidatorBuilder<N>>::Validator,
        <N::Types as NodeTypes>::ChainSpec,
    >;

    async fn build_engine_api(self, ctx: &AddOnsContext<'_, N>) -> eyre::Result<Self::EngineApi> {
        let op_engine_builder = OpEngineApiBuilder::<OpEngineValidatorBuilder>::default();
        let op_engine_api = op_engine_builder.build_engine_api(ctx).await?;
        Ok(XLayerEngineApi::new(Arc::new(op_engine_api), self.monitor.clone()))
    }
}
