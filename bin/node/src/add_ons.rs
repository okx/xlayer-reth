use std::sync::Arc;

use reth_node_api::{AddOnsContext, FullNodeComponents, NodeAddOns};
use reth_node_builder::rpc::{
    EngineValidatorAddOn, EthApiBuilder, RethRpcAddOns, RpcHandle, RpcHooks,
};
use reth_optimism_node::OpAddOns;
use xlayer_legacy_rpc::LegacyRpcRouterConfig;
use xlayer_monitor::{ConsensusListener, XLayerMonitor};

/// Wrapper for RpcAddOns that initializes XLayer monitor with engine events
pub struct XLayerAddOns<Inner> {
    inner: Inner,
    monitor: Arc<XLayerMonitor>,
}

impl<N, EthB, PVB, EB, EVB>
    XLayerAddOns<
        OpAddOns<
            N,
            EthB,
            PVB,
            EB,
            EVB,
            (xlayer_monitor::RpcMonitorLayer, xlayer_legacy_rpc::layer::LegacyRpcRouterLayer),
        >,
    >
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Create a new XLayerAddOns with RPC middleware configured
    pub fn new(
        base_add_ons: OpAddOns<N, EthB, PVB, EB, EVB>,
        monitor: Arc<XLayerMonitor>,
        legacy_config: LegacyRpcRouterConfig,
    ) -> Self {
        let inner = base_add_ons.with_rpc_middleware((
            xlayer_monitor::RpcMonitorLayer::new(monitor.clone()), // Execute first
            xlayer_legacy_rpc::layer::LegacyRpcRouterLayer::new(legacy_config), // Execute second
        ));

        XLayerAddOns { inner, monitor }
    }
}

impl<N, Inner> NodeAddOns<N> for XLayerAddOns<Inner>
where
    N: FullNodeComponents,
    Inner: RethRpcAddOns<N>,
{
    type Handle = RpcHandle<N, Inner::EthApi>;

    async fn launch_add_ons(self, ctx: AddOnsContext<'_, N>) -> eyre::Result<Self::Handle> {
        // Initialize consensus engine events listener to track block received and canonical chain events
        let consensus_listener = ConsensusListener::new(self.monitor.clone());
        consensus_listener.listen(ctx.engine_events.new_listener(), ctx.node.task_executor());

        // Launch the inner add-ons
        self.inner.launch_add_ons(ctx).await
    }
}

impl<N, Inner> RethRpcAddOns<N> for XLayerAddOns<Inner>
where
    N: FullNodeComponents,
    Inner: RethRpcAddOns<N>,
{
    type EthApi = Inner::EthApi;

    fn hooks_mut(&mut self) -> &mut RpcHooks<N, Self::EthApi> {
        self.inner.hooks_mut()
    }
}

impl<N, Inner> EngineValidatorAddOn<N> for XLayerAddOns<Inner>
where
    N: FullNodeComponents,
    Inner: EngineValidatorAddOn<N>,
{
    type ValidatorBuilder = Inner::ValidatorBuilder;

    fn engine_validator_builder(&self) -> Self::ValidatorBuilder {
        self.inner.engine_validator_builder()
    }
}
