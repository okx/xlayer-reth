use crate::{
    cache::CachedTxInfo,
    execution::{BuildArgs, PrefixExecutionMeta},
};
use derive_more::Deref;
use std::collections::HashMap;

use alloy_consensus::BlockHeader;
use alloy_eips::eip2718::{Encodable2718, WithEncoded};
use alloy_primitives::{TxHash, B256, U256};
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;
use reth_primitives_traits::{
    transaction::{signed::SignedTransaction, TxHashRef},
    Block as BlockTrait, BlockBody, NodePrimitives, Recovered, SealedBlock, SealedHeader,
};
use reth_rpc_eth_types::{block::BlockAndReceipts, PendingBlock};

/// The pending flashblocks sequence built with all received `OpFlashblockPayload`
/// alongside the metadata for the last added flashblock.
#[derive(Debug, Clone, Deref)]
pub struct PendingSequence<N: NodePrimitives> {
    /// Locally built full pending block of the latest flashblocks sequence.
    #[deref]
    pub pending: PendingBlock<N>,
    /// Transaction index: tx hash → cached tx info for O(1) tx/receipt lookups.
    pub tx_index: HashMap<TxHash, CachedTxInfo<N>>,
    /// The current block hash of the latest flashblocks sequence.
    pub block_hash: B256,
    /// Parent hash of the built block (may be non-canonical or canonical).
    pub parent_header: SealedHeader<N::BlockHeader>,
    /// Prefix execution metadata for incremental builds.
    pub prefix_execution_meta: PrefixExecutionMeta,
    /// Whether the pending sequence is sequence end
    pub sequence_end: bool,
}

impl<N: NodePrimitives> PendingSequence<N> {
    pub fn get_hash(&self) -> B256 {
        self.block_hash
    }

    pub fn get_height(&self) -> u64 {
        self.pending.block().number()
    }

    pub fn get_block_and_receipts(&self) -> BlockAndReceipts<N> {
        self.pending.to_block_and_receipts()
    }

    pub fn get_tx_info(&self, tx_hash: &TxHash) -> Option<(CachedTxInfo<N>, BlockAndReceipts<N>)> {
        self.tx_index
            .get(tx_hash)
            .cloned()
            .map(|tx_info| (tx_info, self.pending.to_block_and_receipts()))
    }

    pub fn get_last_flashblock_index(&self) -> u64 {
        self.prefix_execution_meta.last_flashblock_index
    }

    pub fn is_sequence_end(&self) -> bool {
        self.sequence_end
    }

    /// Validates the incoming default payload with the pending sequence and generates the
    /// build args with sequence end for flashblocks sequence validation on existing warm
    /// caches if the incoming payload matches.
    ///
    /// Used on the engine validator's cache-miss path: when the default CL/EL sync arrives
    /// before flashblock sequence end message, we try to validate the incoming default
    /// payload through the warm flashblocks sequence validator pipeline instead of dropping
    /// and re-validating from cold + re-executing already validated txs.
    ///
    /// Validation steps:
    /// 1. Every field of the [`OpFlashblockPayloadBase`] derived from the incoming
    ///    payload's header must equal the base this sequence was built against —
    ///    `parent_hash`, `fee_recipient`, `prev_randao`, `block_number`, `gas_limit`,
    ///    `timestamp`, `extra_data`, `base_fee_per_gas`, `parent_beacon_block_root`. Any
    ///    field divergence means a sequencer switch / chain reorg / different attribute
    ///    set, and the caller must fall back to default engine validation.
    /// 2. Every prefix tx hash must match this sequence's already-executed prefix txs.
    #[expect(clippy::type_complexity)]
    pub fn try_insert_default_payload(
        &self,
        incoming: SealedBlock<N::Block>,
    ) -> eyre::Result<BuildArgs<Vec<WithEncoded<Recovered<N::SignedTx>>>>>
    where
        N::SignedTx: SignedTransaction + Encodable2718,
        <<N::Block as BlockTrait>::Body as BlockBody>::Transaction: TxHashRef,
    {
        // 1. Validate incoming block header against the FB-built pending header.
        let incoming_hash = incoming.hash();
        let incoming_header = incoming.header().clone();
        self.validate_incoming_header(&incoming_header)?;

        // 2. Prefix tx hash check
        self.validate_prefix_transactions(incoming.body().transactions())?;

        // Get build args
        let withdrawals =
            incoming.body().withdrawals().map(|w| w.iter().cloned().collect()).unwrap_or_default();
        let recovered_block = incoming
            .try_recover()
            .map_err(|_| eyre::eyre!("failed to recover senders for canonical payload"))?;
        let transactions = recovered_block
            .clone_transactions_recovered()
            .map(|tx| {
                let encoded = tx.encoded_2718();
                WithEncoded::new(encoded.into(), tx)
            })
            .collect();

        // 5. Synthesize a sequence-end BuildArgs identical in shape to what an End
        //    message would produce. `payload_id` matches the pending sequence so
        //    `prevalidate_incoming_sequence` accepts this as an incremental build;
        //    `sequence_end=true` triggers full SR computation + confirm-cache promotion.
        Ok(BuildArgs {
            payload_id: self.prefix_execution_meta.payload_id,
            base: OpFlashblockPayloadBase {
                parent_beacon_block_root: incoming_header
                    .parent_beacon_block_root()
                    .unwrap_or_default(),
                parent_hash: incoming_header.parent_hash(),
                fee_recipient: incoming_header.beneficiary(),
                prev_randao: incoming_header.mix_hash().unwrap_or_default(),
                block_number: incoming_header.number(),
                gas_limit: incoming_header.gas_limit(),
                timestamp: incoming_header.timestamp(),
                extra_data: incoming_header.extra_data().clone(),
                base_fee_per_gas: U256::from(
                    incoming_header.base_fee_per_gas().unwrap_or_default(),
                ),
            },
            transactions,
            // TODO: add support for BAL when OpPayload supports
            access_list: None,
            withdrawals,
            last_flashblock_index: self.prefix_execution_meta.last_flashblock_index,
            target_index: self.prefix_execution_meta.last_flashblock_index,
            sequence_end: true,
            canon_hash: Some(incoming_hash),
        })
    }

