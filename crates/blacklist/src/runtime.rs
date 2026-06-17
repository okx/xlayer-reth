//! Node/revm/reth adapter layer for the XLayer chain-level blacklist (XLOP-1100).
//!
//! Connects the pure cross-client core ([`crate::rules`]) to op-reth's runtime: the shared
//! runtime context (chain-id dispatch + snapshot handle + metrics, FR-6/7), the block-head
//! snapshot read (FR-4), the ETH-balance reconstruction + deposit state surgery (FR-2/3),
//! the follower-face deposit hook (FR-3/5), and the executor builder that installs it. The
//! `BlacklistRuntimeCtx` lives at the top level; the reth-coupled adapters are inner modules.

use crate::rules::eval::Hit;
use crate::rules::metrics::BlacklistMetrics;
use crate::rules::mirror::mirror_address_for_chain;
use crate::rules::snapshot::BlacklistSnapshot;
use alloy_primitives::Address;
use arc_swap::ArcSwap;
use std::sync::Arc;

/// Shared, atomically-replaceable handle to the current block's blacklist snapshot.
pub type SnapshotHandle = Arc<ArcSwap<BlacklistSnapshot>>;

/// Per-node runtime context for the blacklist feature. Cloneable; the snapshot handle and
/// metrics are shared internally.
#[derive(Clone)]
pub struct BlacklistRuntimeCtx {
    chain_id: u64,
    mirror: Option<Address>,
    snapshot: SnapshotHandle,
    metrics: BlacklistMetrics,
}

impl core::fmt::Debug for BlacklistRuntimeCtx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BlacklistRuntimeCtx")
            .field("chain_id", &self.chain_id)
            .field("mirror", &self.mirror)
            .field("snapshot", &self.snapshot)
            .finish_non_exhaustive()
    }
}

impl BlacklistRuntimeCtx {
    /// Build a runtime context for the given chain id. Enabled iff the chain id resolves to a
    /// mirror address (FR-6); otherwise every read is a no-op.
    pub fn new(chain_id: u64) -> Self {
        Self {
            chain_id,
            mirror: mirror_address_for_chain(chain_id),
            snapshot: Arc::new(ArcSwap::from_pointee(BlacklistSnapshot::empty())),
            metrics: BlacklistMetrics::default(),
        }
    }

    /// Whether the feature is active on this chain (mirror address resolved). [FR-6]
    pub fn is_enabled(&self) -> bool {
        self.mirror.is_some()
    }

    /// The running chain id.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// The resolved mirror contract address, if any.
    pub fn mirror(&self) -> Option<Address> {
        self.mirror
    }

    /// A clone of the shared snapshot handle.
    pub fn snapshot_handle(&self) -> SnapshotHandle {
        Arc::clone(&self.snapshot)
    }

    /// Load the current snapshot (cheap atomic load).
    pub fn load_snapshot(&self) -> Arc<BlacklistSnapshot> {
        self.snapshot.load_full()
    }

    /// Atomically replace the snapshot (once per block / on reorg) and update the gauge.
    pub fn store_snapshot(&self, snapshot: BlacklistSnapshot) {
        self.metrics.cache_size.set(snapshot.len() as f64);
        self.snapshot.store(Arc::new(snapshot));
    }

    /// `xlayer_blacklist_exec_revert_total{hook=<category>} += 1` (exec-gate hit). [DM-7.2/7.4]
    pub fn record_exec_revert(&self, hit: &Hit) {
        BlacklistMetrics::increment_exec_revert(hit.category);
    }

