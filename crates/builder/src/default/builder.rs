//! Default payload builder wrapper with flashblocks cache replay.
//!
//! Wraps [`reth_optimism_payload_builder::OpPayloadBuilder`] and intercepts
//! `try_build` to check the [`FlashblockPayloadsCache`] before delegating.
//! On cache hit, cached transactions are replayed via `BlockBuilder::execute_transaction`
//! and the payload is returned as `Freeze` (deterministic, no re-building).

use crate::flashblocks::utils::cache::FlashblockPayloadsCache;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

use alloy_consensus::{transaction::TxHashRef, Transaction, Typed2718};
use alloy_eips::eip2718::WithEncoded;
use alloy_evm::{block::TxResult as _, Evm as _};
use alloy_primitives::{Address, U256};
use op_alloy_consensus::OpTransaction;
use op_revm::{constants::L1_BLOCK_CONTRACT, L1BlockInfo};
use revm::Database as _;

use reth_basic_payload_builder::*;
use reth_chainspec::ChainSpecProvider;
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome, BlockExecutionError},
    ConfigureEvm,
};
use reth_execution_types::BlockExecutionOutput;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::OpPayloadBuilderAttributes;
use reth_optimism_payload_builder::{
    builder::OpPayloadBuilderCtx,
    builder::OpPayloadTransactions,
    error::OpPayloadBuilderError,
    payload::{OpBuiltPayload, OpPayloadAttrs},
    OpPayloadPrimitives,
};
use reth_optimism_txpool::{
    estimated_da_size::DataAvailabilitySized,
    interop::{is_valid_interop, MaybeInteropTransaction},
    OpPooledTx,
};
use reth_payload_builder_primitives::PayloadBuilderError;
use reth_payload_primitives::{BuildNextEnv, BuiltPayloadExecutedBlock};
use reth_payload_util::PayloadTransactions;
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_storage_api::StateProviderFactory;
use reth_transaction_pool::{PoolTransaction, TransactionPool};
use revm::context::Block as _;

/// The default optimism payload builder that wraps the additional builder p2p node
/// with flashblocks cache replay support. This is for flashblocks reorg protection
/// on failover to the default builder (when `flashblocks.enabled` = false).
///
/// When the P2P flashblocks sequence cache matches the current parent hash, cached
/// transactions are replayed instead of pulling from the txpool. Otherwise, payload
/// building is handled by the [`reth_optimism_payload_builder::OpPayloadBuilder`].
pub struct DefaultPayloadBuilder<Pool, Client, Evm, Txs, Attrs> {
    /// The inner default OP payload builder.
    inner: reth_optimism_payload_builder::OpPayloadBuilder<Pool, Client, Evm, Txs, Attrs>,
    /// Cache of flashblock payloads received via P2P, keyed by parent hash.
    p2p_cache: FlashblockPayloadsCache,
    /// XLOP-1100 (FR-2 面2): chain-level blacklist runtime context. When present AND the
    /// snapshot is non-empty, the cache-miss build path runs the normal-L2 commit-condition
    /// gate (check②). `None` / empty snapshot → fail-open, delegate to the upstream builder.
    blacklist_ctx: Option<xlayer_blacklist_node::BlacklistRuntimeCtx>,
}

impl<Pool, Client, Evm, Txs, Attrs> DefaultPayloadBuilder<Pool, Client, Evm, Txs, Attrs> {
    /// Creates a new [`DefaultPayloadBuilder`] wrapping the given inner builder.
    pub fn new(
        inner: reth_optimism_payload_builder::OpPayloadBuilder<Pool, Client, Evm, Txs, Attrs>,
        p2p_cache: FlashblockPayloadsCache,
        blacklist_ctx: Option<xlayer_blacklist_node::BlacklistRuntimeCtx>,
    ) -> Self {
        Self { inner, p2p_cache, blacklist_ctx }
    }
}

