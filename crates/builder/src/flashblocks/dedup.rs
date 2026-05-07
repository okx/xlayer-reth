use alloy_primitives::{keccak256, B256};
use op_alloy_rpc_types_engine::OpFlashblockPayload;
use parking_lot::Mutex;
use schnellru::{ByLength, LruMap};
use std::sync::Arc;

/// Stable identity key for deduplicating inbound P2P messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DedupKey {
    /// Flashblock: keccak256(payload_id || index_le_bytes || diff.state_root)
    Flashblock([u8; 32]),
    /// Full block: block.hash() (B256)
    FullBlock(B256),
}

impl DedupKey {
    pub(crate) fn from_flashblock(payload: &OpFlashblockPayload) -> Self {
        let mut input = Vec::with_capacity(48);
        input.extend_from_slice(payload.payload_id.as_ref());
        input.extend_from_slice(&payload.index.to_le_bytes());
        input.extend_from_slice(payload.diff.state_root.as_ref());
        DedupKey::Flashblock(keccak256(&input).into())
    }

    pub(crate) fn from_full_block(block_hash: B256) -> Self {
        DedupKey::FullBlock(block_hash)
    }
}

/// LRU-based deduplication cache for inbound P2P messages.
///
/// Thread-safe via parking_lot::Mutex (short-held, allocation-free on hit).
#[derive(Debug, Clone)]
pub(crate) struct P2pDedupCache {
    inner: Arc<Mutex<LruMap<DedupKey, (), ByLength>>>,
    enabled: bool,
}

impl P2pDedupCache {
    pub(crate) fn new(capacity: u32, enabled: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LruMap::new(ByLength::new(capacity)))),
            enabled,
        }
    }

    /// Returns `true` if the key was already present (duplicate — should drop).
    /// Returns `false` if the key is new (should process).
    /// When disabled, always returns `false`.
    pub(crate) fn check_or_insert(&self, key: DedupKey) -> bool {
        if !self.enabled {
            return false;
        }
        let mut guard = self.inner.lock();
        if guard.get(&key).is_some() {
            true
        } else {
            guard.insert(key, ());
            false
        }
    }

    pub(crate) fn len(&self) -> u32 {
        self.inner.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use op_alloy_rpc_types_engine::{
        OpFlashblockPayloadBase, OpFlashblockPayloadDelta, OpFlashblockPayloadMetadata,
    };
    use reth_payload_builder::PayloadId;
    use std::collections::BTreeMap;

    fn make_flashblock_payload(
        payload_id: [u8; 8],
        index: u64,
        state_root: B256,
    ) -> OpFlashblockPayload {
        OpFlashblockPayload {
            payload_id: PayloadId::new(payload_id),
            index,
            base: Some(OpFlashblockPayloadBase {
                parent_hash: B256::ZERO,
                block_number: 1,
                ..Default::default()
            }),
            diff: OpFlashblockPayloadDelta {
                state_root,
                ..Default::default()
            },
            metadata: OpFlashblockPayloadMetadata {
                block_number: 1,
                new_account_balances: Some(BTreeMap::new()),
                receipts: Some(BTreeMap::new()),
                access_list: None,
            },
        }
    }

    #[test]
    fn check_or_insert_miss_then_hit() {
        let cache = P2pDedupCache::new(4096, true);
        let key = DedupKey::FullBlock(B256::repeat_byte(0x01));

        assert!(!cache.check_or_insert(key), "first insert should be miss");
        assert!(cache.check_or_insert(key), "second insert should be hit");
    }

    #[test]
    fn check_or_insert_distinct_keys() {
        let cache = P2pDedupCache::new(4096, true);
        let key_a = DedupKey::FullBlock(B256::repeat_byte(0x01));
        let key_b = DedupKey::FullBlock(B256::repeat_byte(0x02));

        assert!(!cache.check_or_insert(key_a));
        assert!(!cache.check_or_insert(key_b));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn lru_eviction_at_capacity() {
        let cache = P2pDedupCache::new(4, true);

        let keys: Vec<DedupKey> =
            (0..4).map(|i| DedupKey::FullBlock(B256::repeat_byte(i))).collect();

        for key in &keys {
            assert!(!cache.check_or_insert(*key));
        }
        assert_eq!(cache.len(), 4);

        // Insert a 5th key — should evict key[0] (oldest)
        let key_new = DedupKey::FullBlock(B256::repeat_byte(0xFF));
        assert!(!cache.check_or_insert(key_new));
        assert_eq!(cache.len(), 4);

        // key[0] was evicted — should be a miss now
        assert!(!cache.check_or_insert(keys[0]));
        // key[1] should still be present (not evicted yet since it was accessed after key[0])
        assert!(cache.check_or_insert(keys[1]));
    }

    #[test]
    fn disabled_cache_always_returns_false() {
        let cache = P2pDedupCache::new(4096, false);
        let key = DedupKey::FullBlock(B256::repeat_byte(0x01));

        assert!(!cache.check_or_insert(key));
        assert!(!cache.check_or_insert(key)); // still false when disabled
        assert_eq!(cache.len(), 0); // nothing inserted
    }

    #[test]
    fn flashblock_key_deterministic() {
        let payload = make_flashblock_payload([1; 8], 5, B256::repeat_byte(0xAA));

        let key1 = DedupKey::from_flashblock(&payload);
        let key2 = DedupKey::from_flashblock(&payload);
        assert_eq!(key1, key2);
    }

    #[test]
    fn flashblock_key_differs_by_index() {
        let payload_a = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xAA));
        let payload_b = make_flashblock_payload([1; 8], 1, B256::repeat_byte(0xAA));

        let key_a = DedupKey::from_flashblock(&payload_a);
        let key_b = DedupKey::from_flashblock(&payload_b);
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn flashblock_key_differs_by_state_root() {
        let payload_a = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xAA));
        let payload_b = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xBB));

        let key_a = DedupKey::from_flashblock(&payload_a);
        let key_b = DedupKey::from_flashblock(&payload_b);
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn flashblock_key_differs_by_payload_id() {
        let payload_a = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xAA));
        let payload_b = make_flashblock_payload([2; 8], 0, B256::repeat_byte(0xAA));

        let key_a = DedupKey::from_flashblock(&payload_a);
        let key_b = DedupKey::from_flashblock(&payload_b);
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn full_block_key_from_hash() {
        let hash = B256::repeat_byte(0xDE);
        let key = DedupKey::from_full_block(hash);
        assert_eq!(key, DedupKey::FullBlock(hash));
    }

    #[test]
    fn concurrent_access() {
        let cache = P2pDedupCache::new(4096, true);
        let cache_clone = cache.clone();

        let writer = std::thread::spawn(move || {
            for i in 0..100u8 {
                cache_clone.check_or_insert(DedupKey::FullBlock(B256::repeat_byte(i)));
            }
        });

        // Read concurrently
        for _ in 0..50 {
            let _ = cache.len();
        }

        writer.join().unwrap();
        assert_eq!(cache.len(), 100);
    }
}
