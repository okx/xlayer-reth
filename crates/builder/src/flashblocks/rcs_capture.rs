//! Builder-side capture of internal native-token (OKB) balance transfers during simulation.
//!
//! This module adapts EVM execution to the risk-control screening input. During the real-user
//! candidate simulation the inspector records internal native-value movements — non-zero internal
//! `CALL` transfers, `CREATE`/`CREATE2` endowments, and `SELFDESTRUCT` beneficiary transfers — as
//! virtual `Transfer(address,address,uint256)` logs whose address is the native-asset identifier,
//! interleaved in execution order with the real EVM logs. The flattened stream is handed to
//! screening only; it never reaches a receipt, RPC log, bloom, the indexer, or consensus state.
//!
//! The screening crate stays free of any EVM / frame-lifecycle dependency: the inspector, its
//! capture-control seam, and the capture outcomes are all builder-owned types here.

use alloy_primitives::{address, keccak256, Address, Bytes, Log, LogData, B256, U256};
use revm::{
    context_interface::{ContextTr, CreateScheme, JournalTr},
    inspector::{JournalExt, NoOpInspector},
    interpreter::{CallInputs, CallOutcome, CreateInputs, CreateOutcome, InterpreterTypes},
    Inspector,
};

/// The native-asset identifier used as the `log.address` of every virtual native transfer.
///
/// This is a native-coin identifier matched by **address value** (not checksum-string case), never
/// an ERC20 contract: the screening side never calls `decimals`/`balanceOf`/`transfer` on it.
pub const NATIVE_ASSET_ADDRESS: Address = address!("EeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE");

/// Which internal EVM operation produced a native-transfer observation. All three funnel through
/// the single [`RcsInspector::observe_native_transfer`] so counting/filtering/overflow can never
/// diverge between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTransferKind {
    /// Non-zero internal `CALL` value transfer.
    Call,
    /// `CREATE`/`CREATE2` endowment to the created address.
    Create,
    /// `SELFDESTRUCT` beneficiary transfer.
    SelfDestruct,
}

/// Builds the virtual `Transfer(address,address,uint256)` log for a native transfer.
///
/// Shape: `log.address` = [`NATIVE_ASSET_ADDRESS`]; `topic0` =
/// `keccak256("Transfer(address,address,uint256)")`; `topic1` = left-padded `from`; `topic2` =
/// left-padded `to`; `data` = 32-byte big-endian `value` (wei).
pub fn native_transfer_log(from: Address, to: Address, value: U256) -> Log {
    let topics = vec![transfer_topic0(), from.into_word(), to.into_word()];
    let data = Bytes::from(value.to_be_bytes::<32>().to_vec());
    Log { address: NATIVE_ASSET_ADDRESS, data: LogData::new_unchecked(topics, data) }
}

/// `keccak256("Transfer(address,address,uint256)")` — the canonical ERC20/ERC1155 `Transfer`
/// event signature topic.
fn transfer_topic0() -> B256 {
    keccak256("Transfer(address,address,uint256)")
}

/// A specific observation invariant that failed during capture. Carries only indices/counts —
/// never log or transfer content — so error logging can never leak transaction data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureInvariantError {
    /// The captured real-log subsequence did not match `result.logs()` for content/count/order.
    RealLogMismatch { first_bad_index: usize, observed: usize, expected: usize },
}

/// The outcome of finishing a capture window. Never an `Option`: an enabled filter can never
/// silently fall back to raw `result.logs()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// Emitted only by the no-op inspector (filter disabled); the caller uses raw `result.logs()`.
    Passthrough,
    /// A validated, flattened ordered stream of real + virtual logs; the RealLog cross-check passed.
    Complete(Vec<Log>),
    /// The per-transaction native-observation budget was exceeded. Fail closed.
    LimitExceeded { attempted: usize, limit: usize },
    /// An observation invariant failed. Fail closed.
    InvariantViolation { reason: CaptureInvariantError },
}

/// Builder-internal capture-control seam. These methods are **not** part of revm's `Inspector`
/// trait, so a generic execution loop bounds its inspector on `Inspector + RcsCaptureControl` to
/// drive capture without a runtime downcast. The `rcs-filter` crate gains no revm dependency.
pub trait RcsCaptureControl {
    /// Resets all capture state and arms the inspector for one real-user candidate simulation.
    fn start_capture(&mut self);
    /// Ends the capture window, cross-checks the real-log subsequence against `result_logs`
    /// (the execution result's logs), and returns the outcome. Disarms the inspector.
    fn finish_capture(&mut self, result_logs: &[Log]) -> CaptureOutcome;
    /// Aborts capture (e.g. the transaction returned an error), clearing state and disarming.
    fn abort_capture(&mut self);
}

/// The filter-disabled inspector performs no capture: `start`/`abort` are no-ops and `finish`
/// always returns [`CaptureOutcome::Passthrough`], so the loop falls back to raw `result.logs()`.
impl RcsCaptureControl for NoOpInspector {
    fn start_capture(&mut self) {}

    fn finish_capture(&mut self, _result_logs: &[Log]) -> CaptureOutcome {
        CaptureOutcome::Passthrough
    }

    fn abort_capture(&mut self) {}
}

