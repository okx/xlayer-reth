use alloy_primitives::{keccak256, B256};
use op_alloy_rpc_types_engine::OpFlashblockPayload;
use parking_lot::Mutex;
use reth_metrics::{
    metrics::{Counter, Gauge},
    Metrics,
};
use schnellru::{ByLength, LruMap};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DedupKey([u8; 32]);

impl DedupKey {
    pub(crate) fn from_flashblock(payload: &OpFlashblockPayload) -> Self {
        let mut input = Vec::with_capacity(48);
        input.extend_from_slice(payload.payload_id.0.as_slice());
        input.extend_from_slice(&payload.index.to_le_bytes());
        input.extend_from_slice(payload.diff.state_root.as_slice());
        Self(keccak256(&input).0)
    }

    pub(crate) fn from_full_block(block_hash: B256) -> Self {
        Self(block_hash.0)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum DedupKind {
    Flashblock,
    FullBlock,
}

#[derive(Metrics, Clone)]
#[metrics(scope = "p2p_dedup")]
pub struct P2pDedupMetrics {
    /// Cache hits for flashblock payloads
    pub hits_flashblock: Counter,
    /// Cache hits for full block payloads
    pub hits_full_block: Counter,
    /// Cache misses for flashblock payloads
    pub misses_flashblock: Counter,
    /// Cache misses for full block payloads
    pub misses_full_block: Counter,
    /// Current number of entries in the dedup cache
    pub cache_size: Gauge,
}

#[derive(Clone)]
pub(crate) struct P2pDedupCache {
    inner: Arc<Mutex<LruMap<DedupKey, (), ByLength>>>,
    metrics: P2pDedupMetrics,
}

impl P2pDedupCache {
    pub(crate) fn new(capacity: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LruMap::new(ByLength::new(capacity)))),
            metrics: P2pDedupMetrics::default(),
        }
    }

    /// Returns `true` if the key was already present (duplicate).
    pub(crate) fn check_and_insert(&self, key: DedupKey, kind: DedupKind) -> bool {
        let mut guard = self.inner.lock();
        if guard.get(&key).is_some() {
            drop(guard);
            match kind {
                DedupKind::Flashblock => self.metrics.hits_flashblock.increment(1),
                DedupKind::FullBlock => self.metrics.hits_full_block.increment(1),
            }
            true
        } else {
            guard.insert(key, ());
            let size = guard.len() as f64;
            drop(guard);
            self.metrics.cache_size.set(size);
            match kind {
                DedupKind::Flashblock => self.metrics.misses_flashblock.increment(1),
                DedupKind::FullBlock => self.metrics.misses_full_block.increment(1),
            }
            false
        }
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
            diff: OpFlashblockPayloadDelta { state_root, ..Default::default() },
            metadata: OpFlashblockPayloadMetadata {
                block_number: 1,
                new_account_balances: Some(BTreeMap::new()),
                receipts: Some(BTreeMap::new()),
                access_list: None,
            },
        }
    }

    #[test]
    fn key_deterministic_for_same_payload() {
        let payload = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xAA));
        let key_a = DedupKey::from_flashblock(&payload);
        let key_b = DedupKey::from_flashblock(&payload);
        assert_eq!(key_a, key_b);
    }

    #[test]
    fn key_differs_for_different_state_root() {
        let payload_a = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xAA));
        let payload_b = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xBB));
        assert_ne!(DedupKey::from_flashblock(&payload_a), DedupKey::from_flashblock(&payload_b));
    }

    #[test]
    fn key_differs_for_different_index() {
        let payload_a = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xAA));
        let payload_b = make_flashblock_payload([1; 8], 1, B256::repeat_byte(0xAA));
        assert_ne!(DedupKey::from_flashblock(&payload_a), DedupKey::from_flashblock(&payload_b));
    }

    #[test]
    fn key_differs_for_different_payload_id() {
        let payload_a = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xAA));
        let payload_b = make_flashblock_payload([2; 8], 0, B256::repeat_byte(0xAA));
        assert_ne!(DedupKey::from_flashblock(&payload_a), DedupKey::from_flashblock(&payload_b));
    }

    #[test]
    fn full_block_key_is_block_hash() {
        let hash = B256::repeat_byte(0xFF);
        let key = DedupKey::from_full_block(hash);
        assert_eq!(key.0, hash.0);
    }

    #[test]
    fn full_block_different_hashes_different_keys() {
        let key_a = DedupKey::from_full_block(B256::repeat_byte(0xAA));
        let key_b = DedupKey::from_full_block(B256::repeat_byte(0xBB));
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn cache_miss_then_hit() {
        let cache = P2pDedupCache::new(4096);
        let payload = make_flashblock_payload([1; 8], 0, B256::repeat_byte(0xAA));
        let key = DedupKey::from_flashblock(&payload);

        assert!(!cache.check_and_insert(key, DedupKind::Flashblock));
        assert!(cache.check_and_insert(key, DedupKind::Flashblock));
    }

    #[test]
    fn cache_full_block_miss_then_hit() {
        let cache = P2pDedupCache::new(4096);
        let key = DedupKey::from_full_block(B256::repeat_byte(0xCC));

        assert!(!cache.check_and_insert(key, DedupKind::FullBlock));
        assert!(cache.check_and_insert(key, DedupKind::FullBlock));
    }

    #[test]
    fn lru_eviction_at_capacity() {
        let cache = P2pDedupCache::new(2);
        let key_a = DedupKey::from_full_block(B256::repeat_byte(0x01));
        let key_b = DedupKey::from_full_block(B256::repeat_byte(0x02));
        let key_c = DedupKey::from_full_block(B256::repeat_byte(0x03));

        // Fill cache: [key_a, key_b]
        assert!(!cache.check_and_insert(key_a, DedupKind::Flashblock));
        assert!(!cache.check_and_insert(key_b, DedupKind::Flashblock));

        // Both present
        assert!(cache.check_and_insert(key_a, DedupKind::Flashblock));
        assert!(cache.check_and_insert(key_b, DedupKind::Flashblock));

        // Insert key_c → evicts LRU (key_a, since key_b was just accessed above)
        assert!(!cache.check_and_insert(key_c, DedupKind::Flashblock));

        // key_a was evicted → miss
        assert!(!cache.check_and_insert(key_a, DedupKind::Flashblock));
    }

    #[test]
    fn evicted_key_re_arrives_as_new() {
        let cache = P2pDedupCache::new(2);
        let key_a = DedupKey::from_full_block(B256::repeat_byte(0x01));
        let key_b = DedupKey::from_full_block(B256::repeat_byte(0x02));
        let key_c = DedupKey::from_full_block(B256::repeat_byte(0x03));

        cache.check_and_insert(key_a, DedupKind::Flashblock);
        cache.check_and_insert(key_b, DedupKind::Flashblock);
        cache.check_and_insert(key_c, DedupKind::Flashblock); // evicts key_a

        // key_a re-arrives → treated as new (miss)
        assert!(!cache.check_and_insert(key_a, DedupKind::Flashblock));
    }

    #[test]
    fn concurrent_access() {
        let cache = P2pDedupCache::new(4096);
        let handles: Vec<_> = (0u8..4)
            .map(|thread_id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    for i in 0u8..50 {
                        let byte = thread_id.wrapping_mul(50).wrapping_add(i);
                        let hash = B256::repeat_byte(byte);
                        let key = DedupKey::from_full_block(hash);
                        cache.check_and_insert(key, DedupKind::Flashblock);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Verify known key is present
        let known_key = DedupKey::from_full_block(B256::repeat_byte(0));
        assert!(cache.check_and_insert(known_key, DedupKind::Flashblock));
    }

    #[test]
    fn clone_shares_state() {
        let cache = P2pDedupCache::new(4096);
        let clone = cache.clone();
        let key = DedupKey::from_full_block(B256::repeat_byte(0x42));

        cache.check_and_insert(key, DedupKind::Flashblock);
        // Clone sees the same entry
        assert!(clone.check_and_insert(key, DedupKind::Flashblock));
    }
}
