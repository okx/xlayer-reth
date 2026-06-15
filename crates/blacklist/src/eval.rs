//! FR-2 — three-check evaluator with fixed priority `call > log > balance`.
//!
//! Given the per-tx [`BlacklistInspector`] observations, the committed logs, and the
//! block-head [`BlacklistSnapshot`], decide whether the transaction touched a blacklisted
//! address in a *committed* way, and under which category. The judgement is made on
//! already-committed effects (revm journaling has already excluded reverted-frame logs
//! and balance diffs). [TD §4.3]

use crate::inspector::{BalanceCandidate, BlacklistInspector};
use crate::snapshot::BlacklistSnapshot;
use alloy_primitives::{keccak256, Address, Log, B256, I256};
use std::sync::LazyLock;

/// `keccak256("Transfer(address,address,uint256)")` (ERC20/721). topics[1]=from, topics[2]=to.
pub static TRANSFER_TOPIC0: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"Transfer(address,address,uint256)"));
/// `keccak256("TransferSingle(address,address,address,uint256,uint256)")` (ERC1155).
/// topics[2]=from, topics[3]=to.
pub static TRANSFER_SINGLE_TOPIC0: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"TransferSingle(address,address,address,uint256,uint256)"));
/// `keccak256("TransferBatch(address,address,address,uint256[],uint256[])")` (ERC1155).
/// topics[2]=from, topics[3]=to.
pub static TRANSFER_BATCH_TOPIC0: LazyLock<B256> =
    LazyLock::new(|| keccak256(b"TransferBatch(address,address,address,uint256[],uint256[])"));

/// The category of a blacklist hit. Maps 1:1 to the `hook` metric label. [TD §3.3, §4.7]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitCategory {
    /// Committed CALL-frame touch (highest priority).
    Call,
    /// Committed Transfer-class event.
    Log,
    /// Net committed ETH balance change beyond the fee-only set.
    EthBalance,
    /// Balance change attributable to a selfdestruct beneficiary.
    SelfDestruct,
}

impl HitCategory {
    /// Prometheus `hook=` label value.
    pub fn metric_label(self) -> &'static str {
        match self {
            HitCategory::Call => "call",
            HitCategory::Log => "log",
            HitCategory::EthBalance => "eth_balance",
            HitCategory::SelfDestruct => "selfdestruct",
        }
    }
}

/// A single blacklist hit: the category and the offending blacklisted address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Which check fired (resolved by priority when several would fire).
    pub category: HitCategory,
    /// The blacklisted address that was touched.
    pub address: Address,
}

/// The three-check evaluator. Stateless; operates purely on its inputs.
#[derive(Debug, Default, Clone, Copy)]
pub struct BlacklistEvaluator;

impl BlacklistEvaluator {
    /// Evaluate the transaction. Returns `Some(Hit)` on a committed touch of a
    /// blacklisted address, with the category chosen by the fixed priority
    /// **call > log > balance**; `None` otherwise (including an empty snapshot). [TD §4.3]
    pub fn evaluate(
        inspector: &BlacklistInspector,
        logs: &[Log],
        snapshot: &BlacklistSnapshot,
    ) -> Option<Hit> {
        if snapshot.is_empty() {
            return None;
        }

        // check① committed CALL-frame touch.
        for addr in inspector.committed_touches() {
            if snapshot.contains(&addr) {
                return Some(Hit { category: HitCategory::Call, address: addr });
            }
        }

        // check② committed Transfer-class event.
        for log in logs {
            if let Some(addr) = transfer_log_hit(log, snapshot) {
                return Some(Hit { category: HitCategory::Log, address: addr });
            }
        }

        // check③ committed net ETH balance change beyond the fee-only set.
        for cand in inspector.balance_candidates() {
            if snapshot.contains(&cand.address) && balance_hit(cand) {
                let category = if cand.selfdestruct {
                    HitCategory::SelfDestruct
                } else {
                    HitCategory::EthBalance
                };
                return Some(Hit { category, address: cand.address });
            }
        }

        None
    }
}

/// Extract a blacklisted from/to from a committed Transfer-class log, if any.
fn transfer_log_hit(log: &Log, snapshot: &BlacklistSnapshot) -> Option<Address> {
    let topics = log.topics();
    let topic0 = topics.first()?;

    // ERC20/721 Transfer: topics[1]=from, topics[2]=to, needs >= 3 topics.
    if *topic0 == *TRANSFER_TOPIC0 {
        if topics.len() < 3 {
            return None; // DM-2.18 defensive skip
        }
        return first_listed(snapshot, &[topics[1], topics[2]]);
    }

    // ERC1155 TransferSingle / TransferBatch: topics[2]=from, topics[3]=to, needs >= 4.
    if *topic0 == *TRANSFER_SINGLE_TOPIC0 || *topic0 == *TRANSFER_BATCH_TOPIC0 {
        if topics.len() < 4 {
            return None; // DM-2.18 defensive skip
        }
        return first_listed(snapshot, &[topics[2], topics[3]]);
    }

    None
}

