use op_alloy_rpc_types_engine::OpFlashblockPayload;
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
}
