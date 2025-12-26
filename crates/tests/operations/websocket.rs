//! WebSocket client utilities for Ethereum JSON-RPC subscriptions

use eyre::Result;
use jsonrpsee::{
    core::client::{Subscription, SubscriptionClientT},
    ws_client::{WsClient, WsClientBuilder},
};
use serde_json::Value;

/// WebSocket client for Ethereum JSON-RPC subscriptions using jsonrpsee
pub struct EthWebSocketClient {
    client: WsClient,
}

impl EthWebSocketClient {
    /// Connect to a WebSocket endpoint
    pub async fn connect(url: &str) -> Result<Self> {
        let client = WsClientBuilder::default()
            .build(url)
            .await
            .map_err(|e| eyre::eyre!("Failed to connect to WebSocket: {}", e))?;
        Ok(Self { client })
    }

    /// Subscribe to Ethereum events using eth_subscribe
    ///
    /// Returns a subscription that yields raw JSON values.
    ///
    /// # Arguments
    /// * `event_type` - The subscription type (e.g., "newHeads", "logs", "newPendingTransactions", "flashblocks")
    /// * `params` - Optional parameters for the subscription
    pub async fn subscribe(
        &self,
        event_type: &str,
        params: Option<Value>,
    ) -> Result<Subscription<Value>> {
        let subscription = match params {
            Some(p) => {
                self.client
                    .subscribe(
                        "eth_subscribe",
                        jsonrpsee::rpc_params![event_type, p],
                        "eth_unsubscribe",
                    )
                    .await
            }
            None => {
                self.client
                    .subscribe(
                        "eth_subscribe",
                        jsonrpsee::rpc_params![event_type],
                        "eth_unsubscribe",
                    )
                    .await
            }
        }
        .map_err(|e| eyre::eyre!("Failed to subscribe to {}: {}", event_type, e))?;

        Ok(subscription)
    }

    /// Get a reference to the underlying jsonrpsee WsClient
    pub fn client(&self) -> &WsClient {
        &self.client
    }
}