    /// Observe a block-head snapshot read duration into the histogram. [DM-7.3]
    pub fn record_snapshot_read(&self, seconds: f64) {
        self.metrics.snapshot_read_duration_seconds.record(seconds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn devnet_is_enabled_unrecognized_is_not() {
        assert!(BlacklistRuntimeCtx::new(195).is_enabled());
        assert!(!BlacklistRuntimeCtx::new(10).is_enabled());
    }

    #[test]
    fn snapshot_store_then_load_roundtrips() {
        let ctx = BlacklistRuntimeCtx::new(195);
        assert!(ctx.load_snapshot().is_empty());
        let snap: BlacklistSnapshot =
            [address!("00000000000000000000000000000000000000aa")].into_iter().collect();
        ctx.store_snapshot(snap);
        assert_eq!(ctx.load_snapshot().len(), 1);
    }
}

pub mod balance {
    //! FR-2 check③ — ETH-balance candidate reconstruction (no-fork, decision B).

    use crate::rules::inspector::BalanceCandidate;
    use alloy_primitives::{Address, I256, U256};

    /// Deterministic fee context for one transaction.
    #[derive(Debug, Clone, Copy)]
    pub struct FeeContext {
        /// Top-level transaction sender (pays `gas_used × effective_gas_price`).
        pub sender: Address,
        /// Block coinbase (receives `gas_used × priority_fee_per_gas`).
        pub coinbase: Address,
        /// Gas actually used by the transaction (post-refund).
        pub gas_used: u64,
        /// Effective gas price paid per gas unit.
        pub effective_gas_price: u128,
        /// Block base fee per gas.
        pub base_fee: u64,
    }

    /// One observed balance change of a listed address.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ListedBalanceChange {
        /// The listed address whose balance changed.
        pub address: Address,
        /// Committed balance before this transaction.
        pub balance_start: U256,
        /// Committed balance after this transaction (pre-revert / pre-commit).
        pub balance_end: U256,
    }

    /// Build the [`BalanceCandidate`] set for check③ from observed listed balance changes.
    pub fn reconstruct_balance_candidates(
        changes: impl IntoIterator<Item = ListedBalanceChange>,
        fee: &FeeContext,
        selfdestruct_targets: &[Address],
    ) -> Vec<BalanceCandidate> {
        changes
            .into_iter()
            .map(|c| BalanceCandidate {
                address: c.address,
                balance_start: c.balance_start,
                balance_end: c.balance_end,
                fee_delta: fee_delta_for(c.address, fee),
                selfdestruct: selfdestruct_targets.contains(&c.address),
            })
            .collect()
    }

    /// Signed fee-only delta for `addr` (op-geth reason {5,6,7} equivalent).
    fn fee_delta_for(addr: Address, fee: &FeeContext) -> I256 {
        let gas_used = U256::from(fee.gas_used);
        let mut delta = I256::ZERO;

        if addr == fee.sender {
            let cost = gas_used.saturating_mul(U256::from(fee.effective_gas_price));
            if let Ok(cost) = I256::try_from(cost) {
                delta = delta.saturating_sub(cost);
            }
        }
        if addr == fee.coinbase {
            let priority = fee.effective_gas_price.saturating_sub(fee.base_fee as u128);
            let reward = gas_used.saturating_mul(U256::from(priority));
            if let Ok(reward) = I256::try_from(reward) {
                delta = delta.saturating_add(reward);
            }
        }
        delta
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::rules::eval::{BlacklistEvaluator, HitCategory};
        use crate::rules::inspector::BlacklistInspector;
        use crate::rules::snapshot::BlacklistSnapshot;
        use alloy_primitives::address;

        const AAA: Address = address!("00000000000000000000000000000000000000aa");
        const COINBASE: Address = address!("00000000000000000000000000000000000000cb");
        const OTHER: Address = address!("00000000000000000000000000000000000000ff");

        fn fee(
            sender: Address,
            coinbase: Address,
            gas_used: u64,
            price: u128,
            base_fee: u64,
        ) -> FeeContext {
            FeeContext { sender, coinbase, gas_used, effective_gas_price: price, base_fee }
        }

        fn hits(cand: &BalanceCandidate) -> bool {
            let mut insp = BlacklistInspector::new();
            insp.record_balance_candidate(cand.clone());
            let snap: BlacklistSnapshot = [cand.address].into_iter().collect();
            BlacklistEvaluator::evaluate(&insp, &[], &snap).is_some()
        }

        #[test]
        fn reconstruct_sender_fee_stripped() {
            let changes = [ListedBalanceChange {
                address: AAA,
                balance_start: U256::from(100u64),
                balance_end: U256::from(79u64),
            }];
            let c = reconstruct_balance_candidates(changes, &fee(AAA, COINBASE, 21, 1, 0), &[]);
            assert_eq!(c[0].fee_delta, I256::try_from(-21i64).unwrap());
            assert!(!hits(&c[0]));
        }

        #[test]
        fn reconstruct_coinbase_reward_stripped() {
            let changes = [ListedBalanceChange {
                address: AAA,
                balance_start: U256::from(100u64),
                balance_end: U256::from(120u64),
            }];
            let c = reconstruct_balance_candidates(changes, &fee(OTHER, AAA, 10, 3, 1), &[]);
            assert_eq!(c[0].fee_delta, I256::try_from(20i64).unwrap());
            assert!(!hits(&c[0]));
        }

        #[test]
        fn reconstruct_value_transfer_hits() {
            let changes = [ListedBalanceChange {
                address: AAA,
                balance_start: U256::ZERO,
                balance_end: U256::from(500u64),
            }];
            let c =
                reconstruct_balance_candidates(changes, &fee(OTHER, COINBASE, 21000, 1, 0), &[]);
            assert_eq!(c[0].fee_delta, I256::ZERO);
            assert!(!c[0].selfdestruct);
            assert!(hits(&c[0]));
        }

        #[test]
        fn reconstruct_l1fee_not_stripped() {
            let changes = [ListedBalanceChange {
                address: AAA,
                balance_start: U256::from(1000u64),
                balance_end: U256::from(900u64),
            }];
            let c = reconstruct_balance_candidates(changes, &fee(AAA, COINBASE, 21, 1, 0), &[]);
            assert_eq!(c[0].fee_delta, I256::try_from(-21i64).unwrap());
            assert!(hits(&c[0]));
        }

        #[test]
        fn reconstruct_selfdestruct_flag() {
            let changes = [ListedBalanceChange {
                address: AAA,
                balance_start: U256::ZERO,
                balance_end: U256::from(7u64),
            }];
            let c = reconstruct_balance_candidates(changes, &fee(OTHER, COINBASE, 0, 0, 0), &[AAA]);
            assert!(c[0].selfdestruct);
            let mut insp = BlacklistInspector::new();
            insp.record_balance_candidate(c[0].clone());
            let snap: BlacklistSnapshot = [AAA].into_iter().collect();
            let hit = BlacklistEvaluator::evaluate(&insp, &[], &snap).expect("hit");
            assert_eq!(hit.category, HitCategory::SelfDestruct);
        }

        #[test]
        fn reconstruct_empty_changes_yields_no_candidates() {
            let c = reconstruct_balance_candidates([], &fee(AAA, COINBASE, 21000, 1, 0), &[]);
            assert!(c.is_empty());
        }

        #[test]
        fn reconstruct_sender_plus_value_still_hits() {
            let changes = [ListedBalanceChange {
                address: AAA,
                balance_start: U256::from(100u64),
                balance_end: U256::from(579u64),
            }];
            let c = reconstruct_balance_candidates(changes, &fee(AAA, COINBASE, 21, 1, 0), &[]);
            assert_eq!(c[0].fee_delta, I256::try_from(-21i64).unwrap());
            assert!(hits(&c[0]));
        }

        #[test]
        fn reconstruct_coinbase_plus_value_still_hits() {
            let changes = [ListedBalanceChange {
                address: AAA,
                balance_start: U256::ZERO,
                balance_end: U256::from(120u64),
            }];
            let c = reconstruct_balance_candidates(changes, &fee(OTHER, AAA, 10, 3, 1), &[]);
            assert_eq!(c[0].fee_delta, I256::try_from(20i64).unwrap());
            assert!(hits(&c[0]));
        }

        #[test]
        fn reconstruct_sender_is_also_coinbase_sums_both() {
            let changes = [ListedBalanceChange {
                address: AAA,
                balance_start: U256::from(100u64),
                balance_end: U256::from(90u64),
            }];
            let c = reconstruct_balance_candidates(changes, &fee(AAA, AAA, 10, 3, 1), &[]);
            assert_eq!(c[0].fee_delta, I256::try_from(-10i64).unwrap());
            assert!(!hits(&c[0]));
        }
    }
}

pub mod deposit_apply {
    //! FR-3 — included-as-reverted state surgery for an intercepted deposit (sender delta).