    /// Validates the incoming block header against this pending sequence.
    fn validate_incoming_header(&self, incoming: &N::BlockHeader) -> eyre::Result<()> {
        let expected = self.pending.executed_block.recovered_block.header();
        if expected.parent_hash() != incoming.parent_hash() {
            return Err(eyre::eyre!(
                "parent_hash mismatch: expected={}, incoming={}",
                expected.parent_hash(),
                incoming.parent_hash(),
            ));
        }
        if expected.beneficiary() != incoming.beneficiary() {
            return Err(eyre::eyre!(
                "fee_recipient mismatch: expected={}, incoming={}",
                expected.beneficiary(),
                incoming.beneficiary(),
            ));
        }
        if expected.mix_hash() != incoming.mix_hash() {
            return Err(eyre::eyre!(
                "prev_randao mismatch: expected={:?}, incoming={:?}",
                expected.mix_hash(),
                incoming.mix_hash(),
            ));
        }
        if expected.number() != incoming.number() {
            return Err(eyre::eyre!(
                "block_number mismatch: expected={}, incoming={}",
                expected.number(),
                incoming.number(),
            ));
        }
        if expected.gas_limit() != incoming.gas_limit() {
            return Err(eyre::eyre!(
                "gas_limit mismatch: expected={}, incoming={}",
                expected.gas_limit(),
                incoming.gas_limit(),
            ));
        }
        if expected.timestamp() != incoming.timestamp() {
            return Err(eyre::eyre!(
                "timestamp mismatch: expected={}, incoming={}",
                expected.timestamp(),
                incoming.timestamp(),
            ));
        }
        if expected.extra_data() != incoming.extra_data() {
            return Err(eyre::eyre!(
                "extra_data mismatch: expected={:?}, incoming={:?}",
                expected.extra_data(),
                incoming.extra_data(),
            ));
        }
        if expected.base_fee_per_gas() != incoming.base_fee_per_gas() {
            return Err(eyre::eyre!(
                "base_fee_per_gas mismatch: expected={:?}, incoming={:?}",
                expected.base_fee_per_gas(),
                incoming.base_fee_per_gas(),
            ));
        }
        if expected.parent_beacon_block_root() != incoming.parent_beacon_block_root() {
            return Err(eyre::eyre!(
                "parent_beacon_block_root mismatch: expected={:?}, incoming={:?}",
                expected.parent_beacon_block_root(),
                incoming.parent_beacon_block_root(),
            ));
        }
        Ok(())
    }

