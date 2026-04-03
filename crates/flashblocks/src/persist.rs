use crate::ReceivedFlashblocksRx;
use std::{sync::Arc, time::Duration};
use tracing::*;

use reth_node_core::dirs::{ChainPath, DataDirPath};

use xlayer_builder::{
    broadcast::{WebSocketPublisher, XLayerFlashblockMessage},
    flashblocks::FlashblockPayloadsCache,
};

/// Handles the persistence of the pending flashblocks sequence to disk.
pub async fn handle_persistence(mut rx: ReceivedFlashblocksRx, datadir: ChainPath<DataDirPath>) {
    let cache = FlashblockPayloadsCache::new(Some(datadir));

    // Set default flush interval to 5 seconds
    let mut flush_interval = tokio::time::interval(Duration::from_secs(5));
    let mut dirty = false;

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(fb_payload) => {
                        if let XLayerFlashblockMessage::Payload(payload) = &*fb_payload
                            && let Err(e) =
                                cache.add_flashblock_payload(payload.inner.clone())
                        {
                            warn!(target: "flashblocks", "Failed to cache flashblock payload: {e}");
                            continue;
                        }
                        dirty = true;
                    }
                    Err(e) => {
                        warn!(target: "flashblocks", "Persistence handle receiver error: {e:?}");
                        break;
                    }
                }
            }
            _ = flush_interval.tick() => {
                if dirty {
                    if let Err(e) = cache.persist().await {
                        warn!(target: "flashblocks", "Failed to persist pending sequence: {e}");
                    }
                    dirty = false;
                }
            }
        }
    }

    // Flush again on shutdown
    if dirty && let Err(e) = cache.persist().await {
        warn!(target: "flashblocks", "Failed final persist of pending sequence: {e}");
    }

    info!(target: "flashblocks", "Flashblocks persistence handle stopped");
}

/// Handles the relaying of the flashblocks to the downstream flashblocks
/// websocket subscribers.
pub async fn handle_relay_flashblocks(
    mut rx: ReceivedFlashblocksRx,
    ws_pub: Arc<WebSocketPublisher>,
) {
    info!(
        target: "flashblocks",
        "Flashblocks websocket publisher started"
    );

    loop {
        match rx.recv().await {
            Ok(flashblock) => {
                if let Err(e) = ws_pub.publish(&flashblock) {
                    warn!(
                        target: "flashblocks",
                        "Failed to relay flashblock to websocket subscribers: {:?}", e
                    );
                }
            }
            Err(e) => {
                warn!(target: "flashblocks", "Flashblock receiver error: {:?}", e);
                break;
            }
        }
    }
    info!(target: "flashblocks", "Flashblocks service stopped");
}
