//! XLayer Engine API builder with middleware support

use crate::XLayerEngineApiMiddleware;
use op_alloy_rpc_types_engine::OpExecutionData;
use reth_chainspec::EthereumHardforks;
use reth_node_api::{AddOnsContext, EngineTypes, FullNodeComponents, NodeTypes};
use reth_node_builder::rpc::EngineApiBuilder;
use reth_optimism_rpc::OpEngineApi;
use tracing::info;

/// Builder for XLayer Engine API with optional middleware support.
pub struct XLayerEngineApiBuilder<OpBuilder, M = ()> {
    /// The inner Optimism Engine API builder
    inner_builder: OpBuilder,
    /// Optional middleware
    middleware: Option<M>,
}

impl<OpBuilder> XLayerEngineApiBuilder<OpBuilder, ()> {
    /// Create a new XLayer Engine API builder without middleware.
    pub fn new(inner_builder: OpBuilder) -> Self {
        Self { inner_builder, middleware: None }
    }
}

impl<OpBuilder, M> XLayerEngineApiBuilder<OpBuilder, M> {
    /// Add middleware using a factory function.
    ///
    /// The factory function is called immediately to create the middleware instance,
    /// which is stored in the builder. The inner OpEngineApi will be set
    /// during the build phase.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let builder = XLayerEngineApiBuilder::new(op_engine_builder)
    ///     .with_middleware(|| MyMiddleware::new());
    /// ```
    pub fn with_middleware<F, NewM>(self, factory: F) -> XLayerEngineApiBuilder<OpBuilder, NewM>
    where
        F: FnOnce() -> NewM,
        NewM: Send + Sync,
    {
        let middleware = factory();
        XLayerEngineApiBuilder { inner_builder: self.inner_builder, middleware: Some(middleware) }
    }
}

// Implementation for builder with middleware
impl<N, OpBuilder, Middleware, Validator> EngineApiBuilder<N>
    for XLayerEngineApiBuilder<OpBuilder, Middleware>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec: EthereumHardforks,
            Payload: EngineTypes<ExecutionData = OpExecutionData>,
        >,
    >,
    OpBuilder: EngineApiBuilder<
        N,
        EngineApi = OpEngineApi<
            N::Provider,
            <N::Types as NodeTypes>::Payload,
            N::Pool,
            Validator,
            <N::Types as NodeTypes>::ChainSpec,
        >,
    >,
    Middleware: XLayerEngineApiMiddleware<
        N::Provider,
        <N::Types as NodeTypes>::Payload,
        N::Pool,
        Validator,
        <N::Types as NodeTypes>::ChainSpec,
    >,
    Validator: reth_node_api::EngineApiValidator<<N::Types as NodeTypes>::Payload>,
{
    type EngineApi = Middleware;

    async fn build_engine_api(self, ctx: &AddOnsContext<'_, N>) -> eyre::Result<Self::EngineApi> {
        // Build the inner OpEngineApi
        let op_engine_api = self.inner_builder.build_engine_api(ctx).await?;

        // Get the middleware that was created by the factory
        let mut middleware =
            self.middleware.expect("middleware must be set when using this builder");

        info!(target: "xlayer::engine", "XLayer Engine API initialized with middleware");

        // Set the inner OpEngineApi on the middleware
        middleware.set_inner(op_engine_api);

        Ok(middleware)
    }
}
