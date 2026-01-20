//! Engine API tracer middleware implementation

use crate::tracer::{BlockInfo, Tracer};
use alloy_eips::eip7685::Requests;
use alloy_primitives::{BlockHash, B256, U64};
use alloy_rpc_types_engine::{
    ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadInputV2, ExecutionPayloadV3,
    ForkchoiceState, ForkchoiceUpdated, PayloadId, PayloadStatus,
};
use async_trait::async_trait;
use jsonrpsee::core::{server::RpcModule, RpcResult};
use op_alloy_rpc_types_engine::{
    OpExecutionData, OpExecutionPayloadV4, ProtocolVersion, SuperchainSignal,
};
use reth_chainspec::EthereumHardforks;
use reth_node_api::{EngineApiValidator, EngineTypes};
use reth_optimism_rpc::{OpEngineApi, OpEngineApiServer};
use reth_rpc_api::IntoEngineApiRpcModule;
use reth_storage_api::{BlockReader, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use std::sync::Arc;
use tracing::trace;
use xlayer_engine_api::XLayerEngineApiMiddleware;

/// Type alias for the inner OpEngineApi to reduce type complexity.
type InnerOpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec> =
    Arc<OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>>;

/// Engine API tracer middleware that wraps OpEngineApi and traces all Engine API calls.
pub struct EngineApiTracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Args: Clone + Send + Sync + 'static,
{
    /// The inner OpEngineApi (set during build)
    inner: Option<InnerOpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>>,
    /// The tracer instance that handles events
    tracer: Arc<Tracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>>,
}

impl<Provider, EngineT, Pool, Validator, ChainSpec, Args>
    EngineApiTracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Args: Clone + Send + Sync + 'static,
{
    /// Create a new Engine API tracer with a tracer instance.
    pub(crate) fn new(
        tracer: Arc<Tracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>>,
    ) -> Self {
        Self { inner: None, tracer }
    }

    /// Set the inner OpEngineApi.
    pub fn set_inner(&mut self, inner: OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>) {
        self.inner = Some(Arc::new(inner));
    }

    /// Get the inner OpEngineApi.
    pub fn inner(&self) -> Option<&OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>> {
        self.inner.as_ref().map(|arc| arc.as_ref())
    }
}

impl<Provider, EngineT, Pool, Validator, ChainSpec, Args> Clone
    for EngineApiTracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Args: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), tracer: self.tracer.clone() }
    }
}

