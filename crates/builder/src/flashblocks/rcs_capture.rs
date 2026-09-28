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
}