impl<Pool: Clone, Client: Clone, Evm: ConfigureEvm + Clone, Txs: Clone, Attrs> Clone
    for DefaultPayloadBuilder<Pool, Client, Evm, Txs, Attrs>
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            p2p_cache: self.p2p_cache.clone(),
            blacklist_ctx: self.blacklist_ctx.clone(),
        }
    }
}

impl<Pool, Client, Evm, N, Txs> PayloadBuilder
    for DefaultPayloadBuilder<Pool, Client, Evm, Txs, OpPayloadBuilderAttributes<N::SignedTx>>
where
    N: OpPayloadPrimitives,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: OpHardforks> + Clone,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = N::SignedTx>>,
    Evm: ConfigureEvm<
        Primitives = N,
        NextBlockEnvCtx: BuildNextEnv<
            OpPayloadBuilderAttributes<N::SignedTx>,
            N::BlockHeader,
            Client::ChainSpec,
        >,
    >,
    Txs: OpPayloadTransactions<Pool::Transaction>,
{
    type Attributes = OpPayloadAttrs;
    type BuiltPayload = OpBuiltPayload<N>;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        let parent_hash = args.config.parent_header.hash();

        // Check P2P cache for a flashblocks sequence matching this parent
        if let Some(cached_txs) = self
            .p2p_cache
            .get_flashblocks_sequence_txs::<N::SignedTx>(parent_hash)
            .filter(|txs| !txs.is_empty())
        {
            info!(
                target: "payload_builder",
                parent_hash = %parent_hash,
                cached_tx_count = cached_txs.len(),
                "Cache hit: replaying cached flashblocks transactions in default builder"
            );
            return self.build_with_cached_txs(args, cached_txs);
        }

        // Cache miss. XLOP-1100 (FR-2 面2): when the blacklist is active (chain has a mirror AND
        // the block-head snapshot is non-empty), build via the gated path so normal-L2 txs that
        // hit a blacklisted address via a Transfer event (check②) are excluded. Otherwise (feature
        // off / empty list) delegate unchanged to the upstream builder — zero behavior change.
        let bl_active =
            self.blacklist_ctx.as_ref().map(|c| !c.load_snapshot().is_empty()).unwrap_or(false);
        if bl_active {
            self.build_with_gate(args)
        } else {
            self.inner.try_build(args)
        }
    }

    fn on_missing_payload(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        self.inner.on_missing_payload(args)
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, N::BlockHeader>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        self.inner.build_empty_payload(config)
    }
}

impl<Pool, Client, Evm, N, Txs>
    DefaultPayloadBuilder<Pool, Client, Evm, Txs, OpPayloadBuilderAttributes<N::SignedTx>>
