use crate::{LegacyRpcRouterConfig, LegacyRpcRouterService};
use reqwest::Client;
use std::sync::Arc;
use tower::Layer;

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
        Self { config: Arc::new(config), client }
    }
}

impl<S> Layer<S> for LegacyRpcRouterLayer {
    type Service = LegacyRpcRouterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LegacyRpcRouterService { inner, config: self.config.clone(), client: self.client.clone() }
    }
}