    use crate::rules::deposit::RevertedDepositOutcome;
    use alloy_primitives::Address;
    use revm::state::{Account, AccountInfo, EvmState};

    /// Build the single-account committed state delta for an intercepted deposit: the
    /// sender's committed pre-tx account info with `nonce = account_nonce` and `balance +=
    /// mint`. All other touched accounts revert by being absent from the delta.
    pub fn reverted_deposit_state(
        sender: Address,
        pre_info: AccountInfo,
        outcome: &RevertedDepositOutcome,
    ) -> EvmState {
        let mut info = pre_info;
        info.nonce = outcome.account_nonce;
        if let Some(mint) = outcome.keep_mint {
            info.balance = info.balance.saturating_add(mint);
        }
        let mut account = Account { info, ..Default::default() };
        account.mark_touch();

        let mut state = EvmState::default();
        state.insert(sender, account);
        state
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::rules::deposit::apply_included_as_reverted;
        use alloy_primitives::{address, U256};

        const SENDER: Address = address!("00000000000000000000000000000000000000aa");

        #[test]
        fn keeps_mint_and_bumps_nonce() {
            let pre = AccountInfo { balance: U256::from(100u64), nonce: 7, ..Default::default() };
            let outcome = apply_included_as_reverted(7, 21_000, Some(U256::from(50u64)), true);
            let state = reverted_deposit_state(SENDER, pre, &outcome);
            let acct = state.get(&SENDER).expect("sender present");
            assert_eq!(acct.info.nonce, 8);
            assert_eq!(acct.info.balance, U256::from(150u64));
            assert!(acct.is_touched());
        }

        #[test]
        fn no_mint_only_bumps_nonce() {
            let pre = AccountInfo { balance: U256::from(100u64), nonce: 0, ..Default::default() };
            let outcome = apply_included_as_reverted(0, 50_000, None, false);
            let state = reverted_deposit_state(SENDER, pre, &outcome);
            let acct = state.get(&SENDER).expect("sender present");
            assert_eq!(acct.info.nonce, 1);
            assert_eq!(acct.info.balance, U256::from(100u64));
        }

        #[test]
        fn preserves_code_hash() {
            let code_hash = alloy_primitives::b256!(
                "1111111111111111111111111111111111111111111111111111111111111111"
            );
            let pre =
                AccountInfo { balance: U256::ZERO, nonce: 3, code_hash, ..Default::default() };
            let outcome = apply_included_as_reverted(3, 100, Some(U256::from(9u64)), true);
            let state = reverted_deposit_state(SENDER, pre, &outcome);
            assert_eq!(state.get(&SENDER).unwrap().info.code_hash, code_hash);
        }
    }
}

pub mod view {
    //! FR-4 — live `MirrorViewCaller` adapter: read-only 50M-gas call to the mirror.

