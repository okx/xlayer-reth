use std::sync::Arc;

use reqwest::Client;
use tower::Layer;
use tracing::info;

use crate::{LegacyRpcRouterConfig, LegacyRpcRouterService};

/// Layer that creates the routing middleware
#[derive(Clone)]
pub struct LegacyRpcRouterLayer {
    config: Arc<LegacyRpcRouterConfig>,
    client: Client,
}

impl LegacyRpcRouterLayer {
    pub fn new(config: LegacyRpcRouterConfig) -> Self {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .expect("Failed to create HTTP client");

        if config.enabled {
            info!(target:"xlayer_legacy_rpc", "xlayer legacy rpc enabled");
        }

        Self { config: Arc::new(config), client }
    }
}

impl<S> Layer<S> for LegacyRpcRouterLayer {
    type Service = LegacyRpcRouterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LegacyRpcRouterService { inner, config: self.config.clone(), client: self.client.clone() }
    }
}