/// Return the first of `topics` (interpreted as a right-aligned address) that is listed.
fn first_listed(snapshot: &BlacklistSnapshot, topics: &[B256]) -> Option<Address> {
    for t in topics {
        let addr = Address::from_word(*t);
        if snapshot.contains(&addr) {
            return Some(addr);
        }
    }
    None
}

/// `(balance_end - balance_start - fee_delta) != 0`, computed in signed 256-bit space.
/// Overflow (impossible for realistic balances) is treated conservatively as no-hit.
fn balance_hit(c: &BalanceCandidate) -> bool {
    let start = match I256::try_from(c.balance_start) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let end = match I256::try_from(c.balance_end) {
        Ok(v) => v,
        Err(_) => return false,
    };
    match end.checked_sub(start).and_then(|d| d.checked_sub(c.fee_delta)) {
        Some(net) => net != I256::ZERO,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::BlacklistInspector;
    use alloy_primitives::{address, Address, LogData, U256};

    const AAA: Address = address!("00000000000000000000000000000000000000aa");
    const BBB: Address = address!("00000000000000000000000000000000000000bb");
    const ROUTER: Address = address!("00000000000000000000000000000000000000cc");
    const TOKEN: Address = address!("00000000000000000000000000000000000000dd");

    fn snapshot_with(addrs: &[Address]) -> BlacklistSnapshot {
        addrs.iter().copied().collect()
    }

    fn addr_topic(a: Address) -> B256 {
        a.into_word()
    }

    fn transfer_log(topic0: B256, topics_tail: &[Address]) -> Log {
        let mut topics = vec![topic0];
        topics.extend(topics_tail.iter().map(|a| addr_topic(*a)));
        Log { address: TOKEN, data: LogData::new(topics, Default::default()).expect("log data") }
    }

    #[test]
    fn empty_snapshot_never_hits() {
        // DM-2.19
        let mut insp = BlacklistInspector::new();
        insp.record_call_frame(BBB, AAA, None);
        let snap = BlacklistSnapshot::empty();
        assert_eq!(BlacklistEvaluator::evaluate(&insp, &[], &snap), None);
    }

    #[test]
    fn call_frame_touch_hits_with_call_category() {
        // DM-2.6
        let mut insp = BlacklistInspector::new();
        let root = insp.record_call_frame(BBB, ROUTER, None);
        insp.record_call_frame(ROUTER, AAA, Some(root));
        let snap = snapshot_with(&[AAA]);
        let hit = BlacklistEvaluator::evaluate(&insp, &[], &snap).expect("hit");
        assert_eq!(hit.category, HitCategory::Call);
        assert_eq!(hit.address, AAA);
    }

    #[test]
    fn deposit_call_touch_only_not_intercepted_under_decision_b() {
        // PRD A-7 / B-13 + Decision B: a deposit whose only effect is a CALL touch of a
        // blacklisted address (no Transfer event, no balance change) is NOT intercepted,
        // because deposits skip check① — the caller (follower hook / sequencer deposit
        // branch) feeds an EMPTY `BlacklistInspector` for deposits, so no call frame is ever
        // recorded. Modeled here as: snapshot DOES list the victim, but observations carry no
        // call frames / no balance candidates and there are no logs → `evaluate` returns None.
        // Contrast with `call_frame_touch_hits_with_call_category` (a normal L2 tx, which DOES
        // record the call frame and hits). A regression that recorded call frames for deposits
        // would flip this assertion to a hit, surfacing the divergence with op-geth.
        let insp = BlacklistInspector::new(); // empty inspector == Decision B for deposits
        let snap = snapshot_with(&[AAA]); // victim IS on the list
        assert_eq!(BlacklistEvaluator::evaluate(&insp, &[], &snap), None);
    }

    #[test]
    fn erc20_transfer_log_hits_on_to() {
        // DM-2.1
        let insp = BlacklistInspector::new();
        let log = transfer_log(*TRANSFER_TOPIC0, &[BBB, AAA]); // from=BBB, to=AAA
        let snap = snapshot_with(&[AAA]);
        let hit = BlacklistEvaluator::evaluate(&insp, &[log], &snap).expect("hit");
        assert_eq!(hit.category, HitCategory::Log);
        assert_eq!(hit.address, AAA);
    }

    #[test]
    fn erc1155_transfer_single_hits_on_topics_2_3() {
        // DM-2.2 — topics: [topic0, operator, from, to]
        let insp = BlacklistInspector::new();
        let log = transfer_log(*TRANSFER_SINGLE_TOPIC0, &[ROUTER, BBB, AAA]);
        let snap = snapshot_with(&[AAA]);
        let hit = BlacklistEvaluator::evaluate(&insp, &[log], &snap).expect("hit");
        assert_eq!(hit.category, HitCategory::Log);
    }

    #[test]
    fn transfer_topic0_with_index_miss_does_not_hit() {
        // DM-2.4
        let insp = BlacklistInspector::new();
        let log = transfer_log(*TRANSFER_TOPIC0, &[BBB, ROUTER]); // neither is AAA
        let snap = snapshot_with(&[AAA]);
        assert_eq!(BlacklistEvaluator::evaluate(&insp, &[log], &snap), None);
    }

    #[test]
    fn insufficient_topics_defensively_skipped() {
        // DM-2.18 — Transfer topic0 but only 2 topics.
        let insp = BlacklistInspector::new();
        let log = transfer_log(*TRANSFER_TOPIC0, &[AAA]); // topics.len() == 2 < 3
        let snap = snapshot_with(&[AAA]);
        assert_eq!(BlacklistEvaluator::evaluate(&insp, &[log], &snap), None);
    }

    #[test]
    fn eth_balance_change_beyond_fee_hits() {
        // DM-2.7
        let mut insp = BlacklistInspector::new();
        insp.record_balance_candidate(BalanceCandidate {
            address: AAA,
            balance_start: U256::from(100u64),
            balance_end: U256::from(150u64),
            fee_delta: I256::ZERO,
            selfdestruct: false,
        });
        let snap = snapshot_with(&[AAA]);
        let hit = BlacklistEvaluator::evaluate(&insp, &[], &snap).expect("hit");
        assert_eq!(hit.category, HitCategory::EthBalance);
    }

    #[test]
    fn fee_only_balance_change_does_not_hit() {
        // DM-2.9 — net change exactly equals fee_delta.
        let mut insp = BlacklistInspector::new();
        insp.record_balance_candidate(BalanceCandidate {
            address: AAA,
            balance_start: U256::from(100u64),
            balance_end: U256::from(79u64),
            fee_delta: I256::try_from(-21i64).unwrap(),
            selfdestruct: false,
        });
        let snap = snapshot_with(&[AAA]);
        assert_eq!(BlacklistEvaluator::evaluate(&insp, &[], &snap), None);
    }

    #[test]
    fn selfdestruct_candidate_categorized_as_selfdestruct() {
        // DM-2.8
        let mut insp = BlacklistInspector::new();
        insp.record_balance_candidate(BalanceCandidate {
            address: AAA,
            balance_start: U256::ZERO,
            balance_end: U256::from(500u64),
            fee_delta: I256::ZERO,
            selfdestruct: true,
        });
        let snap = snapshot_with(&[AAA]);
        let hit = BlacklistEvaluator::evaluate(&insp, &[], &snap).expect("hit");
        assert_eq!(hit.category, HitCategory::SelfDestruct);
    }

    #[test]
    fn priority_call_beats_log_and_balance() {
        // DM-2.11 — all three would fire; call wins.
        let mut insp = BlacklistInspector::new();
        insp.record_call_frame(BBB, AAA, None);
        insp.record_balance_candidate(BalanceCandidate {
            address: AAA,
            balance_start: U256::ZERO,
            balance_end: U256::from(1u64),
            fee_delta: I256::ZERO,
            selfdestruct: false,
        });
        let log = transfer_log(*TRANSFER_TOPIC0, &[BBB, AAA]);
        let snap = snapshot_with(&[AAA]);
        let hit = BlacklistEvaluator::evaluate(&insp, &[log], &snap).expect("hit");
        assert_eq!(hit.category, HitCategory::Call);
    }

    #[test]
    fn topic0_constants_match_keccak() {
        assert_eq!(*TRANSFER_TOPIC0, keccak256(b"Transfer(address,address,uint256)"));
        assert_eq!(
            *TRANSFER_SINGLE_TOPIC0,
            keccak256(b"TransferSingle(address,address,address,uint256,uint256)")
        );
        assert_eq!(
            *TRANSFER_BATCH_TOPIC0,
            keccak256(b"TransferBatch(address,address,address,uint256[],uint256[])")
        );
    }
}