    use crate::rules::snapshot::{
        read_snapshot_at, BlacklistSnapshot, MirrorViewCaller, ViewCallError, SYSTEM_ADDRESS,
    };
    use alloy_evm::{rpc::TryIntoTxEnv, Database, Evm};
    use alloy_op_evm::OpEvm;
    use alloy_primitives::{Address, Bytes};
    use alloy_rpc_types_eth::TransactionInput;
    use op_alloy_rpc_types::OpTransactionRequest;
    use reth_evm::precompiles::PrecompilesMap;
    use reth_optimism_evm::OpTx;
    use revm::{
        context::result::{ExecutionResult, ResultAndState},
        inspector::NoOpInspector,
    };

    /// A [`MirrorViewCaller`] backed by an `OpEvm` built over the parent state.
    pub struct RethMirrorViewCaller<'e, DB: Database + core::fmt::Debug> {
        evm: &'e mut OpEvm<DB, NoOpInspector, PrecompilesMap, OpTx>,
    }

    impl<'e, DB> RethMirrorViewCaller<'e, DB>
    where
        DB: Database + core::fmt::Debug,
    {
        /// Wrap a mutable `OpEvm` reference, disabling the checks a read-only system-style call
        /// must not be subject to. The caller is responsible for the correct parent state /
        /// merge-rule `prevrandao`.
        pub fn new(evm: &'e mut OpEvm<DB, NoOpInspector, PrecompilesMap, OpTx>) -> Self {
            evm.modify_cfg(|cfg| {
                cfg.disable_balance_check = true;
                cfg.disable_block_gas_limit = true;
                cfg.disable_nonce_check = true;
                cfg.disable_base_fee = true;
            });
            Self { evm }
        }
    }