where
    N: OpPayloadPrimitives,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: OpHardforks> + Clone,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = N::SignedTx>>,
    Evm: ConfigureEvm<
        Primitives = N,
        NextBlockEnvCtx: BuildNextEnv<
            OpPayloadBuilderAttributes<N::SignedTx>,
            N::BlockHeader,
            Client::ChainSpec,
        >,
    >,
    Txs: OpPayloadTransactions<Pool::Transaction>,
{
    /// Builds a payload by replaying cached flashblocks transactions.
    ///
    /// Replicates the [`OpBuilder::build`] flow but replaces `execute_best_transactions`
    /// with direct cached transaction execution via [`BlockBuilder::execute_transaction`].
    fn build_with_cached_txs(
        &self,
        args: BuildArguments<OpPayloadAttrs, OpBuiltPayload<N>>,
        cached_txs: Vec<WithEncoded<alloy_consensus::transaction::Recovered<N::SignedTx>>>,
    ) -> Result<BuildOutcome<OpBuiltPayload<N>>, PayloadBuilderError> {
        let BuildArguments { mut cached_reads, config, cancel, best_payload, .. } = args;

        // Convert engine API attrs to internal builder attrs, decoding sequencer txs.
        let parent_hash = config.parent_header.hash();
        let payload_id = config.payload_id;
        let builder_attrs = OpPayloadBuilderAttributes::from_rpc_attrs(
            parent_hash,
            payload_id,
            config.attributes.0,
        )
        .map_err(PayloadBuilderError::other)?;
        let config = PayloadConfig {
            parent_header: config.parent_header,
            attributes: builder_attrs,
            payload_id,
        };

        let ctx = OpPayloadBuilderCtx {
            evm_config: self.inner.evm_config.clone(),
            builder_config: self.inner.config.clone(),
            chain_spec: self.inner.client.chain_spec(),
            config,
            cancel,
            best_payload,
        };

        let state_provider = self.inner.client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);
        let mut db = State::builder()
            .with_database(cached_reads.as_db_mut(state))
            .with_bundle_update()
            .build();

        // Pre-load L1 block contract into cache (required for DA footprint gas scalar)
        db.load_cache_account(L1_BLOCK_CONTRACT).map_err(BlockExecutionError::other)?;

        let mut builder = ctx.block_builder(&mut db)?;

        // 1. Apply pre-execution changes (system calls)
        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes during cache replay");
            PayloadBuilderError::Internal(err.into())
        })?;

        // 2. Execute sequencer transactions from payload attributes
        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;

        // 3. Execute cached transactions (replaces execute_best_transactions)
        let mut block_gas_limit = builder.evm_mut().block().gas_limit();
        if let Some(gas_limit_config) = ctx.builder_config.gas_limit_config.gas_limit() {
            // If a gas limit is configured, use that limit as target if it's smaller, otherwise use
            // the block's actual gas limit.
            block_gas_limit = gas_limit_config.min(block_gas_limit);
        }
        let block_da_limit = ctx.builder_config.da_config.max_da_block_size();
        let tx_da_limit = ctx.builder_config.da_config.max_da_tx_size();
        let base_fee = builder.evm_mut().block().basefee();

        // The execution result is discarded here since even on replay errors, we
        // will resolve the payload till whichever point the replay failed.
        let _ = execute_cached_transactions::<N, _>(
            &mut info,
            &mut builder,
            cached_txs,
            base_fee,
            block_gas_limit,
            tx_da_limit,
            block_da_limit,
        )
        .inspect_err(|e| {
            warn!(
                target: "payload_builder",
                "Failed replaying cached flashblocks sequence fully, error: {e}",
            );
        });

        // 4. Finish: compute state root, transaction root, receipt root
        let BlockBuilderOutcome { execution_result, hashed_state, trie_updates, block } =
            builder.finish(&state_provider, None)?;

        let sealed_block = Arc::new(block.sealed_block().clone());

        let execution_outcome =
            BlockExecutionOutput { state: db.take_bundle(), result: execution_result };

        let executed: BuiltPayloadExecutedBlock<N> = BuiltPayloadExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_outcome),
            hashed_state: either::Either::Left(Arc::new(hashed_state)),
            trie_updates: either::Either::Left(Arc::new(trie_updates)),
        };

        let payload =
            OpBuiltPayload::new(ctx.payload_id(), sealed_block, info.total_fees, Some(executed));

        info!(
            target: "payload_builder",
            id = %ctx.payload_id(),
            block_hash = %payload.block().hash(),
            "Successfully built payload from cached flashblocks transactions"
        );

        // Freeze: deterministic payload from cached sequence, no re-building needed
        Ok(BuildOutcomeKind::Freeze(payload).with_cached_reads(cached_reads))
    }

    /// XLOP-1100 (FR-2 面2): cache-miss build with the normal-L2 blacklist gate.
    ///
    /// Mirrors the upstream `OpBuilder::build` flow (block_builder, apply_pre_execution_changes,
    /// execute_sequencer_transactions, best-txs, finish), keeping deposit included-as-reverted and
    /// snapshot refresh on the (hooked) executor. Only the pool best-txs loop is replaced by a
    /// gated one: each tx runs via `execute_transaction_with_commit_condition`, and a tx whose
    /// committed logs hit the blacklist (check②, Transfer event) is discarded (CommitChanges::No),
    /// marked invalid for this build, and scheduled for physical pool removal. Only reached when the
    /// blacklist is active (caller-checked); check①/③ are out of scope on this path (see plan).
    ///
    /// NOTE: keep the per-tx gate in sync with
    /// `crate::flashblocks::context::FlashblocksBuilderCtx::execute_best_transactions`.
    fn build_with_gate(
        &self,
        args: BuildArguments<OpPayloadAttrs, OpBuiltPayload<N>>,
    ) -> Result<BuildOutcome<OpBuiltPayload<N>>, PayloadBuilderError> {
        let BuildArguments { mut cached_reads, config, cancel, best_payload, .. } = args;

        let parent_hash = config.parent_header.hash();
        let payload_id = config.payload_id;
        let builder_attrs = OpPayloadBuilderAttributes::from_rpc_attrs(
            parent_hash,
            payload_id,
            config.attributes.0,
        )
        .map_err(PayloadBuilderError::other)?;
        let config = PayloadConfig {
            parent_header: config.parent_header,
            attributes: builder_attrs,
            payload_id,
        };

        let ctx = OpPayloadBuilderCtx {
            evm_config: self.inner.evm_config.clone(),
            builder_config: self.inner.config.clone(),
            chain_spec: self.inner.client.chain_spec(),
            config,
            cancel,
            best_payload,
        };

        // Snapshot read once at block head (caller guaranteed non-empty).
        let state_provider = self.inner.client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);
        let mut db = State::builder()
            .with_database(cached_reads.as_db_mut(state))
            .with_bundle_update()
            .build();
        db.load_cache_account(L1_BLOCK_CONTRACT).map_err(BlockExecutionError::other)?;

        let mut builder = ctx.block_builder(&mut db)?;
        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes (gated build)");
            PayloadBuilderError::Internal(err.into())
        })?;
        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;

        let mut txs_to_evict: Vec<alloy_primitives::TxHash> = Vec::new();

        if !ctx.attributes().no_tx_pool {
            let mut block_gas_limit = builder.evm_mut().block().gas_limit();
            if let Some(gas_limit_config) = ctx.builder_config.gas_limit_config.gas_limit() {
                block_gas_limit = gas_limit_config.min(block_gas_limit);
            }
            let block_da_limit = ctx.builder_config.da_config.max_da_block_size();
            let tx_da_limit = ctx.builder_config.da_config.max_da_tx_size();
            let base_fee = builder.evm_mut().block().basefee();
            let coinbase = builder.evm_mut().block().beneficiary();

            // check③ pre-balances. The commit-condition closure sees the post-tx state diff but
            // has no db handle, so seed each listed address's committed balance ONCE here (after
            // pre-exec + sequencer txs) and advance it after every committed tx. O(list) once per
            // block — fine for emergency-freeze-sized lists; this is the rare failsafe path.
            let snapshot = self
                .blacklist_ctx
                .as_ref()
                .expect("build_with_gate is only entered when blacklist_ctx is Some")
                .load_snapshot();
            let mut listed_balances: HashMap<Address, U256> = snapshot
                .iter()
                .map(|addr| {
                    let bal = builder
                        .evm_mut()
                        .db_mut()
                        .basic(*addr)
                        .ok()
                        .flatten()
                        .map(|i| i.balance)
                        .unwrap_or_default();
                    (*addr, bal)
                })
                .collect();

            let best_attrs = ctx.best_transaction_attributes(builder.evm_mut().block());
            let best_source = self.inner.best_transactions.clone();
            let mut best_txs = best_source.best_transactions(self.inner.pool.clone(), best_attrs);

            while let Some(pool_tx) = best_txs.next(()) {
                let interop = pool_tx.interop_deadline();
                let tx_da_size = pool_tx.estimated_da_size();
                let tx = pool_tx.into_consensus();

                let da_footprint_gas_scalar = ctx
                    .chain_spec
                    .is_jovian_active_at_timestamp(ctx.attributes().timestamp())
                    .then_some(
                        L1BlockInfo::fetch_da_footprint_gas_scalar(builder.evm_mut().db_mut())
                            .expect("DA footprint should always be available post jovian"),
                    );

                if info.is_tx_over_limits(
                    tx_da_size,
                    block_gas_limit,
                    tx_da_limit,
                    block_da_limit,
                    tx.gas_limit(),
                    da_footprint_gas_scalar,
                ) {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    continue;
                }
                // A sequencer block never contains blob/deposit txs from the pool.
                if tx.is_eip4844() || tx.is_deposit() {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    continue;
                }
                if let Some(interop) = interop
                    && !is_valid_interop(interop, ctx.config.attributes.timestamp())
                {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    continue;
                }
                if ctx.cancel.is_cancelled() {
                    // Stop pulling txs; finish the block built so far (mirrors the cache-replay
                    // path, which also finishes partial on interruption).
                    break;
                }

                // XLOP-1100 gate: evaluate check②(committed Transfer-class logs) + check③(native
                // -ETH balance change, with the L2 gas fee stripped) on the tx's committed
                // effects. On a hit the tx is discarded (state not committed), dropped from this
                // build, and scheduled for physical pool removal. Fully-qualified to the
                // `BlockBuilder` method (the builder also satisfies `BlockExecutor`, whose
                // same-named method tracks no block transactions). check① (pure CALL touch with
                // no event/balance change) is NOT covered here — no inspector on this path.
                let tx_hash = *tx.tx_hash();
                let sender = tx.signer();
                let effective_gas_price = tx.effective_gas_price(Some(base_fee));
                let committed = BlockBuilder::execute_transaction_with_commit_condition(
                    &mut builder,
                    tx.clone(),
                    |result| {
                        let ras = result.result();
                        let listed_changes: Vec<_> = ras
                            .state
                            .iter()
                            .filter(|(addr, _)| snapshot.contains(addr))
                            .map(|(addr, acct)| xlayer_blacklist_node::ListedBalanceChange {
                                address: *addr,
                                balance_start: listed_balances
                                    .get(addr)
                                    .copied()
                                    .unwrap_or_default(),
                                balance_end: acct.info.balance,
                            })
                            .collect();
                        let fee = xlayer_blacklist_node::FeeContext {
                            sender,
                            coinbase,
                            gas_used: ras.result.tx_gas_used(),
                            effective_gas_price,
                            base_fee,
                        };
                        match gate_decision(ras.result.logs(), listed_changes, &fee, &snapshot) {
                            Some(hit) => {
                                if let Some(bl) = self.blacklist_ctx.as_ref() {
                                    bl.record_exec_revert(&hit);
                                }
                                reth_evm::block::CommitChanges::No
                            }
                            None => {
                                // Committed: advance the running pre-balance for listed addresses
                                // this tx changed, so the next tx sees the correct pre-balance.
                                for (addr, acct) in ras.state.iter() {
                                    if snapshot.contains(addr) {
                                        listed_balances.insert(*addr, acct.info.balance);
                                    }
                                }
                                reth_evm::block::CommitChanges::Yes
                            }
                        }
                    },
                )
                .map_err(|err| PayloadBuilderError::EvmExecutionError(Box::new(err)))?;

                match committed {
                    Some(gas_used) => {
                        info.cumulative_gas_used += gas_used;
                        info.cumulative_da_bytes_used += tx_da_size;
                        let miner_fee = tx
                            .effective_tip_per_gas(base_fee)
                            .expect("fee is always valid; execution succeeded");
                        info.total_fees += U256::from(miner_fee) * U256::from(gas_used);
                    }
                    None => {
                        // Blacklist hit: excluded from the block (metric recorded above).
                        best_txs.mark_invalid(tx.signer(), tx.nonce());
                        txs_to_evict.push(tx_hash);
                        continue;
                    }
                }
            }
        }

        let BlockBuilderOutcome { execution_result, hashed_state, trie_updates, block } =
            builder.finish(&state_provider, None)?;

        let sealed_block = Arc::new(block.sealed_block().clone());
        let execution_outcome =
            BlockExecutionOutput { state: db.take_bundle(), result: execution_result };
        let executed: BuiltPayloadExecutedBlock<N> = BuiltPayloadExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_outcome),
            hashed_state: either::Either::Left(Arc::new(hashed_state)),
            trie_updates: either::Either::Left(Arc::new(trie_updates)),
        };
        let payload =
            OpBuiltPayload::new(ctx.payload_id(), sealed_block, info.total_fees, Some(executed));

        // Physically remove blacklisted-hit txs from the pool (aligns with the flashblocks
        // path + the per-block eviction added earlier). Best-effort; ignore the returned set.
        if !txs_to_evict.is_empty() {
            let _ = self.inner.pool.remove_transactions(txs_to_evict);
        }

        Ok(BuildOutcomeKind::Better { payload }.with_cached_reads(cached_reads))
    }
}

