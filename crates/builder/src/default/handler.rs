//! P2P message handler for the default builder's reorg protection.
//!
//! Routes incoming flashblock messages from P2P peers to the
//! [`FlashblockPayloadsCache`] for replay on failover, and optionally
//! relays them to downstream WebSocket subscribers.

use crate::{
    broadcast::{Message, WebSocketPublisher, XLayerFlashblockMessage},
    flashblocks::utils::cache::FlashblockPayloadsCache,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

/// Handles P2P-received flashblock messages for the default (non-flashblocks) builder.
///
/// Unlike [`FlashblocksPayloadHandler`](crate::flashblocks::handler), this handler
/// only accumulates incoming flashblocks into the cache and relays them via WebSocket.
/// It does not broadcast locally built payloads (the default builder does not produce
/// flashblocks).
pub(crate) struct DefaultPayloadHandler {
    /// Receives incoming P2P messages from peers.
    p2p_rx: mpsc::Receiver<Message>,
    /// Cache for externally received pending flashblock transactions.
    p2p_cache: FlashblockPayloadsCache,
    /// WebSocket publisher for relaying flashblocks to downstream subscribers.
    ws_pub: Arc<WebSocketPublisher>,
    /// Cancellation token for graceful shutdown.
    cancel: tokio_util::sync::CancellationToken,
}

impl DefaultPayloadHandler {
    pub(crate) fn new(
        p2p_rx: mpsc::Receiver<Message>,
        p2p_cache: FlashblockPayloadsCache,
        ws_pub: Arc<WebSocketPublisher>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self { p2p_rx, p2p_cache, ws_pub, cancel }
    }

    /// Runs the handler event loop until cancellation or channel close.
    pub(crate) async fn run(mut self) {
        tracing::info!(target: "payload_builder", "default builder P2P payload handler started");

        loop {
            tokio::select! {
                Some(message) = self.p2p_rx.recv() => {
                    match message {
                        Message::OpFlashblockPayload(fb_payload) => {
                            if let XLayerFlashblockMessage::Payload(payload) = &fb_payload &&
                                let Err(e) = self.p2p_cache.add_flashblock_payload(payload.inner.clone()) {
                                warn!(target: "payload_builder", e = ?e, "failed to add flashblock txs to cache");
                            }
                            if let Err(e) = self.ws_pub.publish(&fb_payload) {
                                warn!(target: "payload_builder", e = ?e, "failed to publish flashblock to websocket publisher");
                            }
                        }
                        Message::OpBuiltPayload(_) => {
                            // Full built payloads are not processed by the default builder handler
                        }
                    }
                }
                _ = self.cancel.cancelled() => {
                    tracing::info!(target: "payload_builder", "default builder P2P payload handler shutting down");
                    break;
                }
                else => break,
            }
        }
    }
}
