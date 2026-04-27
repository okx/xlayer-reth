//! Encode-once carrier types for the flashblocks broadcast pipeline.
//!
//! When the sequencer produces a flashblock, we serialize+compress it exactly
//! once and pass the resulting bytes through both fan-out paths (P2P + WS).
//! When a relay node receives a flashblock, it decodes once for internal
//! consumers (validator, persistence) but forwards the original bytes to its
//! downstream subscribers without re-encoding.

use std::{io, sync::Arc};

use alloy_primitives::Bytes;
use serde::{de::DeserializeOwned, Serialize};

use crate::broadcast::{compress, types::Message};

/// Wire-ready bytes paired with their decoded form.
#[derive(Clone, Debug)]
pub struct BroadcastFrame {
    /// Brotli-compressed JSON, ready to send on the WS pipe or libp2p stream.
    pub bytes: Bytes,
    /// Already-decoded structured form, for in-process consumers.
    pub decoded: Arc<Message>,
}

impl BroadcastFrame {
    /// Decode wire bytes into a complete frame.
    ///
    /// Auto-detects between brotli-compressed and legacy uncompressed JSON via
    /// [`compress::try_decompress`]. The original `bytes` are retained on the
    /// frame so relay paths can forward them downstream without re-encoding.
    pub fn from_bytes(bytes: Bytes) -> io::Result<Self> {
        let decoded: Message = decode(&bytes)?;
        Ok(Self { bytes, decoded: Arc::new(decoded) })
    }
}

/// Serialize `value` to JSON and brotli-compress the result.
///
/// Single encode path for any wire envelope on the broadcast layer (P2P
/// [`Message`], WS `XLayerFlashblockMessage`, etc).
pub fn encode<T: Serialize>(value: &T) -> io::Result<Bytes> {
    let json = serde_json::to_vec(value)?;
    compress::brotli_encode(&json)
}

/// Decode wire bytes into `T`, auto-detecting compressed vs. legacy JSON.
///
/// Single decode path for any wire envelope. The auto-detection (leading-`{`
/// heuristic) lives in [`compress::try_decompress`], so a producer that emits
/// compressed bytes and a producer that emits legacy uncompressed JSON can
/// both be consumed without coordination.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    let json = compress::try_decompress(bytes)?;
    serde_json::from_slice::<T>(&json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("decode: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::{XLayerFlashblockMessage, XLayerFlashblockPayload};

    fn sample_message() -> Message {
        Message::from_flashblock_payload(XLayerFlashblockMessage::from_flashblock_payload(
            XLayerFlashblockPayload::default(),
        ))
    }

    #[test]
    fn encode_decode_round_trip() {
        let original = sample_message();
        let bytes = encode(&original).expect("encode");
        let decoded: Message = decode(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn encode_decode_round_trip_xlayer_flashblock_message() {
        // The same encode/decode path works for any Serialize/DeserializeOwned type —
        // the WS-side stream uses this with XLayerFlashblockMessage.
        let original =
            XLayerFlashblockMessage::from_flashblock_payload(XLayerFlashblockPayload::default());
        let bytes = encode(&original).expect("encode");
        let decoded: XLayerFlashblockMessage = decode(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn from_bytes_round_trip() {
        let original = sample_message();
        let bytes = encode(&original).expect("encode");
        let frame = BroadcastFrame::from_bytes(bytes.clone()).expect("from_bytes");
        assert_eq!(*frame.decoded, original);
        // Frame retains the original bytes byte-for-byte for relay.
        assert_eq!(frame.bytes, bytes);
    }

    #[test]
    fn from_bytes_accepts_legacy_uncompressed_json() {
        let original = sample_message();
        let json = Bytes::from(serde_json::to_vec(&original).expect("json"));
        // Legacy callers might send uncompressed JSON — must still decode.
        let frame = BroadcastFrame::from_bytes(json).expect("decode legacy");
        assert_eq!(*frame.decoded, original);
    }

    #[test]
    fn encoded_bytes_are_not_legacy_json() {
        // The encoded bytes must not start with `{` so that the auto-detect heuristic
        // routes them through brotli decompression on the receive side.
        let original = sample_message();
        let bytes = encode(&original).expect("encode");
        assert!(
            !bytes.starts_with(b"{"),
            "compressed frame must not look like legacy JSON to the auto-detect heuristic"
        );
    }
}
