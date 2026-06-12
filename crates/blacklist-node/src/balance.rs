//! FR-2 check③ — ETH-balance candidate reconstruction (no-fork, decision B).
//!
//! revm's `Inspector` has no balance-change hook (unlike op-geth's `OnBalanceChange`).
//! The hit decision `(balEnd - balStart - feeDelta) != 0` is instead reconstructed from
//! the post-execution state diff + the deterministic gas accounting:
//!   - candidate set = addresses changed by the tx ∩ blacklist (intersection done by the
//!     caller, which reads `balStart` from the pre-commit DB and `balEnd` from the state
//!     diff `Account.info.balance`);
//!   - `feeDelta` strips exactly op-geth's reason set {RewardTransactionFee, GasBuy,
//!     GasReturn} and nothing else (L1 data fee / operator fee / base-fee burn are NOT
//!     stripped — they are not in op-geth's set, and their vaults are never listed):
//!       * sender   : `-(gas_used × effective_gas_price)`   (GasBuy + GasReturn net)
//!       * coinbase : `+(gas_used × (effective_gas_price − base_fee))` (priority reward)
//!
//! This is a pure function over plain values so it is fully unit-testable without revm.
//! The "公式重建 == op-geth reason 集" equivalence is consensus-critical and must be
//! validated against the shared adversarial vectors before enablement (IMPL Step 2 gate).

use alloy_primitives::{Address, I256, U256};
use xlayer_blacklist::BalanceCandidate;

/// Deterministic fee context for one transaction, used to strip the fee-only balance delta
/// of a listed address that happens to be the tx sender and/or the block coinbase.
#[derive(Debug, Clone, Copy)]
pub struct FeeContext {
    /// Top-level transaction sender (pays `gas_used × effective_gas_price`).
    pub sender: Address,
    /// Block coinbase (receives `gas_used × priority_fee_per_gas`).
    pub coinbase: Address,
    /// Gas actually used by the transaction (post-refund), as reported by the result.
    pub gas_used: u64,
    /// Effective gas price paid per gas unit.
    pub effective_gas_price: u128,
    /// Block base fee per gas.
    pub base_fee: u64,
}

/// One observed balance change of a listed address: its committed pre-tx and post-tx
/// (pre-commit) native-ETH balances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListedBalanceChange {
    /// The listed address whose balance changed.
    pub address: Address,
    /// Committed balance before this transaction.
    pub balance_start: U256,
    /// Committed balance after this transaction (pre-revert / pre-commit).
    pub balance_end: U256,
}

/// Build the [`BalanceCandidate`] set for check③ from the observed listed balance changes,
/// computing each candidate's fee-only `feeDelta` from the deterministic gas accounting and
/// tagging selfdestruct-beneficiary candidates for the metric category.
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

/// Signed fee-only delta for `addr` — the sum of the op-geth-reason-{5,6,7}-equivalent
/// components that apply to it (sender gas cost and/or coinbase priority reward). Realistic
/// fee magnitudes never overflow `I256`; an (impossible) overflow yields `0` so the fee is
/// not stripped — conservatively keeping the candidate eligible to hit rather than masking a
/// real transfer.
fn fee_delta_for(addr: Address, fee: &FeeContext) -> I256 {
    let gas_used = U256::from(fee.gas_used);
    let mut delta = I256::ZERO;

    if addr == fee.sender {
        // GasBuy(−) + GasReturn(+) net = −(gas_used × effective_gas_price).
        let cost = gas_used.saturating_mul(U256::from(fee.effective_gas_price));
        if let Ok(cost) = I256::try_from(cost) {
            delta = delta.saturating_sub(cost);
        }
    }
    if addr == fee.coinbase {
        // RewardTransactionFee(+) = gas_used × priority_fee_per_gas.
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
    use alloy_primitives::address;
    use xlayer_blacklist::{
        BlacklistEvaluator, BlacklistInspector, BlacklistSnapshot, HitCategory,
    };

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

    /// Drive the candidate through the real evaluator to check the hit decision.
    fn hits(cand: &BalanceCandidate) -> bool {
        let mut insp = BlacklistInspector::new();
        insp.record_balance_candidate(cand.clone());
        let snap: BlacklistSnapshot = [cand.address].into_iter().collect();
        BlacklistEvaluator::evaluate(&insp, &[], &snap).is_some()
    }

    #[test]
    fn reconstruct_sender_fee_stripped() {
        // addr is sender; net change == gas cost exactly → fee stripped → no hit. (gas 21 × price 1 = 21)
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
        // addr is coinbase; net == priority reward (gas 10 × (price 3 − base 1) = 20) → stripped → no hit.
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
        // pure value receipt, addr neither sender nor coinbase → fee_delta 0 → hit.
        let changes = [ListedBalanceChange {
            address: AAA,
            balance_start: U256::ZERO,
            balance_end: U256::from(500u64),
        }];
        let c = reconstruct_balance_candidates(changes, &fee(OTHER, COINBASE, 21000, 1, 0), &[]);
        assert_eq!(c[0].fee_delta, I256::ZERO);
        assert!(!c[0].selfdestruct);
        assert!(hits(&c[0]));
    }

    #[test]
    fn reconstruct_l1fee_not_stripped() {
        // addr is sender; balance dropped by MORE than the L2 gas cost (extra = L1/operator fee).
        // Only the L2 gas cost is stripped; the remainder keeps net ≠ 0 → hit.
        // start 1000, end 900 (Δ −100); gas 21×1 = 21 stripped → net = −100 − (−21) = −79 ≠ 0.
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
        // non-listed addresses are excluded by the caller's intersection → empty input here.
        let c = reconstruct_balance_candidates([], &fee(AAA, COINBASE, 21000, 1, 0), &[]);
        assert!(c.is_empty());
    }

    #[test]
    fn reconstruct_sender_plus_value_still_hits() {
        // addr is sender AND receives a real value transfer: net = value − gasCost; only gas stripped.
        // start 100, end 579 (Δ +479); gas 21×1 = 21 → fee_delta −21; net = 479 + 21 = 500 ≠ 0 → hit.
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
        // addr is coinbase AND receives non-reward value: only the reward is stripped.
        // start 0, end 120; reward = 10×(3−1)=20 → fee_delta +20; net = 120 − 20 = 100 ≠ 0 → hit.
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
        // degenerate: addr is both sender and coinbase → fee_delta = −cost + reward.
        // gas 10, price 3, base 1: cost = 30, reward = 20 → fee_delta = −10.
        let changes = [ListedBalanceChange {
            address: AAA,
            balance_start: U256::from(100u64),
            balance_end: U256::from(90u64),
        }];
        let c = reconstruct_balance_candidates(changes, &fee(AAA, AAA, 10, 3, 1), &[]);
        assert_eq!(c[0].fee_delta, I256::try_from(-10i64).unwrap());
        // net = (90−100) − (−10) = 0 → no hit (pure fee).
        assert!(!hits(&c[0]));
    }
}
