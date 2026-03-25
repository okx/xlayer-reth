use std::sync::Arc;

use alloy_consensus::{Header, Receipt, TxEip7702};
use alloy_primitives::{Address, Bloom, Bytes, Signature, B256, U256};
use alloy_rpc_types_engine::PayloadId;
use op_alloy_consensus::OpTypedTransaction;
use op_alloy_rpc_types_engine::{
    OpFlashblockPayload, OpFlashblockPayloadBase, OpFlashblockPayloadDelta,
    OpFlashblockPayloadMetadata,
};

use reth_chain_state::{ComputedTrieData, ExecutedBlock};
use reth_execution_types::{BlockExecutionOutput, BlockExecutionResult};
use reth_optimism_primitives::{
    OpBlock, OpBlockBody, OpPrimitives, OpReceipt, OpTransactionSigned,
};
use reth_primitives_traits::{RecoveredBlock, SealedBlock, SealedHeader};

pub(crate) fn mock_tx(nonce: u64) -> OpTransactionSigned {
    let tx = TxEip7702 {
        chain_id: 1u64,
        nonce,
        max_fee_per_gas: 0x28f000fff,
        max_priority_fee_per_gas: 0x28f000fff,
        gas_limit: 10,
        to: Address::default(),
        value: U256::from(3_u64),
        input: Bytes::from(vec![1, 2]),
        access_list: Default::default(),
        authorization_list: Default::default(),
    };
    let signature = Signature::new(U256::default(), U256::default(), true);
    OpTransactionSigned::new_unhashed(OpTypedTransaction::Eip7702(tx), signature)
}

pub(crate) fn make_executed_block(
    block_number: u64,
    parent_hash: B256,
) -> ExecutedBlock<OpPrimitives> {
    let header = Header { number: block_number, parent_hash, ..Default::default() };
    let sealed_header = SealedHeader::seal_slow(header);
    let block = OpBlock::new(sealed_header.unseal(), Default::default());
    let sealed_block = SealedBlock::seal_slow(block);
    let recovered_block = RecoveredBlock::new_sealed(sealed_block, vec![]);
    let execution_output = Arc::new(BlockExecutionOutput {
        result: BlockExecutionResult {
            receipts: vec![],
            requests: Default::default(),
            gas_used: 0,
            blob_gas_used: 0,
        },
        state: Default::default(),
    });
    ExecutedBlock::new(Arc::new(recovered_block), execution_output, ComputedTrieData::default())
}

pub(crate) fn empty_receipts() -> Arc<Vec<OpReceipt>> {
    Arc::new(vec![])
}

pub(crate) fn make_executed_block_with_txs(
    block_number: u64,
    parent_hash: B256,
    nonce_start: u64,
    count: usize,
) -> (ExecutedBlock<OpPrimitives>, Arc<Vec<OpReceipt>>) {
    let txs: Vec<OpTransactionSigned> =
        (0..count).map(|i| mock_tx(nonce_start + i as u64)).collect();
    let senders: Vec<Address> = (0..count).map(|_| Address::default()).collect();
    let receipts: Vec<OpReceipt> = (0..count)
        .map(|i| {
            OpReceipt::Eip7702(Receipt {
                status: true.into(),
                cumulative_gas_used: 21_000 * (i as u64 + 1),
                logs: vec![],
            })
        })
        .collect();

    let header = Header { number: block_number, parent_hash, ..Default::default() };
    let sealed_header = SealedHeader::seal_slow(header);
    let body = OpBlockBody { transactions: txs, ..Default::default() };
    let block = OpBlock::new(sealed_header.unseal(), body);
    let sealed_block = SealedBlock::seal_slow(block);
    let recovered_block = RecoveredBlock::new_sealed(sealed_block, senders);
    let execution_output = Arc::new(BlockExecutionOutput {
        result: BlockExecutionResult {
            receipts: receipts.clone(),
            requests: Default::default(),
            gas_used: 21_000 * count as u64,
            blob_gas_used: 0,
        },
        state: Default::default(),
    });
    let executed = ExecutedBlock::new(
        Arc::new(recovered_block),
        execution_output,
        ComputedTrieData::default(),
    );
    (executed, Arc::new(receipts))
}

#[derive(Debug)]
pub(crate) struct TestFlashBlockFactory {
    /// Block time in seconds (used to auto-increment timestamps)
    block_time: u64,
    /// Starting timestamp for the first block
    base_timestamp: u64,
    /// Current block number being tracked
    current_block_number: u64,
}

impl TestFlashBlockFactory {
    /// Use [`with_block_time`](Self::with_block_time) to customize the block time.
    pub(crate) fn new() -> Self {
        Self { block_time: 2, base_timestamp: 1_000_000, current_block_number: 100 }
    }

    #[allow(dead_code)]
    pub(crate) fn with_block_time(mut self, block_time: u64) -> Self {
        self.block_time = block_time;
        self
    }