/// Executes cached flashblocks transactions via [`BlockBuilder::execute_transaction`].
fn execute_cached_transactions<N, Builder>(
    info: &mut reth_optimism_payload_builder::builder::ExecutionInfo,
    builder: &mut Builder,
    cached_txs: Vec<WithEncoded<alloy_consensus::transaction::Recovered<N::SignedTx>>>,
    base_fee: u64,
    block_gas_limit: u64,
    tx_da_limit: Option<u64>,
    block_da_limit: Option<u64>,
) -> Result<(), PayloadBuilderError>
where
    N: OpPayloadPrimitives,
    Builder: BlockBuilder<Primitives = N>,
{
    for tx_encoded in cached_txs {
        let (encoded_bytes, recovered_tx) = tx_encoded.split();

        // ensure transaction is valid
        let tx_da_size = op_alloy_flz::tx_estimated_size_fjord_bytes(encoded_bytes.as_ref());
        if info.is_tx_over_limits(
            tx_da_size,
            block_gas_limit,
            tx_da_limit,
            block_da_limit,
            recovered_tx.gas_limit(),
            None, // DA footprint gas scalar — skipped (requires L1BlockInfo DB fetch)
        ) {
            return Err(PayloadBuilderError::Other(
                eyre::eyre!(
                    "invalid flashblocks sequence, tx {} over block limits",
                    recovered_tx.tx_hash(),
                )
                .into(),
            ));
        }
        if recovered_tx.is_eip4844() {
            return Err(PayloadBuilderError::other(OpPayloadBuilderError::BlobTransactionRejected));
        }
        if recovered_tx.is_deposit() {
            return Err(PayloadBuilderError::Other(
                eyre::eyre!("invalid flashblocks sequence, deposit transaction rejected").into(),
            ));
        }

        // Ensure transaction execution is valid
        let gas_used = builder
            .execute_transaction(recovered_tx.clone())
            .map_err(|err| PayloadBuilderError::EvmExecutionError(Box::new(err)))?;

        info.cumulative_gas_used += gas_used;
        info.cumulative_da_bytes_used += tx_da_size;

        // Track fees for payload metadata
        let miner_fee = recovered_tx
            .effective_tip_per_gas(base_fee)
            .expect("fee is always valid; execution succeeded");
        info.total_fees += U256::from(miner_fee) * U256::from(gas_used);
    }

    Ok(())
}