/// One entry in the unified, ordered observation stream.
#[derive(Debug, Clone)]
enum QueuedLog {
    /// A real EVM log captured via the `log`/`log_full` hook.
    Real(Log),
    /// A synthesized virtual native `Transfer` log.
    Virtual(Log),
}

impl QueuedLog {
    fn into_log(self) -> Log {
        match self {
            QueuedLog::Real(log) | QueuedLog::Virtual(log) => log,
        }
    }
}

/// The RCS capture inspector: the **inner** inspector of `alloy_op_evm::OpEvm`'s composite
/// wrapper. It maintains one ordered queue of real + virtual logs with per-frame checkpoints and a
/// single, non-rolling-back native-transfer counter with a sticky budget overflow.
///
/// All hooks are strict no-ops until an explicit [`RcsCaptureControl::start_capture`]; correctness
/// rests on this gate, not on caller discipline, and every window begins with a full reset so no
/// state can leak across transactions.
#[derive(Debug)]
pub struct RcsInspector {
    /// Armed only across the real candidate `evm.transact`; false during gasless pre-checks etc.
    active: bool,
    /// Ordered real + virtual logs for the current transaction.
    queue: Vec<QueuedLog>,
    /// Per-frame checkpoints: queue length at each frame's entry, for revert/halt truncation.
    checkpoints: Vec<usize>,
    /// Cumulative count of qualifying native-transfer candidates; never rolls back on revert.
    observed_count: usize,
    /// Set once `observed_count` exceeds `limit`; stops appending new virtual events (sticky).
    overflowed: bool,
    /// The configured per-transaction native-transfer budget.
    limit: usize,
}

impl RcsInspector {
    /// Creates an inactive inspector with the given native-transfer budget (`>= 1`, validated at
    /// startup).
    pub fn new(max_native_transfers: usize) -> Self {
        Self {
            active: false,
            queue: Vec::new(),
            checkpoints: Vec::new(),
            observed_count: 0,
            overflowed: false,
            limit: max_native_transfers,
        }
    }

    /// The single funnel where a native-transfer candidate is counted and (if within budget and a
    /// non-self, non-zero transfer) appended. Shared by the CALL, CREATE/CREATE2, and SELFDESTRUCT
    /// hooks so counting/filtering/overflow can never drift.
    ///
    /// Rules (spec §5.6): a qualifying candidate increments `observed_count` the moment it reaches
    /// its hook; the count never rolls back; the `limit`-th candidate is allowed; the
    /// `(limit + 1)`-th sets a sticky overflow and is not appended; after overflow the EVM keeps
    /// executing normally (semantics unaltered), only new virtual appends stop.
    pub fn observe_native_transfer(
        &mut self,
        from: Address,
        to: Address,
        value: U256,
        _kind: NativeTransferKind,
    ) {
        if !self.active {
            return;
        }
        // Zero-amount and self-transfers never qualify (defensive; hooks pre-filter too).
        if value.is_zero() || from == to {
            return;
        }
        self.observed_count += 1;
        if self.observed_count > self.limit {
            self.overflowed = true;
            return;
        }
        self.queue.push(QueuedLog::Virtual(native_transfer_log(from, to, value)));
    }

    /// CALL-frame native-transfer decision, split out from the `call` hook so it is testable
    /// without a full EVM context. `depth` is the journal depth at the frame (0 = top-level, which
    /// carries only `tx.value` and is excluded). `transfer_value` is `CallInputs::transfer_value()`
    /// — `Some` only for a real `CallValue::Transfer` (so DELEGATECALL/STATICCALL, whose value is
    /// *apparent*, never reach here); zero-value and self-transfers (e.g. CALLCODE, whose caller and
    /// target are the same) are filtered by [`Self::observe_native_transfer`].
    fn record_call_observation(
        &mut self,
        depth: usize,
        transfer_value: Option<U256>,
        from: Address,
        to: Address,
    ) {
        if depth > 0
            && let Some(value) = transfer_value
        {
            self.observe_native_transfer(from, to, value, NativeTransferKind::Call);
        }
    }

    /// CREATE/CREATE2-frame native-transfer decision, split out from the `create` hook for
    /// testability. Top-level creations (`depth == 0`) and zero endowments generate no event; the
    /// recipient is the created address.
    fn record_create_observation(
        &mut self,
        depth: usize,
        value: U256,
        caller: Address,
        created_address: Address,
    ) {
        if depth > 0 && !value.is_zero() {
            self.observe_native_transfer(
                caller,
                created_address,
                value,
                NativeTransferKind::Create,
            );
        }
    }

    /// Records a real EVM log into the unified ordered queue. No-op while inactive.
    pub fn record_real_log(&mut self, log: Log) {
        if !self.active {
            return;
        }
        self.queue.push(QueuedLog::Real(log));
    }

    /// Pushes a checkpoint at the current queue position on frame entry. No-op while inactive.
    pub fn push_checkpoint(&mut self) {
        if !self.active {
            return;
        }
        self.checkpoints.push(self.queue.len());
    }

    /// Frame exit: on success keep the frame's events; on revert/halt/early-failure truncate the
    /// queue back to that frame's checkpoint. `observed_count` never rolls back. No-op while
    /// inactive.
    pub fn pop_checkpoint(&mut self, reverted: bool) {
        if !self.active {
            return;
        }
        // `pop()` always runs (balancing every `push_checkpoint`); the queue is only truncated
        // when the frame reverted/halted.
        if let Some(checkpoint) = self.checkpoints.pop()
            && reverted
        {
            self.queue.truncate(checkpoint);
        }
    }

