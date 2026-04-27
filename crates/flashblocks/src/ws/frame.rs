//! Encode-once carrier for the RPC-side WS receive pipeline.
//!
//! The WS stream emits [`WsFrame`] items, each carrying both the original wire
//! bytes and the decoded [`XLayerFlashblockMessage`]. Internal consumers
//! (validator pipeline, persistence) read `decoded`; the relay-to-downstream
//! WS subscribers path passes `bytes` straight through to
//! [`xlayer_builder::broadcast::WebSocketPublisher::publish_raw`] without
//! re-encoding.

use std::{io, sync::Arc};

use alloy_primitives::Bytes;
use xlayer_builder::broadcast::{decode, Message, XLayerFlashblockMessage};

/// A received WS frame: the wire bytes paired with the decoded message.
#[derive(Clone, Debug)]
pub struct WsFrame {
    /// The original wire bytes (compressed or legacy uncompressed JSON).
    /// Use these when forwarding to downstream WS subscribers via
    /// [`xlayer_builder::broadcast::WebSocketPublisher::publish`].
    pub bytes: Bytes,
    /// The decoded structured message.
    pub decoded: Arc<XLayerFlashblockMessage>,
}

impl WsFrame {
    /// Decode wire bytes into a complete frame.
    pub fn from_bytes(bytes: Bytes) -> io::Result<Self> {
        let outer: Message = decode(&bytes)?;
        if let Message::OpFlashblockPayload(inner) = outer {
            Ok(Self { bytes, decoded: Arc::new(inner) })
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidData, "invalid message type on the WS pipe"))
        }
    }
}