/// XLOP-1100 (FR-2 面2): the normal-L2 blacklist decision used by `build_with_gate`.
///
/// check②(committed Transfer-class logs) + check③(native-ETH balance change), the latter with
/// the L2 gas fee stripped via [`reconstruct_balance_candidates`] (so a listed sender's gas
/// payment or a listed coinbase's fee receipt is not mistaken for a transfer). check① (pure
/// CALL touch) is out of scope on this path (no inspector). Pure → unit-tested below.
///
/// metric caveat: `selfdestruct_targets` is passed empty here (no inspector on this path), so a
/// selfdestruct-funded hit to a listed address is still DROPPED but recorded under the
/// `eth_balance` hook bucket rather than `selfdestruct` (unlike the flashblocks path). Interception
/// is unchanged; only the metric category label differs. The `selfdestruct` categorization is
/// covered by the lower-level `xlayer_blacklist::eval` tests.
fn gate_decision(
    logs: &[alloy_primitives::Log],
    listed_changes: Vec<xlayer_blacklist_node::ListedBalanceChange>,
    fee: &xlayer_blacklist_node::FeeContext,
    snapshot: &xlayer_blacklist::BlacklistSnapshot,
) -> Option<xlayer_blacklist::Hit> {
    let mut insp = xlayer_blacklist::BlacklistInspector::new();
    for cand in xlayer_blacklist_node::reconstruct_balance_candidates(listed_changes, fee, &[]) {
        insp.record_balance_candidate(cand);
    }
    xlayer_blacklist::BlacklistEvaluator::evaluate(&insp, logs, snapshot)
}