    impl<DB> MirrorViewCaller for RethMirrorViewCaller<'_, DB>
    where
        DB: Database + core::fmt::Debug,
    {
        fn static_call(
            &mut self,
            to: Address,
            input: Bytes,
            gas: u64,
        ) -> Result<Bytes, ViewCallError> {
            let tx_req = OpTransactionRequest::default()
                .gas_limit(gas)
                .max_fee_per_gas(0)
                .to(to)
                .from(SYSTEM_ADDRESS)
                .input(TransactionInput::new(input));

            let evm_env = alloy_evm::EvmEnv::from((self.evm.cfg.clone(), self.evm.block.clone()));
            let base = tx_req
                .as_ref()
                .clone()
                .try_into_tx_env(&evm_env)
                .map_err(|_| ViewCallError::CallFailed)?;
            let tx_env = OpTx(op_revm::OpTransaction {
                base,
                enveloped_tx: Some(Bytes::new()),
                deposit: Default::default(),
            });

            match self.evm.transact(tx_env) {
                Ok(ResultAndState { result: ExecutionResult::Success { output, .. }, .. }) => {
                    Ok(output.into_data())
                }
                _ => Err(ViewCallError::CallFailed),
            }
        }
    }

    /// Read the full block-head blacklist snapshot from `mirror` via the given read-only
    /// `OpEvm` (built over the parent state), using the cross-client pagination.
    pub fn read_blacklist_snapshot<DB: Database + core::fmt::Debug>(
        evm: &mut OpEvm<DB, NoOpInspector, PrecompilesMap, OpTx>,
        mirror: Address,
    ) -> BlacklistSnapshot {
        let mut caller = RethMirrorViewCaller::new(evm);
        read_snapshot_at(&mut caller, mirror)
    }
}

pub mod follower_hook {
    //! FR-3/FR-5 — follower-face deposit interception hook (decision B).
    //!
    //! Implements the upstream [`DepositBlacklistHook`] (alloy-op-evm). Decision uses
    //! committed effects only — check②(Transfer logs) + check③(ETH balance) — not check①.
    //! The included-as-reverted field plan is computed via the shared
    //! [`apply_included_as_reverted`] (single source of truth with the builder face); the
    //! upstream executor only applies it.

    use super::BlacklistRuntimeCtx;
    use crate::rules::deposit::{apply_included_as_reverted, is_exempt_sender};
    use crate::rules::eval::BlacklistEvaluator;
    use crate::rules::inspector::{BalanceCandidate, BlacklistInspector};
    use crate::rules::snapshot::{
        read_snapshot_at, BlacklistSnapshot, MirrorViewCaller, ViewCallError,
    };
    use alloy_op_evm::block::{DepositBlacklistHook, DepositRevertData};
    use alloy_primitives::{Address, Bytes, Log, I256, U256};