    pub(crate) fn flashblock_at(&self, index: u64) -> TestFlashBlockBuilder {
        self.builder().index(index).block_number(self.current_block_number)
    }

    pub(crate) fn flashblock_after(&self, previous: &OpFlashblockPayload) -> TestFlashBlockBuilder {
        let parent_hash =
            previous.base.as_ref().map(|b| b.parent_hash).unwrap_or(previous.diff.block_hash);

        self.builder()
            .index(previous.index + 1)
            .block_number(previous.metadata.block_number)
            .payload_id(previous.payload_id)
            .parent_hash(parent_hash)
            .timestamp(previous.base.as_ref().map(|b| b.timestamp).unwrap_or(self.base_timestamp))
    }

    pub(crate) fn flashblock_for_next_block(
        &self,
        previous: &OpFlashblockPayload,
    ) -> TestFlashBlockBuilder {
        let prev_timestamp =
            previous.base.as_ref().map(|b| b.timestamp).unwrap_or(self.base_timestamp);

        self.builder()
            .index(0)
            .block_number(previous.metadata.block_number + 1)
            .payload_id(PayloadId::new(B256::random().0[0..8].try_into().unwrap()))
            .parent_hash(previous.diff.block_hash)
            .timestamp(prev_timestamp + self.block_time)
    }

    pub(crate) fn builder(&self) -> TestFlashBlockBuilder {
        TestFlashBlockBuilder {
            index: 0,
            block_number: self.current_block_number,
            payload_id: PayloadId::new([1u8; 8]),
            parent_hash: B256::random(),
            timestamp: self.base_timestamp,
            base: None,
            block_hash: B256::random(),
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: Bloom::default(),
            gas_used: 0,
            transactions: vec![],
            withdrawals: vec![],
            withdrawals_root: B256::ZERO,
            blob_gas_used: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TestFlashBlockBuilder {
    index: u64,
    block_number: u64,
    payload_id: PayloadId,
    parent_hash: B256,
    timestamp: u64,
    base: Option<OpFlashblockPayloadBase>,
    block_hash: B256,
    state_root: B256,
    receipts_root: B256,
    logs_bloom: Bloom,
    gas_used: u64,
    transactions: Vec<Bytes>,
    withdrawals: Vec<alloy_eips::eip4895::Withdrawal>,
    withdrawals_root: B256,
    blob_gas_used: Option<u64>,
}

impl TestFlashBlockBuilder {
    pub(crate) fn index(mut self, index: u64) -> Self {
        self.index = index;
        self
    }

    pub(crate) fn block_number(mut self, block_number: u64) -> Self {
        self.block_number = block_number;
        self
    }

    pub(crate) fn payload_id(mut self, payload_id: PayloadId) -> Self {
        self.payload_id = payload_id;
        self
    }

    pub(crate) fn parent_hash(mut self, parent_hash: B256) -> Self {
        self.parent_hash = parent_hash;
        self
    }

    pub(crate) fn timestamp(mut self, timestamp: u64) -> Self {
        self.timestamp = timestamp;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn base(mut self, base: OpFlashblockPayloadBase) -> Self {
        self.base = Some(base);
        self
    }

    #[allow(dead_code)]
    pub(crate) fn block_hash(mut self, block_hash: B256) -> Self {
        self.block_hash = block_hash;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn state_root(mut self, state_root: B256) -> Self {
        self.state_root = state_root;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn receipts_root(mut self, receipts_root: B256) -> Self {
        self.receipts_root = receipts_root;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn transactions(mut self, transactions: Vec<Bytes>) -> Self {
        self.transactions = transactions;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn gas_used(mut self, gas_used: u64) -> Self {
        self.gas_used = gas_used;
        self
    }

    pub(crate) fn build(mut self) -> OpFlashblockPayload {
        // Auto-create base for index 0 if not set
        if self.index == 0 && self.base.is_none() {
            self.base = Some(OpFlashblockPayloadBase {
                parent_hash: self.parent_hash,
                parent_beacon_block_root: B256::random(),
                fee_recipient: Address::default(),
                prev_randao: B256::random(),
                block_number: self.block_number,
                gas_limit: 30_000_000,
                timestamp: self.timestamp,
                extra_data: Default::default(),
                base_fee_per_gas: U256::from(1_000_000_000u64),
            });
        }

        OpFlashblockPayload {
            index: self.index,
            payload_id: self.payload_id,
            base: self.base,
            diff: OpFlashblockPayloadDelta {
                block_hash: self.block_hash,
                state_root: self.state_root,
                receipts_root: self.receipts_root,
                logs_bloom: self.logs_bloom,
                gas_used: self.gas_used,
                transactions: self.transactions,
                withdrawals: self.withdrawals,
                withdrawals_root: self.withdrawals_root,
                blob_gas_used: self.blob_gas_used,
            },
            metadata: OpFlashblockPayloadMetadata {
                block_number: self.block_number,
                receipts: Default::default(),
                new_account_balances: Default::default(),
            },
        }
    }
}
