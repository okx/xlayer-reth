use parking_lot::Mutex;
use std::sync::Arc;

use alloy_consensus::transaction::Recovered;
use alloy_eips::eip2718::WithEncoded;
use alloy_primitives::B256;
use op_alloy_rpc_types_engine::OpFlashblockPayload;
use reth_payload_builder::PayloadId;
use reth_primitives_traits::SignedTransaction;

type FlashblockPayloadsSequence = Option<(PayloadId, Option<B256>, Vec<OpFlashblockPayload>)>;

/// Cache for the current pending block's flashblock payloads sequence that is
/// being built, based on the `payload_id`.
#[derive(Debug, Clone, Default)]
pub(crate) struct FlashblockPayloadsCache {
    inner: Arc<Mutex<FlashblockPayloadsSequence>>,
}

impl FlashblockPayloadsCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn add_flashblock_payload(&self, payload: OpFlashblockPayload) -> eyre::Result<()> {
        let mut guard = self.inner.lock();
        match guard.as_mut() {
            Some((curr_payload_id, parent_hash, payloads))
                if *curr_payload_id == payload.payload_id =>
            {
                if parent_hash.is_none()
                    && let Some(hash) = payload.parent_hash()
                {
                    *parent_hash = Some(hash);
                }
                payloads.push(payload);
            }
            _ => {
                // New payload_id - replace entire cache
                *guard = Some((payload.payload_id, payload.parent_hash(), vec![payload]));
            }
        }
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
            let mut guard = self.inner.lock();
            let (_, curr_parent_hash, _) = guard.as_ref()?;
            if *curr_parent_hash != Some(parent_hash) {
                return None;
            }
            // Take ownership and flush the cache
            let (_, _, payloads) = guard.take()?;
            payloads
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
