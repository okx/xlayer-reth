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

fn init_pending_sequence_path(datadir: &ChainPath<DataDirPath>) -> eyre::Result<PathBuf> {
    let flashblocks_dir = datadir.data_dir().join(FLASHBLOCKS_DIR);
    std::fs::create_dir_all(&flashblocks_dir).map_err(|e| {
        eyre::eyre!("Failed to create flashblocks directory at {}: {e}", flashblocks_dir.display())
    })?;
    Ok(flashblocks_dir.join(PENDING_SEQUENCE_FILE))
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
    pub fn new(datadir: Option<&ChainPath<DataDirPath>>) -> Self {
        let persist_path = datadir.and_then(|d| {
            init_pending_sequence_path(d)
                .inspect_err(|e| {
                    tracing::warn!(target: "flashblocks", "Failed to init persistence path: {e}");
                })
                .ok()
        });
        Self { inner: Arc::new(Mutex::new(None)), persist_path }
    }

    pub fn add_flashblock_payload(&self, payload: OpFlashblockPayload) -> eyre::Result<()> {
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
        Ok(())
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

    pub fn load_from_file(path: &Path) -> eyre::Result<Self> {
        let data = std::fs::read(path)?;
        let sequence: FlashblockPayloadsSequence = serde_json::from_slice(&data)?;

        let cache = Self::default();
        *cache.inner.lock() = Some(sequence);

        Ok(cache)
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
                        expected = expected_index + 1,
                        got = payload.index,
                        "flashblock payloads have missing or out-of-order indexes"
                    );
                    return None;
                }
                acc.extend(payload.recover_transactions().collect::<Result<Vec<_>, _>>().ok()?);
                Some(acc)
            },
        )
    }
}