    /// Validates that the canonical payload's first `cached_tx_count` transactions
    /// match this pending sequence's already-executed prefix by `tx_hash`.
    fn validate_prefix_transactions(
        &self,
        canonical_txs: &[<<N::Block as BlockTrait>::Body as BlockBody>::Transaction],
    ) -> eyre::Result<()>
    where
        <<N::Block as BlockTrait>::Body as BlockBody>::Transaction: TxHashRef,
    {
        let prefix_count = self.prefix_execution_meta.cached_tx_count;
        if canonical_txs.len() < prefix_count {
            return Err(eyre::eyre!(
                "canonical block has {} txs but pending prefix expects {prefix_count}",
                canonical_txs.len(),
            ));
        }
        let pending_block = &self.pending.executed_block.recovered_block;
        for (i, (canon, pend)) in canonical_txs
            .iter()
            .zip(pending_block.transactions_recovered())
            .take(prefix_count)
            .enumerate()
        {
            let canon_hash = *canon.tx_hash();
            let pending_hash = *pend.tx_hash();
            if canon_hash != pending_hash {
                return Err(eyre::eyre!(
                    "prefix tx mismatch at index {i}: canon={canon_hash}, pending={pending_hash}",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_executed_block, mock_tx};
    use std::{collections::HashMap, time::Instant};

    use alloy_consensus::Receipt;
    use alloy_primitives::B256;
    use op_alloy_consensus::OpReceipt;

    use reth_optimism_primitives::OpPrimitives;
    use reth_rpc_eth_types::PendingBlock;

    fn make_pending_sequence(block_number: u64) -> PendingSequence<OpPrimitives> {
        let executed = make_executed_block(block_number, B256::ZERO);
        let block_hash = executed.recovered_block.hash();
        let pending_block = PendingBlock::with_executed_block(Instant::now(), executed);
        PendingSequence {
            pending: pending_block,
            tx_index: HashMap::new(),
            block_hash,
            parent_header: Default::default(),
            prefix_execution_meta: Default::default(),
            sequence_end: false,
        }
    }

    fn make_pending_sequence_with_txs(
        block_number: u64,
        tx_count: usize,
    ) -> PendingSequence<OpPrimitives> {
        let executed = make_executed_block(block_number, B256::ZERO);
        let block_hash = executed.recovered_block.hash();
        let mut tx_index = HashMap::new();
        for i in 0..tx_count {
            let tx = mock_tx(i as u64);
            let tx_hash = tx.tx_hash();
            let receipt = OpReceipt::Eip7702(Receipt {
                status: true.into(),
                cumulative_gas_used: 21_000 * (i as u64 + 1),
                logs: vec![],
            });
            tx_index.insert(
                tx_hash,
                CachedTxInfo { block_number, block_hash, tx_index: i as u64, tx, receipt },
            );
        }

        let pending_block = PendingBlock::with_executed_block(Instant::now(), executed);
        PendingSequence {
            pending: pending_block,
            tx_index,
            block_hash,
            parent_header: Default::default(),
            prefix_execution_meta: Default::default(),
            sequence_end: false,
        }
    }

    #[test]
    fn test_pending_sequence_get_hash_returns_stored_block_hash() {
        let cache = make_pending_sequence(42);
        assert_eq!(cache.get_hash(), cache.block_hash);
    }

    #[test]
    fn test_pending_sequence_get_height_returns_block_number() {
        let cache = make_pending_sequence(99);
        assert_eq!(cache.get_height(), 99);
    }

    #[test]
    fn test_pending_sequence_get_block_and_receipts_empty_receipts_on_no_tx_block() {
        let cache = make_pending_sequence(3);
        let bar = cache.get_block_and_receipts();
        assert!(bar.receipts.is_empty());
    }

    #[test]
    fn test_pending_sequence_get_tx_info_returns_none_for_unknown_hash() {
        let cache = make_pending_sequence_with_txs(10, 2);
        assert!(cache.get_tx_info(&B256::repeat_byte(0xFF)).is_none());
    }

    #[test]
    fn test_pending_sequence_get_tx_info_returns_correct_info_for_known_tx() {
        let cache = make_pending_sequence_with_txs(42, 3);
        let (tx_hash, expected_info) = cache.tx_index.iter().next().unwrap();
        let (info, bar) = cache.get_tx_info(tx_hash).expect("known tx hash should return Some");
        assert_eq!(info.block_number, 42);
        assert_eq!(info.block_hash, cache.block_hash);
        assert_eq!(info.tx_index, expected_info.tx_index);
        assert_eq!(*info.tx.tx_hash(), *tx_hash);
        assert_eq!(bar.block.number(), 42);
    }
}
