use alloy_consensus::{
    constants::EMPTY_WITHDRAWALS, Block, BlockBody, Header, EMPTY_OMMER_ROOT_HASH,
};
use alloy_eips::{eip7685::EMPTY_REQUESTS_HASH, merge::BEACON_NONCE};
use alloy_primitives::{Bloom, B256};
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;
use reth_errors::BlockExecutionError;
use reth_optimism_consensus::isthmus;
use reth_optimism_forks::OpHardforks;
use reth_provider::StateProvider;
use reth_revm::db::BundleState;

/// Input for [`assemble_flashblock`] bundling pre-computed roots and execution data.
pub(crate) struct FlashblockAssemblerInput<'a, T> {
    /// The flashblock base payload attributes.
    pub base: &'a OpFlashblockPayloadBase,
    /// Pre-computed state root.
    pub state_root: B256,
    /// Pre-computed transaction root.
    pub transactions_root: B256,
    /// Pre-computed receipts root.
    pub receipts_root: B256,
    /// Pre-computed logs bloom.
    pub logs_bloom: Bloom,
    /// Total gas used by the block.
    pub gas_used: u64,
    /// Total blob gas used by the block.
    pub blob_gas_used: u64,
    /// Bundle state from execution (for isthmus `withdrawals_root` computation).
    pub bundle_state: &'a BundleState,
    /// State provider for isthmus `withdrawals_root` computation.
    pub state_provider: &'a dyn StateProvider,
    /// Signed transactions for the block body.
    pub transactions: Vec<T>,
}

/// Assembles a flashblock ([`Block`]) from pre-computed roots and execution output.
///
/// Mirrors `OpBlockAssembler::assemble_block()` for hardfork-dependent header fields
/// (`withdrawals_root`, `requests_hash`, `blob_gas_used`, `excess_blob_gas`) but uses
/// pre-computed `transactions_root`, `receipts_root`, `logs_bloom`, and `state_root`
/// directly instead of recomputing them.
pub(crate) fn assemble_flashblock<ChainSpec, T>(
    chain_spec: &ChainSpec,
    input: FlashblockAssemblerInput<'_, T>,
) -> Result<Block<T>, BlockExecutionError>
where
    ChainSpec: OpHardforks,
{
    let FlashblockAssemblerInput {
        base,
        state_root,
        transactions_root,
        receipts_root,
        logs_bloom,
        gas_used,
        blob_gas_used,
        bundle_state,
        state_provider,
        transactions,
    } = input;

    let timestamp = base.timestamp;
    let mut requests_hash = None;

    let withdrawals_root = if chain_spec.is_isthmus_active_at_timestamp(timestamp) {
        requests_hash = Some(EMPTY_REQUESTS_HASH);
        Some(
            isthmus::withdrawals_root(bundle_state, state_provider)
                .map_err(BlockExecutionError::other)?,
        )
    } else if chain_spec.is_canyon_active_at_timestamp(timestamp) {
        Some(EMPTY_WITHDRAWALS)
    } else {
        None
    };

    let (excess_blob_gas, blob_gas_used) = if chain_spec.is_jovian_active_at_timestamp(timestamp) {
        (Some(0), Some(blob_gas_used))
    } else if chain_spec.is_ecotone_active_at_timestamp(timestamp) {
        (Some(0), Some(0))
    } else {
        (None, None)
    };

    let header = Header {
        parent_hash: base.parent_hash,
        ommers_hash: EMPTY_OMMER_ROOT_HASH,
        beneficiary: base.fee_recipient,
        state_root,
        transactions_root,
        receipts_root,
        withdrawals_root,
        logs_bloom,
        timestamp,
        mix_hash: base.prev_randao,
        nonce: BEACON_NONCE.into(),
        base_fee_per_gas: Some(base.base_fee_per_gas.saturating_to()),
        number: base.block_number,
        gas_limit: base.gas_limit,
        difficulty: Default::default(),
        gas_used,
        extra_data: base.extra_data.clone(),
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        blob_gas_used,
        excess_blob_gas,
        requests_hash,
        block_access_list_hash: None,
        slot_number: None,
    };

    Ok(Block::new(
        header,
        BlockBody {
            transactions,
            ommers: Default::default(),
            withdrawals: chain_spec.is_canyon_active_at_timestamp(timestamp).then(Default::default),
        },
    ))
}
