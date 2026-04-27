use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_consensus::transaction::Recovered;
use alloy_eips::eip2718::WithEncoded;
use alloy_primitives::B256;
use op_alloy_rpc_types_engine::OpFlashblockPayload;

use reth_node_core::dirs::{ChainPath, DataDirPath};
use reth_payload_builder::PayloadId;
use reth_primitives_traits::SignedTransaction;

/// Flashblocks sub-dir within the datadir.
const FLASHBLOCKS_DIR: &str = "flashblocks";

/// Flashblocks persistence filename for the current pending flashblocks sequence.
const PENDING_SEQUENCE_FILE: &str = "pending_sequence.json";

fn init_pending_sequence_path(datadir: ChainPath<DataDirPath>) -> Option<PathBuf> {
    let flashblocks_dir = datadir.data_dir().join(FLASHBLOCKS_DIR);
    std::fs::create_dir_all(&flashblocks_dir)
        .inspect_err(|e| {
            // log target is flashblocks since datadir init can be for both sequencer and RPC
            tracing::warn!(
                target: "flashblocks",
                "Failed to create flashblocks directory at {}: {e}",
                flashblocks_dir.display()
            );
        })
        .ok()?;
    Some(flashblocks_dir.join(PENDING_SEQUENCE_FILE))
}