#[async_trait]
impl<Provider, EngineT, Pool, Validator, ChainSpec, Args> OpEngineApiServer<EngineT>
    for EngineApiTracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
    Args: Clone + Send + Sync + 'static,
{
    async fn new_payload_v2(&self, payload: ExecutionPayloadInputV2) -> RpcResult<PayloadStatus> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: new_payload_v2 called"
        );

        // Call the tracer before execution
        let block_info = BlockInfo {
            block_number: payload.execution_payload.block_number,
            block_hash: payload.execution_payload.block_hash,
        };
        self.tracer.on_new_payload("v2", &block_info);

        match self.inner() {
            Some(inner) => inner.new_payload_v2(payload).await,
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn new_payload_v3(
        &self,
        payload: ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_beacon_block_root: B256,
    ) -> RpcResult<PayloadStatus> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: new_payload_v3 called"
        );

        // Call the tracer before execution
        let block_info = BlockInfo {
            block_number: payload.payload_inner.payload_inner.block_number,
            block_hash: payload.payload_inner.payload_inner.block_hash,
        };
        self.tracer.on_new_payload("v3", &block_info);

        match self.inner() {
            Some(inner) => {
                inner.new_payload_v3(payload, versioned_hashes, parent_beacon_block_root).await
            }
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn new_payload_v4(
        &self,
        payload: OpExecutionPayloadV4,
        versioned_hashes: Vec<B256>,
        parent_beacon_block_root: B256,
        execution_requests: Requests,
    ) -> RpcResult<PayloadStatus> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: new_payload_v4 called"
        );

        // Call the tracer before execution
        let block_info = BlockInfo {
            block_number: payload.payload_inner.payload_inner.payload_inner.block_number,
            block_hash: payload.payload_inner.payload_inner.payload_inner.block_hash,
        };
        self.tracer.on_new_payload("v4", &block_info);

        match self.inner() {
            Some(inner) => {
                inner
                    .new_payload_v4(
                        payload,
                        versioned_hashes,
                        parent_beacon_block_root,
                        execution_requests,
                    )
                    .await
            }
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn fork_choice_updated_v1(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: fork_choice_updated_v1 - head={:?}, safe={:?}, finalized={:?}, has_attrs={}",
            fork_choice_state.head_block_hash,
            fork_choice_state.safe_block_hash,
            fork_choice_state.finalized_block_hash,
            payload_attributes.is_some()
        );

        // Call the tracer before execution
        self.tracer.on_fork_choice_updated("v1", &fork_choice_state, &payload_attributes);

        match self.inner() {
            Some(inner) => {
                inner.fork_choice_updated_v1(fork_choice_state, payload_attributes).await
            }
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn fork_choice_updated_v2(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: fork_choice_updated_v2 - head={:?}, safe={:?}, finalized={:?}, has_attrs={}",
            fork_choice_state.head_block_hash,
            fork_choice_state.safe_block_hash,
            fork_choice_state.finalized_block_hash,
            payload_attributes.is_some()
        );

        // Call the tracer before execution
        self.tracer.on_fork_choice_updated("v2", &fork_choice_state, &payload_attributes);

        match self.inner() {
            Some(inner) => {
                inner.fork_choice_updated_v2(fork_choice_state, payload_attributes).await
            }
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: fork_choice_updated_v3 - head={:?}, safe={:?}, finalized={:?}, has_attrs={}",
            fork_choice_state.head_block_hash,
            fork_choice_state.safe_block_hash,
            fork_choice_state.finalized_block_hash,
            payload_attributes.is_some()
        );

        // Call the tracer before execution
        self.tracer.on_fork_choice_updated("v3", &fork_choice_state, &payload_attributes);

        match self.inner() {
            Some(inner) => {
                inner.fork_choice_updated_v3(fork_choice_state, payload_attributes).await
            }
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn get_payload_v2(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<EngineT::ExecutionPayloadEnvelopeV2> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: get_payload_v2 called with id={:?}",
            payload_id
        );
        match self.inner() {
            Some(inner) => inner.get_payload_v2(payload_id).await,
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn get_payload_v3(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<EngineT::ExecutionPayloadEnvelopeV3> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: get_payload_v3 called with id={:?}",
            payload_id
        );
        match self.inner() {
            Some(inner) => inner.get_payload_v3(payload_id).await,
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn get_payload_v4(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<EngineT::ExecutionPayloadEnvelopeV4> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: get_payload_v4 called with id={:?}",
            payload_id
        );
        match self.inner() {
            Some(inner) => inner.get_payload_v4(payload_id).await,
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn get_payload_bodies_by_hash_v1(
        &self,
        block_hashes: Vec<BlockHash>,
    ) -> RpcResult<ExecutionPayloadBodiesV1> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: get_payload_bodies_by_hash_v1 called"
        );
        match self.inner() {
            Some(inner) => inner.get_payload_bodies_by_hash_v1(block_hashes).await,
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn get_payload_bodies_by_range_v1(
        &self,
        start: U64,
        count: U64,
    ) -> RpcResult<ExecutionPayloadBodiesV1> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: get_payload_bodies_by_range_v1 called with start={}, count={}",
            start,
            count
        );
        match self.inner() {
            Some(inner) => inner.get_payload_bodies_by_range_v1(start, count).await,
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn signal_superchain_v1(&self, signal: SuperchainSignal) -> RpcResult<ProtocolVersion> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: signal_superchain_v1 called"
        );
        match self.inner() {
            Some(inner) => inner.signal_superchain_v1(signal).await,
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn get_client_version_v1(
        &self,
        client: ClientVersionV1,
    ) -> RpcResult<Vec<ClientVersionV1>> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: get_client_version_v1 called"
        );
        match self.inner() {
            Some(inner) => inner.get_client_version_v1(client).await,
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }

    async fn exchange_capabilities(&self, capabilities: Vec<String>) -> RpcResult<Vec<String>> {
        trace!(
            target: "xlayer::full_trace::engine",
            "TRACE: exchange_capabilities called with {} capabilities",
            capabilities.len()
        );
        match self.inner() {
            Some(inner) => inner.exchange_capabilities(capabilities).await,
            None => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32603,
                "Inner engine API not set",
                None::<String>,
            )),
        }
    }
}

impl<Provider, EngineT, Pool, Validator, ChainSpec, Args> IntoEngineApiRpcModule
    for EngineApiTracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Args: Clone + Send + Sync + 'static,
    Self: OpEngineApiServer<EngineT>,
{
    fn into_rpc_module(self) -> RpcModule<()> {
        self.into_rpc().remove_context()
    }
}

// Implement XLayerEngineApiMiddleware trait for EngineApiTracer
impl<Provider, EngineT, Pool, Validator, ChainSpec, Args>
    XLayerEngineApiMiddleware<Provider, EngineT, Pool, Validator, ChainSpec>
    for EngineApiTracer<Provider, EngineT, Pool, Validator, ChainSpec, Args>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
    Args: Clone + Send + Sync + 'static,
{
    fn set_inner(&mut self, inner: OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>) {
        // Call the struct's set_inner method
        EngineApiTracer::set_inner(self, inner);
    }
}