    /// Extracts the real-log subsequence of the current queue (order preserved).
    fn real_log_subsequence(&self) -> Vec<&Log> {
        self.queue
            .iter()
            .filter_map(|entry| match entry {
                QueuedLog::Real(log) => Some(log),
                QueuedLog::Virtual(_) => None,
            })
            .collect()
    }
}

impl RcsCaptureControl for RcsInspector {
    fn start_capture(&mut self) {
        self.queue.clear();
        self.checkpoints.clear();
        self.observed_count = 0;
        self.overflowed = false;
        self.active = true;
    }

    fn finish_capture(&mut self, result_logs: &[Log]) -> CaptureOutcome {
        self.active = false;
        if self.overflowed {
            return CaptureOutcome::LimitExceeded {
                attempted: self.observed_count,
                limit: self.limit,
            };
        }
        // Cross-check: the RealLog subsequence must equal `result.logs()` for count, content, and
        // order. Any mismatch fails closed. Only indices/counts are surfaced, never content.
        let real = self.real_log_subsequence();
        if real.len() != result_logs.len() {
            let first_bad_index = real
                .iter()
                .zip(result_logs)
                .position(|(a, b)| *a != b)
                .unwrap_or_else(|| real.len().min(result_logs.len()));
            return CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::RealLogMismatch {
                    first_bad_index,
                    observed: real.len(),
                    expected: result_logs.len(),
                },
            };
        }
        if let Some(first_bad_index) = real.iter().zip(result_logs).position(|(a, b)| *a != b) {
            return CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::RealLogMismatch {
                    first_bad_index,
                    observed: real.len(),
                    expected: result_logs.len(),
                },
            };
        }
        let stream = std::mem::take(&mut self.queue).into_iter().map(QueuedLog::into_log).collect();
        CaptureOutcome::Complete(stream)
    }

    fn abort_capture(&mut self) {
        self.queue.clear();
        self.checkpoints.clear();
        self.observed_count = 0;
        self.overflowed = false;
        self.active = false;
    }
}