#[cfg(test)]
mod gate_tests {
    use super::gate_decision;
    use alloy_primitives::{address, Address, Log, LogData, U256};
    use xlayer_blacklist::{eval::TRANSFER_TOPIC0, BlacklistSnapshot, HitCategory};
    use xlayer_blacklist_node::{FeeContext, ListedBalanceChange};

    const VICTIM: Address = address!("00000000000000000000000000000000000000aa");
    const SENDER: Address = address!("00000000000000000000000000000000000000b0");
    const COINBASE: Address = address!("00000000000000000000000000000000000000cb");
    const TOKEN: Address = address!("00000000000000000000000000000000000000dd");

    fn snap(addrs: &[Address]) -> BlacklistSnapshot {
        addrs.iter().copied().collect()
    }

    fn fee(sender: Address, coinbase: Address, gas_used: u64, egp: u128, base: u64) -> FeeContext {
        FeeContext { sender, coinbase, gas_used, effective_gas_price: egp, base_fee: base }
    }

    fn transfer_log(from: Address, to: Address) -> Log {
        let topics = vec![*TRANSFER_TOPIC0, from.into_word(), to.into_word()];
        Log { address: TOKEN, data: LogData::new(topics, Default::default()).expect("log data") }
    }

    #[test]
    fn balance_inflow_to_listed_hits() {
        // C7 (forwarder inner CALL value) and C15 (selfdestruct beneficiary): a listed addr
        // (neither sender nor coinbase) gains ETH with NO Transfer event → only check③ catches
        // it. NOTE: this path has no inspector, so a selfdestruct-funded hit is categorized as
        // `EthBalance` here (not `SelfDestruct`) — interception is unchanged, only the metric
        // label differs (see `gate_decision` doc). The `SelfDestruct` category is asserted by the
        // lower-level `xlayer_blacklist::eval` tests.
        let changes = vec![ListedBalanceChange {
            address: VICTIM,
            balance_start: U256::ZERO,
            balance_end: U256::from(1u64),
        }];
        let hit =
            gate_decision(&[], changes, &fee(SENDER, COINBASE, 21_000, 10, 7), &snap(&[VICTIM]))
                .expect("hit");
        assert_eq!(hit.category, HitCategory::EthBalance);
        assert_eq!(hit.address, VICTIM);
    }