    /// Adapts the executor-provided `(to, input, gas) -> Option<output>` closure to the core
    /// [`MirrorViewCaller`] seam so the shared `getBlacklist` pagination can drive it.
    struct ClosureViewCaller<'a> {
        static_call: &'a mut dyn FnMut(Address, Bytes, u64) -> Option<Bytes>,
    }

    impl MirrorViewCaller for ClosureViewCaller<'_> {
        fn static_call(
            &mut self,
            to: Address,
            input: Bytes,
            gas: u64,
        ) -> Result<Bytes, ViewCallError> {
            (self.static_call)(to, input, gas).ok_or(ViewCallError::CallFailed)
        }
    }

    /// Deposit blacklist decision hook backed by the shared [`BlacklistRuntimeCtx`] snapshot.
    #[derive(Debug, Clone)]
    pub struct XLayerDepositBlacklistHook {
        ctx: BlacklistRuntimeCtx,
    }

    impl XLayerDepositBlacklistHook {
        /// Build the hook from the shared runtime context.
        pub fn new(ctx: BlacklistRuntimeCtx) -> Self {
            Self { ctx }
        }
    }

    impl DepositBlacklistHook for XLayerDepositBlacklistHook {
        fn decide_deposit(
            &self,
            sender: Address,
            logs: &[Log],
            balance_changes: &[(Address, U256, U256)],
            pre_nonce: u64,
            gas_limit: u64,
            mint: u128,
            canyon_active: bool,
        ) -> Option<DepositRevertData> {
            if !self.ctx.is_enabled() || is_exempt_sender(&sender) {
                return None;
            }
            let snapshot = self.ctx.load_snapshot();
            if snapshot.is_empty() {
                return None;
            }

            let mut inspector = BlacklistInspector::new();
            for (addr, before, after) in balance_changes {
                if snapshot.contains(addr) {
                    inspector.record_balance_candidate(BalanceCandidate {
                        address: *addr,
                        balance_start: *before,
                        balance_end: *after,
                        fee_delta: I256::ZERO,
                        selfdestruct: false,
                    });
                }
            }

            let hit = BlacklistEvaluator::evaluate(&inspector, logs, &snapshot)?;
            self.ctx.record_exec_revert(&hit);

            let outcome = apply_included_as_reverted(
                pre_nonce,
                gas_limit,
                (mint != 0).then(|| U256::from(mint)),
                canyon_active,
            );
            Some(DepositRevertData {
                status: outcome.status,
                gas_used: outcome.gas_used,
                deposit_nonce: outcome.deposit_nonce,
                account_nonce: outcome.account_nonce,
                keep_mint: mint,
                deposit_receipt_version: outcome.deposit_receipt_version,
            })
        }

        fn refresh_snapshot(
            &self,
            static_call: &mut dyn FnMut(Address, Bytes, u64) -> Option<Bytes>,
        ) {
            let Some(mirror) = self.ctx.mirror() else {
                self.ctx.store_snapshot(BlacklistSnapshot::empty());
                return;
            };
            let mut caller = ClosureViewCaller { static_call };
            let snapshot = read_snapshot_at(&mut caller, mirror);
            self.ctx.store_snapshot(snapshot);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::rules::snapshot::BlacklistSnapshot;
        use alloy_primitives::address;

        const AAA: Address = address!("00000000000000000000000000000000000000aa");
        const SYS: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

        fn ctx_with(addrs: &[Address]) -> BlacklistRuntimeCtx {
            let ctx = BlacklistRuntimeCtx::new(195);
            let snap: BlacklistSnapshot = addrs.iter().copied().collect();
            ctx.store_snapshot(snap);
            ctx
        }

        #[test]
        fn deposit_balance_inflow_to_listed_returns_plan() {
            let hook = XLayerDepositBlacklistHook::new(ctx_with(&[AAA]));
            let plan = hook
                .decide_deposit(
                    AAA,
                    &[],
                    &[(AAA, U256::ZERO, U256::from(100u64))],
                    7,
                    21_000,
                    0,
                    true,
                )
                .expect("plan");
            assert!(!plan.status);
            assert_eq!(plan.gas_used, 21_000);
            assert_eq!(plan.deposit_nonce, 7);
            assert_eq!(plan.account_nonce, 8);
            assert_eq!(plan.deposit_receipt_version, Some(1));
        }

        #[test]
        fn deposit_no_listed_change_returns_none() {
            let hook = XLayerDepositBlacklistHook::new(ctx_with(&[AAA]));
            let other = address!("00000000000000000000000000000000000000bb");
            assert!(hook
                .decide_deposit(
                    other,
                    &[],
                    &[(other, U256::ZERO, U256::from(100u64))],
                    0,
                    21_000,
                    0,
                    true
                )
                .is_none());
        }

        #[test]
        fn exempt_sender_never_gated() {
            let hook = XLayerDepositBlacklistHook::new(ctx_with(&[AAA]));
            assert!(hook
                .decide_deposit(
                    SYS,
                    &[],
                    &[(AAA, U256::ZERO, U256::from(100u64))],
                    0,
                    21_000,
                    0,
                    true
                )
                .is_none());
        }

        #[test]
        fn disabled_chain_never_gated() {
            let ctx = BlacklistRuntimeCtx::new(10);
            let hook = XLayerDepositBlacklistHook::new(ctx);
            assert!(hook
                .decide_deposit(
                    AAA,
                    &[],
                    &[(AAA, U256::ZERO, U256::from(100u64))],
                    0,
                    21_000,
                    0,
                    true
                )
                .is_none());
        }
    }
}

