use op_alloy_rpc_types_engine::OpFlashblockPayload;
use reth::payload::PayloadId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct XLayerFlashblockPayload {
    #[serde(flatten)]
    pub inner: OpFlashblockPayload,
    /// The target flashblock index that the builder will build until. Default to zero if
    /// unset yet, for base flashblock payload.
    #[serde(default)]
    pub target_index: u64,
}

impl XLayerFlashblockPayload {
    pub fn new(inner: OpFlashblockPayload, target_index: u64) -> Self {
        Self { inner, target_index }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct XLayerFlashblockEnd {
    pub payload_id: PayloadId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum XLayerFlashblockMessage {
    /// Flashblock payload wrap with the target index.
    Payload(Box<XLayerFlashblockPayload>),
    /// End-of-sequence signal — no more flashblocks for the current block.
    PayloadEnd(XLayerFlashblockEnd),
}

impl XLayerFlashblockMessage {
    pub fn from_flashblock_payload(payload: XLayerFlashblockPayload) -> Self {
        Self::Payload(Box::new(payload))
    }

    pub fn from_flashblock_end(payload_id: PayloadId) -> Self {
        Self::PayloadEnd(XLayerFlashblockEnd { payload_id })
    }

    pub fn as_payload(&self) -> Option<&XLayerFlashblockPayload> {
        if let Self::Payload(payload) = self {
            Some(payload)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xlayer_payload_serializes_flat() {
        let payload = OpFlashblockPayload::default();
        let wrapped = XLayerFlashblockPayload::new(payload.clone(), 7);
        let json = serde_json::to_string(&wrapped).unwrap();
        // target_index should appear at top level, not nested
        assert!(json.contains("\"target_index\":7"));
        // inner fields should also be at top level
        assert!(json.contains("\"index\":"));
    }

    #[test]
    fn test_backwards_compat_old_consumer_ignores_target_index() {
        let payload = OpFlashblockPayload::default();
        let wrapped = XLayerFlashblockPayload::new(payload, 7);
        let json = serde_json::to_string(&wrapped).unwrap();
        // Old consumer deserializes as OpFlashblockPayload — should succeed
        let _: OpFlashblockPayload = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn test_backwards_compat_new_consumer_defaults_target_index() {
        let payload = OpFlashblockPayload::default();
        let json = serde_json::to_string(&payload).unwrap();
        // New consumer deserializes as XLayerFlashblockPayload — target_index defaults to 0
        let wrapped: XLayerFlashblockPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(wrapped.target_index, 0);
    }

    #[test]
    fn test_message_payload_serializes_same_as_struct() {
        let payload = XLayerFlashblockPayload::new(OpFlashblockPayload::default(), 7);
        let direct_json = serde_json::to_string(&payload).unwrap();
        let message = XLayerFlashblockMessage::from_flashblock_payload(payload);
        let message_json = serde_json::to_string(&message).unwrap();
        assert_eq!(direct_json, message_json, "Payload variant must serialize identically");
    }

    #[test]
    fn test_message_end_of_sequence_round_trip() {
        let signal = XLayerFlashblockEnd { payload_id: PayloadId::new([1; 8]) };
        let message = XLayerFlashblockMessage::PayloadEnd(signal.clone());
        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("\"payload_id\""));

        let deserialized: XLayerFlashblockMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, message);
    }

    #[test]
    fn test_message_payload_round_trip() {
        let payload = XLayerFlashblockPayload::new(OpFlashblockPayload::default(), 7);
        let message = XLayerFlashblockMessage::from_flashblock_payload(payload);
        let json = serde_json::to_string(&message).unwrap();
        let deserialized: XLayerFlashblockMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, message);
    }

    #[test]
    fn test_end_of_sequence_not_deserializable_as_payload() {
        let signal = XLayerFlashblockEnd { payload_id: PayloadId::new([1; 8]) };
        let json = serde_json::to_string(&signal).unwrap();
        // Deserializing an EndOfSequence JSON as XLayerFlashblockMessage should yield
        // the EndOfSequence variant, not Payload
        let message: XLayerFlashblockMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(message, XLayerFlashblockMessage::PayloadEnd(_)));
    }
}