    #[test]
    fn balance_outflow_from_listed_hits() {
        // check③ also catches ETH leaving a listed address (e.g. drained via a contract) when
        // that address is neither sender nor coinbase (no fee to strip) → net != 0 → hit.
        let changes = vec![ListedBalanceChange {
            address: VICTIM,
            balance_start: U256::from(100u64),
            balance_end: U256::from(40u64),
        }];
        let hit =
            gate_decision(&[], changes, &fee(SENDER, COINBASE, 21_000, 10, 7), &snap(&[VICTIM]))
                .expect("hit");
        assert_eq!(hit.category, HitCategory::EthBalance);
        assert_eq!(hit.address, VICTIM);
    }

    #[test]
    fn multiple_listed_changed_one_tx_hits() {
        // A tx touching several listed addresses still yields a hit (which one is returned is
        // unspecified — `evaluate` returns the first balance candidate; both are listed).
        let other = address!("00000000000000000000000000000000000000a1");
        let changes = vec![
            ListedBalanceChange {
                address: VICTIM,
                balance_start: U256::ZERO,
                balance_end: U256::from(1u64),
            },
            ListedBalanceChange {
                address: other,
                balance_start: U256::ZERO,
                balance_end: U256::from(2u64),
            },
        ];
        let hit = gate_decision(
            &[],
            changes,
            &fee(SENDER, COINBASE, 21_000, 10, 7),
            &snap(&[VICTIM, other]),
        )
        .expect("hit");
        assert_eq!(hit.category, HitCategory::EthBalance);
    }