pub mod executor_builder {
    //! FR-3/FR-5 (follower face) — node-builder `ExecutorBuilder` that installs the blacklist
    //! deposit hook WITHOUT changing the EVM-config type (same `OpEvmConfig` as upstream).

    use super::follower_hook::XLayerDepositBlacklistHook;
    use super::BlacklistRuntimeCtx;
    use alloy_op_evm::block::DepositBlacklistHook;
    use reth_node_api::NodeTypes;
    use reth_node_builder::{components::ExecutorBuilder, BuilderContext, FullNodeTypes};
    use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
    use reth_optimism_forks::OpHardforks;
    use reth_optimism_primitives::OpPrimitives;
    use std::sync::Arc;

    /// Node-builder executor component that attaches the blacklist deposit hook to the
    /// follower / newPayload-validation EVM config, keeping the upstream `OpEvmConfig` type.
    #[derive(Debug, Clone)]
    pub struct XLayerExecutorBuilder {
        ctx: BlacklistRuntimeCtx,
    }

    impl XLayerExecutorBuilder {
        /// New builder carrying the shared blacklist runtime context.
        pub fn new(ctx: BlacklistRuntimeCtx) -> Self {
            Self { ctx }
        }
    }

    impl<Node> ExecutorBuilder<Node> for XLayerExecutorBuilder
    where
        Node: FullNodeTypes<Types: NodeTypes<ChainSpec: OpHardforks, Primitives = OpPrimitives>>,
    {
        type EVM = OpEvmConfig<
            <Node::Types as NodeTypes>::ChainSpec,
            <Node::Types as NodeTypes>::Primitives,
        >;

        async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
            let mut evm_config =
                OpEvmConfig::new(ctx.chain_spec(), OpRethReceiptBuilder::default());
            let hook: Arc<dyn DepositBlacklistHook> =
                Arc::new(XLayerDepositBlacklistHook::new(self.ctx));
            evm_config.executor_factory =
                evm_config.executor_factory.with_blacklist_hook(Some(hook));
            Ok(evm_config)
        }
    }
}
