//! FR-2 — execution-time observation accumulator.
//!
//! During transaction execution the revm `Inspector` adapter (wired in the builder /
//! executor crates) feeds observations into [`BlacklistInspector`]: the CALL-frame tree
//! (with per-frame revert status), selfdestruct events, and the per-candidate ETH
//! balance probe used by check③. This type holds only plain data + pure derivations so
//! it compiles and unit-tests without any revm coupling. [TD §4.3]

use alloy_primitives::{Address, I256, U256};

/// One observed CALL frame. A "touch" of `{from, to}` is *committed* iff this frame and
/// every ancestor frame did not revert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFrameRecord {
    /// Caller of the frame.
    pub from: Address,
    /// Callee / target of the frame.
    pub to: Address,
    /// Whether this frame itself reverted.
    pub reverted: bool,
    /// Index of the parent frame in [`BlacklistInspector::frames`], or `None` for the root.
    pub parent: Option<usize>,
}

/// A candidate address for the ETH-balance check (check③).
///
/// `balance_start` / `balance_end` are the committed pre/post-tx balances; `fee_delta` is
/// the **signed** fee-only adjustment (gas pre-charge/refund on the sender, tx-fee reward
/// on the coinbase). A hit is `(balance_end - balance_start - fee_delta) != 0`. [TD §4.3]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceCandidate {
    /// The address whose net balance change is examined.
    pub address: Address,
    /// Committed balance before the transaction.
    pub balance_start: U256,
    /// Committed balance after the transaction.
    pub balance_end: U256,
    /// Signed fee-only delta to strip before judging a real transfer.
    pub fee_delta: I256,
    /// Whether this candidate arises from a selfdestruct beneficiary (affects category).
    pub selfdestruct: bool,
}

/// Accumulates per-transaction execution observations for the three checks.
#[derive(Debug, Default, Clone)]
pub struct BlacklistInspector {
    frames: Vec<CallFrameRecord>,
    balance_candidates: Vec<BalanceCandidate>,
}

impl BlacklistInspector {
    /// A fresh, empty inspector (one per transaction).
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new CALL frame; returns its index for use as a child's `parent`.
    pub fn record_call_frame(
        &mut self,
        from: Address,
        to: Address,
        parent: Option<usize>,
    ) -> usize {
        let idx = self.frames.len();
        self.frames.push(CallFrameRecord { from, to, reverted: false, parent });
        idx
    }

    /// Mark a previously-recorded frame as reverted.
    pub fn mark_reverted(&mut self, idx: usize) {
        if let Some(f) = self.frames.get_mut(idx) {
            f.reverted = true;
        }
    }

    /// Record a balance-check candidate.
    pub fn record_balance_candidate(&mut self, candidate: BalanceCandidate) {
        self.balance_candidates.push(candidate);
    }

    /// All recorded frames.
    pub fn frames(&self) -> &[CallFrameRecord] {
        &self.frames
    }

    /// All recorded balance candidates.
    pub fn balance_candidates(&self) -> &[BalanceCandidate] {
        &self.balance_candidates
    }

    /// Addresses touched (`from`/`to`) by at least one committed frame — i.e. a frame
    /// whose entire ancestor chain (including itself) did not revert. Deduplicated.
    pub fn committed_touches(&self) -> Vec<Address> {
        let mut out: Vec<Address> = Vec::new();
        'frames: for (i, frame) in self.frames.iter().enumerate() {
            // Walk the ancestor chain including this frame.
            let mut cursor = Some(i);
            while let Some(ci) = cursor {
                match self.frames.get(ci) {
                    Some(f) if f.reverted => continue 'frames,
                    Some(f) => cursor = f.parent,
                    None => continue 'frames, // dangling parent → treat as non-committed
                }
            }
            out.push(frame.from);
            out.push(frame.to);
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    const A: Address = address!("00000000000000000000000000000000000000aa");
    const B: Address = address!("00000000000000000000000000000000000000bb");
    const C: Address = address!("00000000000000000000000000000000000000cc");

    #[test]
    fn committed_touches_collects_non_reverted_frames() {
        let mut insp = BlacklistInspector::new();
        let root = insp.record_call_frame(A, B, None);
        insp.record_call_frame(B, C, Some(root));
        let touches = insp.committed_touches();
        assert!(touches.contains(&A) && touches.contains(&B) && touches.contains(&C));
    }

    #[test]
    fn reverted_frame_touches_are_excluded() {
        // DM-2.10 — inner frame touching C reverts; only A,B from the committed root remain.
        let mut insp = BlacklistInspector::new();
        let root = insp.record_call_frame(A, B, None);
        let inner = insp.record_call_frame(B, C, Some(root));
        insp.mark_reverted(inner);
        let touches = insp.committed_touches();
        assert!(touches.contains(&A) && touches.contains(&B));
        assert!(!touches.contains(&C));
    }

    #[test]
    fn ancestor_revert_excludes_descendant() {
        // Parent reverts → child's touch is not committed even if the child did not revert.
        let mut insp = BlacklistInspector::new();
        let root = insp.record_call_frame(A, B, None);
        insp.record_call_frame(B, C, Some(root));
        insp.mark_reverted(root);
        let touches = insp.committed_touches();
        assert!(touches.is_empty());
    }
}