    #[test]
    fn empty_snapshot_fail_open() {
        // Disabled chain / empty list → never a hit (try_build also short-circuits before the
        // gate, but the decision itself must be fail-open too).
        let changes = vec![ListedBalanceChange {
            address: VICTIM,
            balance_start: U256::ZERO,
            balance_end: U256::from(1u64),
        }];
        assert!(gate_decision(
            &[transfer_log(SENDER, VICTIM)],
            changes,
            &fee(SENDER, COINBASE, 21_000, 10, 7),
            &BlacklistSnapshot::empty()
        )
        .is_none());
    }

    #[test]
    fn transfer_event_to_listed_hits() {
        // check②: internal Transfer to a listed address (forwarder/proxy), clean top-level.
        let hit = gate_decision(
            &[transfer_log(SENDER, VICTIM)],
            vec![],
            &fee(SENDER, COINBASE, 21_000, 10, 7),
            &snap(&[VICTIM]),
        )
        .expect("hit");
        assert_eq!(hit.category, HitCategory::Log);
    }

    #[test]
    fn listed_coinbase_fee_only_not_hit() {
        // A listed coinbase collecting only the block priority fee must NOT be flagged.
        let (gas_used, egp, base) = (100u64, 10u128, 7u64);
        let reward = U256::from(gas_used) * U256::from(egp - base as u128); // 100 × 3 = 300
        let changes = vec![ListedBalanceChange {
            address: VICTIM, // == coinbase below
            balance_start: U256::ZERO,
            balance_end: reward,
        }];
        assert!(gate_decision(
            &[],
            changes,
            &fee(SENDER, VICTIM, gas_used, egp, base),
            &snap(&[VICTIM])
        )
        .is_none());
    }

    #[test]
    fn listed_sender_gas_only_not_hit() {
        // A listed sender paying only gas (no transfer) must NOT be flagged.
        let (gas_used, egp) = (100u64, 10u128);
        let cost = U256::from(gas_used) * U256::from(egp); // 1000
        let changes = vec![ListedBalanceChange {
            address: VICTIM, // == sender below
            balance_start: U256::from(1000u64),
            balance_end: U256::from(1000u64) - cost, // 0
        }];
        assert!(gate_decision(
            &[],
            changes,
            &fee(VICTIM, COINBASE, gas_used, egp, 7),
            &snap(&[VICTIM])
        )
        .is_none());
    }

    #[test]
    fn clean_tx_not_hit() {
        assert!(gate_decision(
            &[],
            vec![],
            &fee(SENDER, COINBASE, 21_000, 10, 7),
            &snap(&[VICTIM])
        )
        .is_none());
    }
}
