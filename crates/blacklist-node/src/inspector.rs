//! FR-2 — revm `Inspector` adapter feeding the pure [`BlacklistInspector`] accumulator.
//!
//! Records the CALL-frame tree (with parent links + revert status) and selfdestruct
//! beneficiaries during execution; the executor wrapper additionally records ETH-balance
//! candidates from the post-state diff. The accumulated observations are then judged by
//! [`xlayer_blacklist::BlacklistEvaluator`].
//!
//! Grounded signatures (verbatim, extracted sources):
//! - `revm::Inspector<CTX, INTR = EthInterpreter>` — `revm-38.0.0/src/lib.rs:44` re-export;
//!   hooks `call`/`call_end`/`selfdestruct` — `revm-inspector-19.0.0/src/inspector.rs:96,108,143`.
//! - `CallInputs { caller, target_address, value: CallValue, … }` —
//!   `revm-interpreter-35.0.1/src/interpreter_action/call_inputs.rs:135,155,159,165`.
//! - `CallOutcome::instruction_result(&self) -> &InstructionResult` — `…/call_outcome.rs:65`.
//! - `InstructionResult::is_revert(self) -> bool` — `…/instruction_result.rs:268`.

use alloy_primitives::{Address, U256};
use revm::interpreter::{CallInputs, CallOutcome};
use revm::Inspector;
use xlayer_blacklist::BlacklistInspector;

/// revm `Inspector` that forwards call/selfdestruct observations into a
/// [`BlacklistInspector`]. One instance per transaction.
#[derive(Debug, Default)]
pub struct XLayerRevmInspector {
    inner: BlacklistInspector,
    /// Stack of currently-open frame indices, for parent linking.
    open_frames: Vec<usize>,
    /// Beneficiaries observed via the `selfdestruct` hook (for check③ category tagging).
    selfdestruct_targets: Vec<Address>,
}

impl XLayerRevmInspector {
    /// A fresh inspector for one transaction.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all per-transaction observations so a single inspector instance can be reused
    /// across the txs of the build loop (the EVM is built once outside the loop, so the
    /// inspector cannot be moved out per tx — it is cleared between txs instead).
    pub fn reset(&mut self) {
        self.inner = BlacklistInspector::new();
        self.open_frames.clear();
        self.selfdestruct_targets.clear();
    }

    /// Selfdestruct beneficiaries observed this transaction (for `check③` category).
    pub fn selfdestruct_targets(&self) -> &[Address] {
        &self.selfdestruct_targets
    }

    /// Borrow the accumulated observations (for `BlacklistEvaluator::evaluate`).
    pub fn observations(&self) -> &BlacklistInspector {
        &self.inner
    }

    /// Mutable borrow — the executor wrapper records balance candidates here from the
    /// post-state diff (check③ `balStart`/`balEnd`/`feeDelta`).
    pub fn observations_mut(&mut self) -> &mut BlacklistInspector {
        &mut self.inner
    }

    /// Consume and return the accumulated observations.
    pub fn into_observations(self) -> BlacklistInspector {
        self.inner
    }
}

impl<CTX> Inspector<CTX> for XLayerRevmInspector {
    fn call(&mut self, _context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        let parent = self.open_frames.last().copied();
        let idx = self.inner.record_call_frame(inputs.caller, inputs.target_address, parent);
        self.open_frames.push(idx);
        // Observe only; never short-circuit the call.
        None
    }

    fn call_end(&mut self, _context: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        if let Some(idx) = self.open_frames.pop()
            && outcome.instruction_result().is_revert()
        {
            self.inner.mark_reverted(idx);
        }
    }

    fn selfdestruct(&mut self, _contract: Address, target: Address, _value: U256) {
        // Record the beneficiary so check③'s state-diff reconstruction can tag its candidate
        // with the `selfdestruct` category. The actual net balance change is taken from the
        // committed state diff (authoritative), not from this hook's `value` — recording a
        // candidate here too would double-count.
        self.selfdestruct_targets.push(target);
    }
}