fn try_load_from_filepath(path: Option<&Path>) -> Option<FlashblockPayloadsSequence> {
    let path = path?;
    if !path.exists() {
        tracing::warn!(target: "payload_builder", "Failed to read flashblocks persistence file: does not exist");
        return None;
    }

    let data = std::fs::read(path)
        .inspect_err(|e| {
            tracing::warn!(target: "payload_builder", "Failed to read flashblocks persistence file: {e}");
        })
        .ok()?;

    let sequence = serde_json::from_slice::<FlashblockPayloadsSequence>(&data)
        .inspect_err(|e| {
            tracing::warn!(target: "payload_builder", "Failed to deserialize flashblocks persistence file: {e}");
        })
        .ok()?;

    tracing::info!(
        target: "payload_builder",
        payload_id = %sequence.payload_id,
        parent_hash = ?sequence.parent_hash,
        payloads = sequence.payloads.len(),
        "Loaded pending flashblocks sequence from disk"
    );

    Some(sequence)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashblockPayloadsSequence {
    pub payload_id: PayloadId,
    pub parent_hash: Option<B256>,
    pub payloads: Vec<OpFlashblockPayload>,
}

/// Cache for the current pending block's flashblock payloads sequence that is
/// being built, based on the `payload_id`.
#[derive(Debug, Clone, Default)]
pub struct FlashblockPayloadsCache {
    inner: Arc<Mutex<Option<FlashblockPayloadsSequence>>>,
    persist_path: Option<PathBuf>,
}

impl FlashblockPayloadsCache {
    pub fn new(datadir: Option<ChainPath<DataDirPath>>) -> Self {
        let persist_path = datadir.and_then(init_pending_sequence_path);

        Self {
            inner: Arc::new(Mutex::new(try_load_from_filepath(persist_path.as_deref()))),
            persist_path,
        }
    }

    pub fn add_flashblock_payload(&self, payload: OpFlashblockPayload) {
        let mut guard = self.inner.lock();
        match guard.as_mut() {
            Some(sequence) if sequence.payload_id == payload.payload_id => {
                if sequence.parent_hash.is_none()
                    && let Some(hash) = payload.parent_hash()
                {
                    sequence.parent_hash = Some(hash);
                }
                sequence.payloads.push(payload);
            }
            _ => {
                // New payload_id - replace entire cache
                *guard = Some(FlashblockPayloadsSequence {
                    payload_id: payload.payload_id,
                    parent_hash: payload.parent_hash(),
                    payloads: vec![payload],
                });
            }
        }
    }

    pub async fn persist(&self) -> eyre::Result<()> {
        let Some(path) = self.persist_path.as_ref() else { return Ok(()) };
        let Some(sequence) = self.inner.lock().clone() else { return Ok(()) };

        let data = serde_json::to_vec(&sequence)?;

        let file_name = path
            .file_name()
            .ok_or_else(|| eyre::eyre!("persist path has no file name"))?
            .to_string_lossy();
        let tmp_path = path.with_file_name(format!(".{file_name}"));

        tokio::fs::write(&tmp_path, &data).await?;
        tokio::fs::rename(&tmp_path, path).await?;

        Ok(())
    }

    /// Get the flashblocks sequence transactions for a given `parent_hash`. Note that we do not
    /// yield sequencer transactions that were included in the payload attributes (index 0).
    ///
    /// Returns `None` if:
    /// - `parent_hash` is not the current pending block's parent hash
    /// - The payloads are not in sequential order or have missing indexes
    pub(crate) fn get_flashblocks_sequence_txs<T: SignedTransaction>(
        &self,
        parent_hash: B256,
    ) -> Option<Vec<WithEncoded<Recovered<T>>>> {
        let mut payloads = {
            let guard = self.inner.lock();
            let sequence = guard.as_ref()?;
            if sequence.parent_hash != Some(parent_hash) {
                return None;
            }
            sequence.payloads.clone()
        };

        payloads.sort_by_key(|p| p.index);

        // Skip base payload index 0 (sequencer transactions)
        payloads.iter().skip(1).enumerate().try_fold(
            Vec::with_capacity(payloads.len()),
            |mut acc, (expected_index, payload)| {
                if payload.index != expected_index as u64 + 1 {
                    tracing::warn!(
                        target: "payload_builder",
                        expected = expected_index + 1,
                        got = payload.index,
                        "flashblock payloads have missing or out-of-order indexes",
                    );
                    return None;
                }
                acc.extend(payload.recover_transactions().collect::<Result<Vec<_>, _>>().ok()?);
                Some(acc)
            },
        )
    }

    #[cfg(test)]
    fn with_persist_path(path: PathBuf) -> Self {
        Self { inner: Arc::new(Mutex::new(None)), persist_path: Some(path) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use op_alloy_rpc_types_engine::{
        OpFlashblockPayloadBase, OpFlashblockPayloadDelta, OpFlashblockPayloadMetadata,
    };
    use reth_optimism_primitives::OpTransactionSigned;
    use std::collections::BTreeMap;

    /// RAII guard for a temporary directory that cleans up on drop (success or failure).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("xlayer_cache_test_{name}_{}", std::process::id()));
            std::fs::create_dir_all(&path).expect("failed to create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Creates a test flashblock payload with configurable fields.
    fn make_payload(
        payload_id: [u8; 8],
        index: u64,
        parent_hash: Option<B256>,
        block_number: u64,
    ) -> OpFlashblockPayload {
        OpFlashblockPayload {
            payload_id: PayloadId::new(payload_id),
            index,
            base: parent_hash.map(|hash| OpFlashblockPayloadBase {
                parent_hash: hash,
                block_number,
                ..Default::default()
            }),
            diff: OpFlashblockPayloadDelta::default(),
            metadata: OpFlashblockPayloadMetadata {
                block_number,
                new_account_balances: Some(BTreeMap::new()),
                receipts: Some(BTreeMap::new()),
                access_list: None,
            },
        }
    }

    // ========================================================================
    // Cache creation
    // ========================================================================

    #[test]
    fn default_cache_is_empty() {
        let cache = FlashblockPayloadsCache::default();
        assert!(cache.inner.lock().is_none());
        assert!(cache.persist_path.is_none());
    }

    #[test]
    fn new_without_datadir_has_no_persist_path() {
        let cache = FlashblockPayloadsCache::new(None);
        assert!(cache.persist_path.is_none());
    }

    // ========================================================================
    // add_flashblock_payload
    // ========================================================================

    #[test]
    fn add_single_payload_creates_sequence() {
        let cache = FlashblockPayloadsCache::default();
        let parent = B256::random();
        let payload = make_payload([1u8; 8], 0, Some(parent), 100);

        cache.add_flashblock_payload(payload);

        let guard = cache.inner.lock();
        let seq = guard.as_ref().unwrap();
        assert_eq!(seq.payload_id, PayloadId::new([1u8; 8]));
        assert_eq!(seq.parent_hash, Some(parent));
        assert_eq!(seq.payloads.len(), 1);
    }

    #[test]
    fn add_payloads_same_id_appends() {
        let cache = FlashblockPayloadsCache::default();
        let parent = B256::random();
        let id = [1u8; 8];

        // First payload (base, index 0)
        cache.add_flashblock_payload(make_payload(id, 0, Some(parent), 100));
        // Second payload (incremental, index 1)
        cache.add_flashblock_payload(make_payload(id, 1, None, 100));
        // Third payload (incremental, index 2)
        cache.add_flashblock_payload(make_payload(id, 2, None, 100));

        let guard = cache.inner.lock();
        let seq = guard.as_ref().unwrap();
        assert_eq!(seq.payload_id, PayloadId::new(id));
        assert_eq!(seq.payloads.len(), 3);
        assert_eq!(seq.payloads[0].index, 0);
        assert_eq!(seq.payloads[1].index, 1);
        assert_eq!(seq.payloads[2].index, 2);
    }

    #[test]
    fn add_payload_new_id_replaces_sequence() {
        let cache = FlashblockPayloadsCache::default();
        let parent_a = B256::random();
        let parent_b = B256::random();

        // Add payloads for first block
        cache.add_flashblock_payload(make_payload([1u8; 8], 0, Some(parent_a), 100));
        cache.add_flashblock_payload(make_payload([1u8; 8], 1, None, 100));

        // New payload_id replaces the entire cache
        cache.add_flashblock_payload(make_payload([2u8; 8], 0, Some(parent_b), 101));

        let guard = cache.inner.lock();
        let seq = guard.as_ref().unwrap();
        assert_eq!(seq.payload_id, PayloadId::new([2u8; 8]));
        assert_eq!(seq.parent_hash, Some(parent_b));
        assert_eq!(seq.payloads.len(), 1);
    }

    #[test]
    fn parent_hash_extracted_from_first_payload_with_base() {
        let cache = FlashblockPayloadsCache::default();
        let parent = B256::random();
        let id = [1u8; 8];

        // First payload without base (no parent_hash)
        cache.add_flashblock_payload(make_payload(id, 1, None, 100));

        {
            let guard = cache.inner.lock();
            assert_eq!(guard.as_ref().unwrap().parent_hash, None);
        }

        // Second payload with base containing parent_hash - should backfill
        cache.add_flashblock_payload(make_payload(id, 0, Some(parent), 100));

        let guard = cache.inner.lock();
        assert_eq!(guard.as_ref().unwrap().parent_hash, Some(parent));
    }

    #[test]
    fn parent_hash_not_overwritten_once_set() {
        let cache = FlashblockPayloadsCache::default();
        let parent_first = B256::random();
        let parent_second = B256::random();
        let id = [1u8; 8];

        // First payload sets parent_hash
        cache.add_flashblock_payload(make_payload(id, 0, Some(parent_first), 100));

        // Second payload with different parent_hash in base - should NOT overwrite
        cache.add_flashblock_payload(make_payload(id, 1, Some(parent_second), 100));

        let guard = cache.inner.lock();
        assert_eq!(guard.as_ref().unwrap().parent_hash, Some(parent_first));
    }

    // ========================================================================
    // FlashblockPayloadsSequence serialization
    // ========================================================================

    #[test]
    fn sequence_serde_roundtrip() {
        let parent = B256::random();
        let sequence = FlashblockPayloadsSequence {
            payload_id: PayloadId::new([42u8; 8]),
            parent_hash: Some(parent),
            payloads: vec![
                make_payload([42u8; 8], 0, Some(parent), 100),
                make_payload([42u8; 8], 1, None, 100),
            ],
        };

        let json = serde_json::to_vec(&sequence).unwrap();
        let deserialized: FlashblockPayloadsSequence = serde_json::from_slice(&json).unwrap();

        assert_eq!(deserialized.payload_id, sequence.payload_id);
        assert_eq!(deserialized.parent_hash, sequence.parent_hash);
        assert_eq!(deserialized.payloads.len(), sequence.payloads.len());
        assert_eq!(deserialized.payloads[0].index, 0);
        assert_eq!(deserialized.payloads[1].index, 1);
    }

    #[test]
    fn sequence_serde_with_none_parent_hash() {
        let sequence = FlashblockPayloadsSequence {
            payload_id: PayloadId::new([1u8; 8]),
            parent_hash: None,
            payloads: vec![make_payload([1u8; 8], 0, None, 50)],
        };

        let json = serde_json::to_vec(&sequence).unwrap();
        let deserialized: FlashblockPayloadsSequence = serde_json::from_slice(&json).unwrap();

        assert_eq!(deserialized.parent_hash, None);
    }

    #[tokio::test]
    async fn persist_writes_file_and_load_restores() {
        let tmp = TempDir::new("persist_roundtrip");
        let file_path = tmp.path().join(PENDING_SEQUENCE_FILE);

        let cache = FlashblockPayloadsCache::with_persist_path(file_path.clone());
        let parent = B256::random();
        let id = [10u8; 8];

        cache.add_flashblock_payload(make_payload(id, 0, Some(parent), 200));
        cache.add_flashblock_payload(make_payload(id, 1, None, 200));
        cache.add_flashblock_payload(make_payload(id, 2, None, 200));

        // Persist to disk
        cache.persist().await.unwrap();

        // Verify file exists
        assert!(file_path.exists(), "persistence file should exist after persist()");

        // Verify no temp file left behind
        let tmp_path = file_path.with_file_name(format!(".{PENDING_SEQUENCE_FILE}"));
        assert!(!tmp_path.exists(), "temp file should be cleaned up after atomic rename");

        // Load from file and verify contents match
        let loaded_seq =
            try_load_from_filepath(Some(&file_path)).expect("should load persisted sequence");

        assert_eq!(loaded_seq.payload_id, PayloadId::new(id));
        assert_eq!(loaded_seq.parent_hash, Some(parent));
        assert_eq!(loaded_seq.payloads.len(), 3);
        assert_eq!(loaded_seq.payloads[0].index, 0);
        assert_eq!(loaded_seq.payloads[1].index, 1);
        assert_eq!(loaded_seq.payloads[2].index, 2);
    }

    #[tokio::test]
    async fn persist_no_path_is_noop() {
        let cache = FlashblockPayloadsCache::default();
        cache.add_flashblock_payload(make_payload([1u8; 8], 0, Some(B256::ZERO), 1));

        // Should succeed without writing anything (no persist_path)
        cache.persist().await.unwrap();
    }

    #[tokio::test]
    async fn persist_empty_cache_is_noop() {
        let tmp = TempDir::new("persist_empty");
        let file_path = tmp.path().join(PENDING_SEQUENCE_FILE);

        let cache = FlashblockPayloadsCache::with_persist_path(file_path.clone());

        // Cache is empty — persist should be a no-op
        cache.persist().await.unwrap();
        assert!(!file_path.exists(), "no file should be written for empty cache");
    }

    #[tokio::test]
    async fn persist_overwrites_previous_file() {
        let tmp = TempDir::new("persist_overwrite");
        let file_path = tmp.path().join(PENDING_SEQUENCE_FILE);

        let cache = FlashblockPayloadsCache::with_persist_path(file_path.clone());

        // First sequence
        let parent_a = B256::random();
        cache.add_flashblock_payload(make_payload([1u8; 8], 0, Some(parent_a), 100));
        cache.persist().await.unwrap();

        // Replace with second sequence
        let parent_b = B256::random();
        cache.add_flashblock_payload(make_payload([2u8; 8], 0, Some(parent_b), 101));
        cache.persist().await.unwrap();

        // Loaded data should reflect the second sequence
        let loaded_seq =
            try_load_from_filepath(Some(&file_path)).expect("should load persisted sequence");
        assert_eq!(loaded_seq.payload_id, PayloadId::new([2u8; 8]));
        assert_eq!(loaded_seq.parent_hash, Some(parent_b));
    }

    #[test]
    fn load_from_nonexistent_file_returns_none() {
        let result = try_load_from_filepath(Some(Path::new("/nonexistent/path.json")));
        assert!(result.is_none());
    }

    #[test]
    fn load_from_invalid_json_returns_none() {
        let tmp = TempDir::new("load_invalid");
        let file_path = tmp.path().join("bad.json");
        std::fs::write(&file_path, b"not valid json").unwrap();

        let result = try_load_from_filepath(Some(&file_path));
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn persist_file_contains_valid_json() {
        let tmp = TempDir::new("persist_json");
        let file_path = tmp.path().join(PENDING_SEQUENCE_FILE);

        let cache = FlashblockPayloadsCache::with_persist_path(file_path.clone());
        let parent = B256::random();
        cache.add_flashblock_payload(make_payload([5u8; 8], 0, Some(parent), 42));
        cache.persist().await.unwrap();

        // Read raw bytes and verify it's valid JSON that deserializes correctly
        let data = std::fs::read(&file_path).unwrap();
        let seq: FlashblockPayloadsSequence = serde_json::from_slice(&data).unwrap();
        assert_eq!(seq.payload_id, PayloadId::new([5u8; 8]));
        assert_eq!(seq.parent_hash, Some(parent));
        assert_eq!(seq.payloads[0].metadata.block_number, 42);
    }

    #[test]
    fn get_txs_empty_cache_returns_none() {
        let cache = FlashblockPayloadsCache::default();
        let result = cache.get_flashblocks_sequence_txs::<OpTransactionSigned>(B256::random());
        assert!(result.is_none());
    }

    #[test]
    fn get_txs_wrong_parent_hash_returns_none() {
        let cache = FlashblockPayloadsCache::default();
        let parent = B256::random();
        let wrong_parent = B256::random();

        cache.add_flashblock_payload(make_payload([1u8; 8], 0, Some(parent), 100));

        let result = cache.get_flashblocks_sequence_txs::<OpTransactionSigned>(wrong_parent);
        assert!(result.is_none());
    }

    #[test]
    fn get_txs_only_base_payload_returns_empty_vec() {
        let cache = FlashblockPayloadsCache::default();
        let parent = B256::random();

        // Only index 0 (base) — no flashblock transactions to return
        cache.add_flashblock_payload(make_payload([1u8; 8], 0, Some(parent), 100));

        let result = cache.get_flashblocks_sequence_txs::<OpTransactionSigned>(parent);
        assert_eq!(result, Some(vec![]));
    }

    #[test]
    fn get_txs_with_none_parent_hash_returns_none() {
        let cache = FlashblockPayloadsCache::default();

        // Payload without base (parent_hash will be None)
        cache.add_flashblock_payload(make_payload([1u8; 8], 1, None, 100));

        // Any query should return None since cached parent_hash is None
        let result = cache.get_flashblocks_sequence_txs::<OpTransactionSigned>(B256::ZERO);
        assert!(result.is_none());
    }

    #[test]
    fn get_txs_non_sequential_indexes_returns_none() {
        let cache = FlashblockPayloadsCache::default();
        let parent = B256::random();
        let id = [1u8; 8];

        // index 0 (base)
        cache.add_flashblock_payload(make_payload(id, 0, Some(parent), 100));
        // index 1 — sequential
        cache.add_flashblock_payload(make_payload(id, 1, None, 100));
        // index 3 — gap (skipped index 2)
        cache.add_flashblock_payload(make_payload(id, 3, None, 100));

        let result = cache.get_flashblocks_sequence_txs::<OpTransactionSigned>(parent);
        assert!(result.is_none(), "gap in indexes should return None");
    }

    #[test]
    fn concurrent_add_and_read() {
        let cache = FlashblockPayloadsCache::default();
        let cache_clone = cache.clone();
        let parent = B256::random();
        let id = [1u8; 8];

        // Spawn writer thread
        let writer = std::thread::spawn(move || {
            for i in 0..100u64 {
                cache_clone.add_flashblock_payload(make_payload(id, i, Some(parent), 100));
            }
        });

        // Read concurrently from main thread
        for _ in 0..50 {
            let guard = cache.inner.lock();
            if let Some(seq) = guard.as_ref() {
                assert_eq!(seq.payload_id, PayloadId::new(id));
                assert!(!seq.payloads.is_empty());
            }
            drop(guard);
        }

        writer.join().unwrap();

        let guard = cache.inner.lock();
        let seq = guard.as_ref().unwrap();
        assert_eq!(seq.payloads.len(), 100);
    }

    #[test]
    fn clone_shares_underlying_data() {
        let cache = FlashblockPayloadsCache::default();
        let clone = cache.clone();

        cache.add_flashblock_payload(make_payload([1u8; 8], 0, Some(B256::ZERO), 1));

        // Clone should see the same data
        let guard = clone.inner.lock();
        assert!(guard.is_some());
        assert_eq!(guard.as_ref().unwrap().payloads.len(), 1);
    }
}