/// revm `Inspector` hooks (spec §5.3/§5.5). All hooks no-op while inactive via the helper methods,
/// so the gasless pre-check and any non-real-user simulation never capture. Every CALL/CREATE frame
/// (of any type) gets a checkpoint so real logs inside a later-reverted frame roll back; only
/// qualifying native-transfer candidates append a virtual event.
impl<CTX, INTR> Inspector<CTX, INTR> for RcsInspector
where
    CTX: ContextTr<Journal: JournalExt>,
    INTR: InterpreterTypes,
{
    fn log(&mut self, _context: &mut CTX, log: Log) {
        self.record_real_log(log);
    }

    fn call(&mut self, context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        // Every CALL-type frame (incl. zero-value / DELEGATECALL / STATICCALL / CALLCODE) gets a
        // checkpoint so real logs it captured roll back if it reverts.
        self.push_checkpoint();
        // `transfer_value()` is `Some` only for a real `CallValue::Transfer`; DELEGATECALL and
        // STATICCALL carry an *apparent* value (`None`) and never generate an event. CALLCODE does
        // report a transfer value, but its caller and target are the same account, so it is filtered
        // as a self-transfer inside `observe_native_transfer`. Top-level (`depth == 0`) is excluded.
        self.record_call_observation(
            context.journal().depth(),
            inputs.transfer_value(),
            inputs.transfer_from(),
            inputs.transfer_to(),
        );
        None
    }

    fn call_end(&mut self, _context: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        self.pop_checkpoint(!outcome.result.is_ok());
    }

    fn create(&mut self, context: &mut CTX, inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        self.push_checkpoint();
        // Non-top-level CREATE/CREATE2 with a non-zero endowment. The recipient is the created
        // address (nonce-derived for CREATE; salt/init-code-derived for CREATE2). The created-address
        // derivation needs the context, so it is done here and handed to the (context-free) decision.
        let depth = context.journal().depth();
        let value = inputs.value();
        if depth > 0 && !value.is_zero() {
            let caller = inputs.caller();
            let created_address = match inputs.scheme() {
                CreateScheme::Create => {
                    let nonce = context
                        .journal_ref()
                        .evm_state()
                        .get(&caller)
                        .map(|account| account.info.nonce)
                        .unwrap_or_default();
                    inputs.created_address(nonce)
                }
                _ => inputs.created_address(0),
            };
            self.record_create_observation(depth, value, caller, created_address);
        }
        None
    }

    fn create_end(
        &mut self,
        _context: &mut CTX,
        _inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        self.pop_checkpoint(!outcome.result.is_ok());
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        // SELFDESTRUCT is an opcode-level internal transfer, counted even in the root frame (not
        // journal-depth-excluded). `observe_native_transfer` filters zero-value and self-transfers.
        self.observe_native_transfer(contract, target, value, NativeTransferKind::SelfDestruct);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        Address::repeat_byte(byte)
    }

    fn real_log(byte: u8) -> Log {
        Log {
            address: Address::repeat_byte(byte),
            data: LogData::new_unchecked(vec![B256::repeat_byte(byte)], Bytes::new()),
        }
    }

    // ---- Task 4: native asset address + virtual Transfer encoder --------------------------------

    #[test]
    fn native_asset_address_matches_identifier() {
        assert_eq!(
            NATIVE_ASSET_ADDRESS,
            "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE".parse::<Address>().unwrap()
        );
    }

    #[test]
    fn native_transfer_log_has_exact_shape() {
        let log = native_transfer_log(addr(0xAA), addr(0xBB), U256::from(123u64));
        assert_eq!(log.address, NATIVE_ASSET_ADDRESS);
        assert_eq!(log.topics()[0], keccak256("Transfer(address,address,uint256)"));
        assert_eq!(log.topics()[1], addr(0xAA).into_word());
        assert_eq!(log.topics()[2], addr(0xBB).into_word());
        assert_eq!(log.data.data.as_ref(), &U256::from(123u64).to_be_bytes::<32>());
    }

    // ---- Task 5: NoOpInspector capture-control seam ---------------------------------------------

    #[test]
    fn noop_inspector_finish_is_passthrough() {
        let mut inspector = NoOpInspector;
        inspector.start_capture();
        assert_eq!(inspector.finish_capture(&[]), CaptureOutcome::Passthrough);
        inspector.abort_capture();
    }

    // ---- Task 6: lifecycle, unified counter, sticky overflow ------------------------------------

    #[test]
    fn inactive_inspector_ignores_all_observations() {
        let mut inspector = RcsInspector::new(10);
        // Observations before start_capture are dropped by the inactive gate.
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        inspector.record_real_log(real_log(3));
        inspector.start_capture();
        assert_eq!(inspector.observed_count, 0);
        assert!(
            matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete(v) if v.is_empty())
        );
    }

    #[test]
    fn observed_count_increments_and_does_not_roll_back() {
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.push_checkpoint(); // root frame checkpoint at 0
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        assert_eq!(inspector.observed_count, 3);
        // Roll the frame back: virtual events truncated, but the count stays.
        inspector.pop_checkpoint(true);
        assert!(inspector.queue.is_empty());
        assert_eq!(inspector.observed_count, 3);
    }

    #[test]
    fn limit_th_allowed_limit_plus_one_overflows_sticky() {
        let mut inspector = RcsInspector::new(2);
        inspector.start_capture();
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        assert!(matches!(
            inspector.finish_capture(&[]),
            CaptureOutcome::LimitExceeded { attempted: 3, limit: 2 }
        ));
    }

    #[test]
    fn identical_from_to_value_not_deduplicated() {
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(5u64),
            NativeTransferKind::Call,
        );
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(5u64),
            NativeTransferKind::Call,
        );
        match inspector.finish_capture(&[]) {
            CaptureOutcome::Complete(logs) => assert_eq!(logs.len(), 2),
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn zero_and_self_transfers_are_ignored() {
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.observe_native_transfer(addr(1), addr(2), U256::ZERO, NativeTransferKind::Call);
        inspector.observe_native_transfer(
            addr(1),
            addr(1),
            U256::from(9u64),
            NativeTransferKind::Call,
        );
        assert_eq!(inspector.observed_count, 0);
        assert!(
            matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete(v) if v.is_empty())
        );
    }

    #[test]
    fn start_capture_resets_state_between_txs() {
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        inspector.record_real_log(real_log(3));
        // A fresh window wipes everything.
        inspector.start_capture();
        assert!(inspector.queue.is_empty());
        assert_eq!(inspector.observed_count, 0);
        assert!(!inspector.overflowed);
    }

    // ---- Task 8: finish_capture RealLog cross-check ---------------------------------------------

    #[test]
    fn matching_real_logs_yield_complete() {
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.record_real_log(real_log(1));
        inspector.observe_native_transfer(
            addr(4),
            addr(5),
            U256::from(7u64),
            NativeTransferKind::Call,
        );
        inspector.record_real_log(real_log(2));
        match inspector.finish_capture(&[real_log(1), real_log(2)]) {
            // real subsequence [1,2] == result.logs() [1,2]; virtual native log interleaves.
            CaptureOutcome::Complete(stream) => assert_eq!(stream.len(), 3),
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn real_log_count_mismatch_fails_closed() {
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.record_real_log(real_log(1));
        assert!(matches!(
            inspector.finish_capture(&[real_log(1), real_log(2)]),
            CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::RealLogMismatch { observed: 1, expected: 2, .. }
            }
        ));
    }

    #[test]
    fn real_log_order_mismatch_fails_closed() {
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.record_real_log(real_log(1));
        inspector.record_real_log(real_log(2));
        assert!(matches!(
            inspector.finish_capture(&[real_log(2), real_log(1)]),
            CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::RealLogMismatch { first_bad_index: 0, .. }
            }
        ));
    }

    // ---- Task 11: budget boundaries + unified SELFDESTRUCT budget -------------------------------

    fn observe_n(inspector: &mut RcsInspector, kind: NativeTransferKind, n: usize) {
        for _ in 0..n {
            inspector.observe_native_transfer(addr(1), addr(2), U256::from(1u64), kind);
        }
    }

    #[test]
    fn native_budget_boundary_10000_allowed_10001_overflows() {
        let mut ok = RcsInspector::new(10_000);
        ok.start_capture();
        observe_n(&mut ok, NativeTransferKind::Call, 10_000);
        match ok.finish_capture(&[]) {
            CaptureOutcome::Complete(logs) => assert_eq!(logs.len(), 10_000),
            other => panic!("expected Complete at limit, got {other:?}"),
        }

        let mut over = RcsInspector::new(10_000);
        over.start_capture();
        observe_n(&mut over, NativeTransferKind::Call, 10_001);
        assert!(matches!(
            over.finish_capture(&[]),
            CaptureOutcome::LimitExceeded { attempted: 10_001, limit: 10_000 }
        ));
    }

    #[test]
    fn selfdestruct_shares_unified_budget_with_call() {
        // limit 1: one qualifying CALL then one qualifying SELFDESTRUCT ⇒ the second overflows.
        let mut inspector = RcsInspector::new(1);
        inspector.start_capture();
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        inspector.observe_native_transfer(
            addr(3),
            addr(4),
            U256::from(1u64),
            NativeTransferKind::SelfDestruct,
        );
        assert!(matches!(
            inspector.finish_capture(&[]),
            CaptureOutcome::LimitExceeded { attempted: 2, limit: 1 }
        ));
    }

    #[test]
    fn selfdestruct_then_parent_revert_removes_event_but_count_stays() {
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.push_checkpoint(); // parent frame
        inspector.observe_native_transfer(
            addr(3),
            addr(4),
            U256::from(9u64),
            NativeTransferKind::SelfDestruct,
        );
        assert_eq!(inspector.observed_count, 1);
        inspector.pop_checkpoint(true); // parent reverts
        assert!(inspector.queue.is_empty());
        assert_eq!(inspector.observed_count, 1); // count does not roll back
    }

    #[test]
    fn selfdestruct_zero_or_self_target_consumes_no_budget() {
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.observe_native_transfer(
            addr(3),
            addr(4),
            U256::ZERO,
            NativeTransferKind::SelfDestruct,
        );
        inspector.observe_native_transfer(
            addr(3),
            addr(3),
            U256::from(9u64),
            NativeTransferKind::SelfDestruct,
        );
        assert_eq!(inspector.observed_count, 0);
        assert!(
            matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete(v) if v.is_empty())
        );
    }

    // ---- Task 7: context-free CALL/CREATE hook decision logic ------------------------------------
    // Exercise the field/depth decisions the revm hooks delegate to (the hooks themselves only
    // extract depth/inputs from the EVM context). Runtime execution through OpEvm is the follow-up.

    fn armed(limit: usize) -> RcsInspector {
        let mut inspector = RcsInspector::new(limit);
        inspector.start_capture();
        inspector
    }

    #[test]
    fn call_top_level_generates_no_event() {
        // depth 0 = the tx's own top-level call; its value is top-level tx.value, excluded.
        let mut i = armed(10);
        i.record_call_observation(0, Some(U256::from(5u64)), addr(1), addr(2));
        assert_eq!(i.observed_count, 0);
    }

    #[test]
    fn call_internal_nonzero_transfer_is_observed() {
        let mut i = armed(10);
        i.record_call_observation(1, Some(U256::from(5u64)), addr(1), addr(2));
        assert_eq!(i.observed_count, 1);
        match i.finish_capture(&[]) {
            CaptureOutcome::Complete(logs) => {
                assert_eq!(logs.len(), 1);
                assert_eq!(logs[0].address, NATIVE_ASSET_ADDRESS);
                assert_eq!(logs[0].topics()[1], addr(1).into_word());
                assert_eq!(logs[0].topics()[2], addr(2).into_word());
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn call_apparent_value_generates_no_event() {
        // DELEGATECALL / STATICCALL: transfer_value() is None.
        let mut i = armed(10);
        i.record_call_observation(1, None, addr(1), addr(2));
        assert_eq!(i.observed_count, 0);
    }

    #[test]
    fn call_self_transfer_generates_no_event() {
        // CALLCODE reports a transfer value but caller == target, so it is a self-transfer.
        let mut i = armed(10);
        i.record_call_observation(1, Some(U256::from(5u64)), addr(1), addr(1));
        assert_eq!(i.observed_count, 0);
    }

    #[test]
    fn create_top_level_generates_no_event() {
        let mut i = armed(10);
        i.record_create_observation(0, U256::from(5u64), addr(1), addr(2));
        assert_eq!(i.observed_count, 0);
    }

    #[test]
    fn create_internal_endowment_is_observed_to_created_address() {
        let mut i = armed(10);
        let created = addr(9);
        i.record_create_observation(1, U256::from(7u64), addr(1), created);
        match i.finish_capture(&[]) {
            CaptureOutcome::Complete(logs) => {
                assert_eq!(logs.len(), 1);
                assert_eq!(logs[0].topics()[2], created.into_word()); // recipient = created address
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn create_zero_endowment_generates_no_event() {
        let mut i = armed(10);
        i.record_create_observation(1, U256::ZERO, addr(1), addr(2));
        assert_eq!(i.observed_count, 0);
    }

    // ---- R8: per-operation, never netted -------------------------------------------------------

    #[test]
    fn reverse_transfers_are_two_ordered_events_not_netted() {
        // A→B: v then B→A: v (account net zero) must yield TWO ordered virtual Transfers, in order,
        // never offset/netted/deduped.
        let mut i = armed(10);
        i.observe_native_transfer(addr(0xA), addr(0xB), U256::from(1u64), NativeTransferKind::Call);
        i.observe_native_transfer(addr(0xB), addr(0xA), U256::from(1u64), NativeTransferKind::Call);
        match i.finish_capture(&[]) {
            CaptureOutcome::Complete(logs) => {
                assert_eq!(logs.len(), 2, "net-zero pair must not be offset/deduped");
                // Execution order preserved: first A→B, then B→A.
                assert_eq!(logs[0].topics()[1], addr(0xA).into_word());
                assert_eq!(logs[0].topics()[2], addr(0xB).into_word());
                assert_eq!(logs[1].topics()[1], addr(0xB).into_word());
                assert_eq!(logs[1].topics()[2], addr(0xA).into_word());
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn reverse_transfers_parent_revert_removes_both_count_stays() {
        let mut i = armed(10);
        i.push_checkpoint(); // parent frame
        i.observe_native_transfer(addr(0xA), addr(0xB), U256::from(1u64), NativeTransferKind::Call);
        i.observe_native_transfer(addr(0xB), addr(0xA), U256::from(1u64), NativeTransferKind::Call);
        assert_eq!(i.observed_count, 2);
        i.pop_checkpoint(true); // parent reverts → both dropped from the stream
        assert!(i.queue.is_empty());
        assert_eq!(i.observed_count, 2, "observed_count must not roll back on revert");
    }
}

/// Production-shaped GATING runtime integration test (design §8). Builds a real
/// `alloy_op_evm::OpEvm<_, RcsInspector, _>` — the `RcsInspector` is the inner inspector of the
/// baked-in `PostExecCompositeInspector` — via the same factory the flashblocks path uses, executes
/// real transactions through `evm.transact_raw`, and asserts capture happens THROUGH the composite
/// wrapper at runtime. This proves the "inner implements the hook, the wrapper must forward it"
/// property (§4/§5.3) that a helper-level unit test cannot; a green CI pipeline does NOT replace it.
#[cfg(test)]
mod gating_integration_tests {
    use super::*;
    use alloy_consensus::{SignableTransaction, TxLegacy};
    use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
    use alloy_op_evm::{OpEvmFactory, OpTx};
    use alloy_primitives::{Signature, TxKind};
    use op_revm::OpSpecId;
    use revm::context::{BlockEnv, CfgEnv};
    use revm::database::InMemoryDB;
    use revm::state::{AccountInfo, Bytecode};

    fn caller() -> Address {
        Address::from([0xAA; 20])
    }

    fn account(balance: u64, code: Option<Vec<u8>>) -> AccountInfo {
        AccountInfo {
            balance: U256::from(balance),
            code: code.map(|c| Bytecode::new_raw(alloy_primitives::Bytes::from(c))),
            ..Default::default()
        }
    }

    fn call_tx(target: Address) -> OpTx {
        call_tx_from(caller(), target)
    }

    /// Same as [`call_tx`] but from an explicit sender. Two candidate txs in one test can then share
    /// a single EVM/inspector without a nonce-ordering dependency: each distinct sender uses nonce 0.
    fn call_tx_from(from: Address, target: Address) -> OpTx {
        let tx = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Call(target),
            value: U256::ZERO,
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        OpTx::from_recovered_tx(&tx, from)
    }

    /// Executes one candidate tx through a real composite-wrapped `OpEvm` and returns the capture
    /// outcome plus whether the top-level tx succeeded. The capture seam (`start_capture` →
    /// `transact_raw` → `finish_capture(result.logs())`) mirrors the builder's real path.
    fn run(db: InMemoryDB, target: Address, budget: usize) -> (CaptureOutcome, bool) {
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(budget),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact_raw(call_tx(target)).expect("tx executes");
        let success = result.result.is_success();
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());
        (outcome, success)
    }

    /// Runtime bytecode: `CALL(gas, dst, value=1, 0, 0, 0, 0); STOP` — one internal 1-wei transfer.
    fn call_value_1_to(dst: Address) -> Vec<u8> {
        // PUSH1 0 (retSize) PUSH1 0 (retOffset) PUSH1 0 (argsSize) PUSH1 0 (argsOffset)
        // PUSH1 1 (value)   PUSH20 dst          GAS                CALL                 STOP
        let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0x73];
        code.extend_from_slice(dst.as_slice());
        code.extend_from_slice(&[0x5a, 0xf1, 0x00]);
        code
    }

    #[test]
    fn internal_call_transfer_captured_through_composite() {
        // caller → contract (value 0); contract makes an internal CALL transferring 1 wei to dst.
        // Proves the composite forwards `call` to the inner RcsInspector at runtime, that
        // `transfer_value`/`transfer_from`/`transfer_to` are read correctly, and top-level exclusion
        // (the outer caller→contract call at journal depth 0 is NOT captured).
        let contract = Address::from([0xCC; 20]);
        let dst = Address::from([0xDD; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(contract, account(1_000, Some(call_value_1_to(dst))));

        let (outcome, success) = run(db, contract, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "exactly one internal native transfer expected");
                assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
                assert_eq!(stream[0].topics()[1], contract.into_word());
                assert_eq!(stream[0].topics()[2], dst.into_word());
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(1u64).to_be_bytes::<32>());
            }
            other => panic!("expected Complete with one native transfer, got {other:?}"),
        }
    }

    /// Runtime bytecode: `CREATE(value=7, offset=0, size=0); STOP` — one CREATE with a 7-wei endowment
    /// and empty init code.
    fn create_endowment_7() -> Vec<u8> {
        // PUSH1 0 (size) PUSH1 0 (offset) PUSH1 7 (value) CREATE STOP
        vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x07, 0xf0, 0x00]
    }

    #[test]
    fn internal_create_endowment_captured_through_composite() {
        // A depth>0 CREATE with a non-zero endowment yields one native Transfer from the creator to
        // the created address (recipient derivation runs through the composite-forwarded `create`).
        let factory = Address::from([0xF1; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(factory, account(1_000, Some(create_endowment_7())));

        let (outcome, success) = run(db, factory, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "one CREATE endowment transfer expected");
                assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
                assert_eq!(stream[0].topics()[1], factory.into_word());
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(7u64).to_be_bytes::<32>());
                // Recipient (topic2) must be the newly created address — not the creator, the tx
                // caller, or zero. Guards against a recipient-derivation bug that credits the wrong
                // account (robust to revm's contract-nonce semantics, which fix the exact address).
                let created = stream[0].topics()[2];
                assert_ne!(
                    created,
                    factory.into_word(),
                    "recipient is the created addr, not creator"
                );
                assert_ne!(
                    created,
                    caller().into_word(),
                    "recipient is the created addr, not caller"
                );
                assert_ne!(created, B256::ZERO, "recipient must be a real derived address");
            }
            other => panic!("expected Complete with one CREATE transfer, got {other:?}"),
        }
    }

    /// Runtime bytecode: `PUSH20 beneficiary; SELFDESTRUCT`.
    fn selfdestruct_to(beneficiary: Address) -> Vec<u8> {
        let mut code = vec![0x73];
        code.extend_from_slice(beneficiary.as_slice());
        code.push(0xff);
        code
    }

    #[test]
    fn selfdestruct_transfer_captured_through_composite() {
        // SELFDESTRUCT (value != 0, contract != beneficiary) is a qualifying native transfer even in
        // the root frame — proves the composite forwards `selfdestruct` to the inner inspector.
        let victim = Address::from([0xE1; 20]);
        let beneficiary = Address::from([0xB1; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(victim, account(500, Some(selfdestruct_to(beneficiary))));

        let (outcome, success) = run(db, victim, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "one SELFDESTRUCT transfer expected");
                assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
                assert_eq!(stream[0].topics()[1], victim.into_word());
                assert_eq!(stream[0].topics()[2], beneficiary.into_word());
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(500u64).to_be_bytes::<32>());
            }
            other => panic!("expected Complete with one SELFDESTRUCT transfer, got {other:?}"),
        }
    }

    /// Runtime bytecode: `CALL(gas, dst, value=0, 0,0,0,0); STOP` — a zero-value child call whose
    /// failure is ignored.
    fn call_value_0_to(dst: Address) -> Vec<u8> {
        let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73];
        code.extend_from_slice(dst.as_slice());
        code.extend_from_slice(&[0x5a, 0xf1, 0x00]);
        code
    }

    /// Runtime bytecode: `LOG0(offset=0, size=0); REVERT(0, 0)` — emits one real (empty) log, then
    /// reverts the frame.
    fn log_then_revert() -> Vec<u8> {
        vec![0x60, 0x00, 0x60, 0x00, 0xa0, 0x60, 0x00, 0x60, 0x00, 0xfd]
    }

    #[test]
    fn reverted_child_frame_rolls_back_real_log_and_cross_check_passes() {
        // A zero-value child CALL emits a real log then REVERTs. The captured real log must be
        // truncated on the sub-frame revert, and the finish_capture RealLog cross-check against
        // `result.logs()` (which excludes reverted logs) must still pass → Complete(empty).
        let parent = Address::from([0xA1; 20]);
        let child = Address::from([0xA2; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(parent, account(0, Some(call_value_0_to(child))));
        db.insert_account_info(child, account(0, Some(log_then_revert())));

        let (outcome, success) = run(db, parent, 16);
        assert!(success, "parent tx ignores the child revert and succeeds");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert!(
                    stream.is_empty(),
                    "reverted child frame's real log must be rolled back, got {stream:?}"
                );
            }
            other => panic!("expected Complete(empty) after child revert, got {other:?}"),
        }
    }

    /// Runtime bytecode: `CALL(gas, dst, value=1, 0,0,0,0); POP; LOG0(0, 0); STOP` — one internal
    /// 1-wei transfer followed by a persisted real `LOG0` in the same (successful) frame.
    fn call_value_1_then_log0(dst: Address) -> Vec<u8> {
        // PUSH1 0 (retSize) PUSH1 0 (retOffset) PUSH1 0 (argsSize) PUSH1 0 (argsOffset)
        // PUSH1 1 (value)   PUSH20 dst          GAS                CALL
        let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0x73];
        code.extend_from_slice(dst.as_slice());
        // GAS CALL POP  PUSH1 0 (size) PUSH1 0 (offset) LOG0  STOP
        code.extend_from_slice(&[0x5a, 0xf1, 0x50, 0x60, 0x00, 0x60, 0x00, 0xa0, 0x00]);
        code
    }

    #[test]
    fn internal_transfer_then_real_log_interleaved_captured_through_composite() {
        // A successful contract makes an internal value CALL and then emits a real LOG0. The stream
        // must contain BOTH the virtual native `Transfer` and the persisted real log, interleaved in
        // execution order [virtual, real]. This is the non-tautological real-log proof: if the
        // composite stopped forwarding `log`/`log_full` to the inner inspector, the RealLog
        // subsequence would be empty while `result.logs()` has one entry, so `finish_capture` would
        // fail closed with `InvariantViolation` instead of `Complete` — the assert below would fail.
        let contract = Address::from([0xC5; 20]);
        let dst = Address::from([0xD5; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(contract, account(1_000, Some(call_value_1_then_log0(dst))));

        let (outcome, success) = run(db, contract, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(
                    stream.len(),
                    2,
                    "one virtual transfer + one real log expected, in order"
                );
                // [0] virtual native transfer (contract -> dst, value 1).
                assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
                assert_eq!(stream[0].topics()[1], contract.into_word());
                assert_eq!(stream[0].topics()[2], dst.into_word());
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(1u64).to_be_bytes::<32>());
                // [1] the persisted real LOG0 — proves real-log forwarding + the cross-check passed.
                assert_ne!(
                    stream[1].address, NATIVE_ASSET_ADDRESS,
                    "second entry must be the real log, not another virtual transfer"
                );
                assert_eq!(
                    stream[1].address, contract,
                    "real log carries the emitting contract addr"
                );
            }
            other => panic!("expected Complete([virtual, real]) interleaved, got {other:?}"),
        }
    }

    /// Runtime bytecode: two internal 1-wei CALLs (to `a` then `b`), each result popped, then STOP.
    fn call_value_1_twice(a: Address, b: Address) -> Vec<u8> {
        let mut code = Vec::new();
        for dst in [a, b] {
            // PUSH1 0 x4 (ret/args) PUSH1 1 (value) PUSH20 dst GAS CALL POP
            code.extend_from_slice(&[
                0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0x73,
            ]);
            code.extend_from_slice(dst.as_slice());
            code.extend_from_slice(&[0x5a, 0xf1, 0x50]);
        }
        code.push(0x00); // STOP
        code
    }

    #[test]
    fn inspector_state_does_not_cross_transactions_through_real_evm() {
        // Two candidate txs share ONE inspector (as in the real per-block loop), distinct senders so
        // neither depends on the other committing. tx1 makes TWO internal transfers against a budget
        // of 1, so it overflows: `finish_capture` returns `LimitExceeded` WITHOUT draining the queue
        // (that early-return path skips the `mem::take`), leaving residual queue + `observed_count` +
        // `overflowed` state on the inspector. `start_capture` at the head of tx2 MUST wipe all of it,
        // or tx2 would inherit tx1's transfers (queue) and/or be born already overflowed. If any reset
        // regressed, tx2 would not be a clean `Complete([d2 transfer])` and the asserts below fail.
        let sender_a = Address::from([0xA5; 20]);
        let sender_b = Address::from([0xB5; 20]);
        let c1 = Address::from([0xC1; 20]);
        let d1a = Address::from([0xD1; 20]);
        let d1b = Address::from([0xD3; 20]);
        let c2 = Address::from([0xC2; 20]);
        let d2 = Address::from([0xD2; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(sender_a, account(1_000_000_000, None));
        db.insert_account_info(sender_b, account(1_000_000_000, None));
        db.insert_account_info(c1, account(1_000, Some(call_value_1_twice(d1a, d1b))));
        db.insert_account_info(c2, account(1_000, Some(call_value_1_to(d2))));

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(1),
        );

        // tx1 -> c1: two transfers over a budget of 1 ⇒ overflow (queue not drained on this path).
        evm.components_mut().1.start_capture();
        let r1 = evm.transact_raw(call_tx_from(sender_a, c1)).expect("tx1 executes");
        let o1 = evm.components_mut().1.finish_capture(r1.result.logs());
        assert!(
            matches!(o1, CaptureOutcome::LimitExceeded { .. }),
            "tx1 must overflow the native-transfer budget, got {o1:?}"
        );

        // tx2 -> c2 on the SAME (dirty) inspector: start_capture must reset queue/count/overflow.
        evm.components_mut().1.start_capture();
        let r2 = evm.transact_raw(call_tx_from(sender_b, c2)).expect("tx2 executes");
        let o2 = evm.components_mut().1.finish_capture(r2.result.logs());
        match o2 {
            CaptureOutcome::Complete(s) => {
                assert_eq!(s.len(), 1, "tx2 must not inherit tx1's residual queue (reset per tx)");
                assert_eq!(s[0].topics()[2], d2.into_word(), "tx2's transfer targets d2 only");
            }
            other => panic!("tx2 expected clean Complete(1) after a dirty tx1, got {other:?}"),
        }
    }
}
