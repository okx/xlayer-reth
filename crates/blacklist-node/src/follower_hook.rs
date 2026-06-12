//! FR-3/FR-5 — follower-face deposit interception hook (decision B).
//!
//! Implements the upstream [`DepositBlacklistHook`] (alloy-op-evm) so the follower /
//! newPayload-validation executor reverts a blacklisted deposit identically to the builder
//! face (Step 3). Decision uses committed effects only — check②(Transfer-class logs) +
//! check③(native-ETH balance change) — and **not** check①(committed CALL touch), because the
//! follower EVM cannot mount an inspector (cross-client decision B; see IMPL). The actual
//! included-as-reverted state/receipt surgery is applied by the upstream executor.

use crate::runtime::BlacklistRuntimeCtx;
use alloy_op_evm::block::DepositBlacklistHook;
use alloy_primitives::{Address, Bytes, Log, I256, U256};
use xlayer_blacklist::deposit::is_exempt_sender;
use xlayer_blacklist::{
    read_snapshot_at, BalanceCandidate, BlacklistEvaluator, BlacklistInspector, BlacklistSnapshot,
    MirrorViewCaller, ViewCallError,
};

/// Adapts the executor-provided `(to, input, gas) -> Option<output>` closure to the core
/// [`MirrorViewCaller`] seam so the shared `getBlacklist` pagination can drive it.
struct ClosureViewCaller<'a> {
    static_call: &'a mut dyn FnMut(Address, Bytes, u64) -> Option<Bytes>,
}

impl MirrorViewCaller for ClosureViewCaller<'_> {
    fn static_call(&mut self, to: Address, input: Bytes, gas: u64) -> Result<Bytes, ViewCallError> {
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
    fn should_intercept_deposit(
        &self,
        sender: Address,
        logs: &[Log],
        balance_changes: &[(Address, U256, U256)],
    ) -> bool {
        // Disabled chain / exempt sender (system, L1-attributes) / empty list → never gate.
        if !self.ctx.is_enabled() || is_exempt_sender(&sender) {
            return false;
        }
        let snapshot = self.ctx.load_snapshot();
        if snapshot.is_empty() {
            return false;
        }

        // check③ candidates: listed addresses this deposit changed. Deposits pay no L2
        // execution gas fee, so `fee_delta = 0` (no sender/coinbase fee to strip) and the net
        // is the raw committed balance movement.
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

        // No call-frame observations on the follower (decision B) → check① never fires; the
        // evaluator runs check②(logs) + check③(balance) only.
        match BlacklistEvaluator::evaluate(&inspector, logs, &snapshot) {
            Some(hit) => {
                self.ctx.record_exec_revert(&hit);
                true
            }
            None => false,
        }
    }

    fn refresh_snapshot(&self, static_call: &mut dyn FnMut(Address, Bytes, u64) -> Option<Bytes>) {
        // Disabled chain / unresolved mirror → store empty (no-op).
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
    use alloy_primitives::address;
    use xlayer_blacklist::BlacklistSnapshot;

    const AAA: Address = address!("00000000000000000000000000000000000000aa");
    const SYS: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

    fn ctx_with(addrs: &[Address]) -> BlacklistRuntimeCtx {
        let ctx = BlacklistRuntimeCtx::new(195); // devnet → enabled
        let snap: BlacklistSnapshot = addrs.iter().copied().collect();
        ctx.store_snapshot(snap);
        ctx
    }

    #[test]
    fn deposit_balance_inflow_to_listed_hits() {
        let hook = XLayerDepositBlacklistHook::new(ctx_with(&[AAA]));
        // 0xAAA balance 0 → 100 (deposit moved ETH in).
        assert!(hook.should_intercept_deposit(AAA, &[], &[(AAA, U256::ZERO, U256::from(100u64))]));
    }

    #[test]
    fn deposit_no_listed_change_does_not_hit() {
        let hook = XLayerDepositBlacklistHook::new(ctx_with(&[AAA]));
        let other = address!("00000000000000000000000000000000000000bb");
        assert!(!hook.should_intercept_deposit(
            other,
            &[],
            &[(other, U256::ZERO, U256::from(100u64))]
        ));
    }

    #[test]
    fn exempt_sender_never_gated() {
        let hook = XLayerDepositBlacklistHook::new(ctx_with(&[AAA]));
        // Even though 0xAAA is listed and receives ETH, a system-address deposit is exempt.
        assert!(!hook.should_intercept_deposit(SYS, &[], &[(AAA, U256::ZERO, U256::from(100u64))]));
    }

    #[test]
    fn disabled_chain_never_gated() {
        let ctx = BlacklistRuntimeCtx::new(10); // unrecognized → disabled
        let hook = XLayerDepositBlacklistHook::new(ctx);
        assert!(!hook.should_intercept_deposit(AAA, &[], &[(AAA, U256::ZERO, U256::from(100u64))]));
    }
}
