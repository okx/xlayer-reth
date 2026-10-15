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
    bytecode::opcode,
    context_interface::{ContextTr, CreateScheme, JournalTr},
    inspector::{JournalExt, NoOpInspector},
    interpreter::{
        interpreter_types::Jumps, CallInputs, CallOutcome, CreateInputs, CreateOutcome,
        Interpreter, InterpreterTypes,
    },
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
    /// A successful CREATE returned an address that differed from the pre-derived address stored
    /// in the CREATE frame's `create_recipients` entry. `create_index` is the 0-based depth of
    /// open CREATE frames at the point the mismatch was detected.
    CreateAddressMismatch { create_index: usize },
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

// Test-only "controlled inspector outcome" seam (spec §5.10.2 / §8 test 35). When set, the next
// `finish_capture` on an active inspector injects one extra real-log entry so the REAL RealLog
// cross-check diverges from `result.logs()` and returns a genuine
// `CaptureOutcome::InvariantViolation { RealLogMismatch }`. This lets the loop-level builder-path
// test drive a real runtime mismatch through the actual `execute_best_transactions` candidate loop
// (the loop's own inspector honors it) instead of hand-constructing a `CaptureOutcome`. One-shot:
// the flag is cleared as it is consumed so only the armed candidate is affected. Kept as `//` (not
// `///`) so the RcsInspector doc comment below is not orphaned onto this macro invocation.
#[cfg(test)]
thread_local! {
    pub(crate) static FORCE_REALLOG_MISMATCH: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

// Test-only "forced CREATE address mismatch" seam (D7b). When set, the next `create` hook on an
// active inspector pushes `Address::repeat_byte(0xFE)` as the expected recipient instead of the
// real derived address, so `create_end` sees `e != a` on a genuinely successful CREATE. One-shot:
// the flag is consumed (replaced with `false`) by the `create` hook so only the armed frame is
// affected. Kept as `//` so the struct doc below is not orphaned onto the macro.
#[cfg(test)]
thread_local! {
    pub(crate) static FORCE_CREATE_ADDRESS_MISMATCH: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

// Test-only create-hook gate snapshot. The `create` hook writes here at the exact point where it
// returns — before `create_end` has a chance to fire — so tests can assert that neither
// `create_recipients` nor `checkpoints` were mutated while the inspector was unarmed.
//
// Tuple layout: `(create_recipients_len, checkpoints_len, capture_body_ran, derivation_ran)`.
// - `capture_body_ran` — the hook reached the capture-relevant body (i.e. the inspector was armed).
// - `derivation_ran` — the journal read that the CREATE-scheme created-address derivation needs
//   actually executed. This is the COMPUTED value written by the hook, not a literal: `false`
//   alone would be ambiguous between "the hook never ran" and "it ran but no journal read was
//   needed", so the two are reported separately and a reader can always tell them apart.
// Never set in production (the whole seam is `#[cfg(test)]`).
#[cfg(test)]
thread_local! {
    pub(crate) static CREATE_GATE_SNAPSHOT: std::cell::Cell<(usize, usize, bool, bool)> =
        const { std::cell::Cell::new((0, 0, false, false)) };
}

// Test-only CREATE-completion outcome seam. The `create_end` hook records the REAL `CreateOutcome`
// it was handed, so tests can assert the actual completion validity instead of inferring it from the
// flattened capture stream. Tuple layout:
// `(is_ok, address_is_none, observed_count, address)`. `address` is the exact deployed address the
// EVM reported, which the CREATE-recipient assertions compare against. Never set in production.
#[cfg(test)]
thread_local! {
    pub(crate) static CREATE_END_OUTCOME: std::cell::Cell<(bool, bool, usize, Option<Address>)> =
        const { std::cell::Cell::new((false, false, 0, None)) };
}

// Test-only push/pop balance seam (T1-BALANCE). The `create_end` hook writes here after every
// `pop` and `pop_checkpoint` call, before `finish_capture` can run. Tuple layout:
// `(create_recipients_len, checkpoints_len)`. Proves that each `create` push is balanced by
// a `create_end` pop within the same EVM frame, not deferred to `finish_capture`. Never set
// in production.
#[cfg(test)]
thread_local! {
    pub(crate) static CREATE_END_BALANCE: std::cell::Cell<(usize, usize)> =
        const { std::cell::Cell::new((0, 0)) };
}

// Test-only create-hook invocation counter. Incremented as the FIRST statement
// of the `create` hook, before the `!self.active` gate, so `0` means the hook was never invoked at
// all for this transaction — distinguishable from `CREATE_GATE_SNAPSHOT`'s `derivation_ran == false`,
// which is ambiguous between "never hit" and "hit while inactive" without this counter. Never set
// in production (the whole seam is `#[cfg(test)]`).
#[cfg(test)]
thread_local! {
    pub(crate) static CREATE_HOOK_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only reset for the create-hook gate seams. Clears [`CREATE_HOOK_HITS`]
/// and [`CREATE_GATE_SNAPSHOT`] so a stale tuple left by an earlier test on the same thread (`cargo
/// test` reuses threads across tests, unlike `nextest`'s process-per-test) cannot make a later
/// assertion pass for the wrong reason.
#[cfg(test)]
pub(crate) fn reset_create_gate_seams() {
    CREATE_HOOK_HITS.with(|c| c.set(0));
    CREATE_GATE_SNAPSHOT.with(|s| s.set((0, 0, false, false)));
}

// Test-only SELFDESTRUCT-watermark gate snapshot, in the same style as `CREATE_GATE_SNAPSHOT` /
// `CREATE_HOOK_HITS`. Tuple layout: `(hook_hits, active_branch_ran, journal_read_ran)`.
// - `hook_hits` — incremented in `step`, as the FIRST thing done once the about-to-execute opcode
//   is recognized as `SELFDESTRUCT`, before the `!self.active` gate. `0` means "a SELFDESTRUCT
//   opcode's `step` was never reached"; distinguishable from "reached while inactive".
// - `active_branch_ran` — set once the `!self.active` gate has been passed for that opcode.
// - `journal_read_ran` — set immediately before the watermark journal read itself.
// Never set in production (the whole seam is `#[cfg(test)]`).
#[cfg(test)]
thread_local! {
    pub(crate) static SELFDESTRUCT_GATE_SNAPSHOT: std::cell::Cell<(usize, bool, bool)> =
        const { std::cell::Cell::new((0, false, false)) };
}

/// Test-only reset for [`SELFDESTRUCT_GATE_SNAPSHOT`], for the same reason
/// [`reset_create_gate_seams`] exists: `cargo test` reuses threads across tests.
#[cfg(test)]
pub(crate) fn reset_selfdestruct_gate_seams() {
    SELFDESTRUCT_GATE_SNAPSHOT.with(|s| s.set((0, false, false)));
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
    /// Per-CREATE-frame pre-derived recipient stack. `Some(addr)` when the CREATE carries a
    /// non-zero endowment; `None` otherwise. Balanced with `checkpoints`: one entry per
    /// `create` hook, popped on each `create_end`. Used to assert consistency between the
    /// pre-derived address and the real `CreateOutcome.address` on success.
    create_recipients: Vec<Option<Address>>,
    /// Sticky flag set in `create_end` when a successful CREATE returns an address that differs
    /// from the pre-derived expected address. Surfaced by `finish_capture` as
    /// `InvariantViolation { CreateAddressMismatch }` — never silently truncated. Cleared on
    /// every `start_capture`, `abort_capture`, and `finish_capture` exit.
    address_mismatch_create_index: Option<usize>,
    /// Journal length captured in `step` when the opcode about to execute is `SELFDESTRUCT`.
    /// `None` outside that one-opcode window. Only ever written while `active`.
    selfdestruct_journal_mark: Option<usize>,
    /// `true` when the last measured `SELFDESTRUCT` actually appended a journal entry, i.e. when
    /// the `selfdestruct` callback's payload describes THIS opcode rather than a previous entry.
    /// Consumed with `mem::take` by the callback. Only ever written while `active`.
    selfdestruct_wrote_journal: bool,
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
            create_recipients: Vec::new(),
            address_mismatch_create_index: None,
            selfdestruct_journal_mark: None,
            selfdestruct_wrote_journal: false,
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
    /// without a full EVM context. `transfer_value` is `CallInputs::transfer_value()`
    /// — `Some` only for a real `CallValue::Transfer` (so DELEGATECALL/STATICCALL, whose value is
    /// *apparent*, never reach here); zero-value and self-transfers (e.g. CALLCODE, whose caller and
    /// target are the same) are filtered by [`Self::observe_native_transfer`].
    fn record_call_observation(
        &mut self,
        transfer_value: Option<U256>,
        from: Address,
        to: Address,
    ) {
        if let Some(value) = transfer_value {
            self.observe_native_transfer(from, to, value, NativeTransferKind::Call);
        }
    }

    /// CREATE/CREATE2-frame native-transfer decision, split out from the `create` hook for
    /// testability. Zero endowments generate no event; the recipient is the created address.
    fn record_create_observation(
        &mut self,
        value: U256,
        caller: Address,
        created_address: Address,
    ) {
        self.observe_native_transfer(caller, created_address, value, NativeTransferKind::Create);
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
        self.create_recipients.clear();
        self.observed_count = 0;
        self.overflowed = false;
        self.address_mismatch_create_index = None;
        self.selfdestruct_journal_mark = None;
        self.selfdestruct_wrote_journal = false;
        self.active = true;
    }

    fn finish_capture(&mut self, result_logs: &[Log]) -> CaptureOutcome {
        // Test-only controlled-outcome seam (spec §5.10.2): when armed, inject a real-log divergence
        // so the REAL cross-check below yields a genuine `RealLogMismatch` on the live candidate-loop
        // path (used by the loop-level test 35 driving `execute_best_transactions`). No effect in
        // production builds — the whole block is `#[cfg(test)]`.
        #[cfg(test)]
        if self.active && FORCE_REALLOG_MISMATCH.with(|f| f.replace(false)) {
            self.queue.push(QueuedLog::Real(Log {
                address: NATIVE_ASSET_ADDRESS,
                data: LogData::new_unchecked(Vec::new(), Bytes::new()),
            }));
        }
        // Compute the outcome first — copying every count the outcome needs into locals — so the
        // uniform state clear below can run on EVERY return path (spec §5.2, R9 item 4). The earlier
        // implementation returned on the `LimitExceeded` / `InvariantViolation` paths *before*
        // clearing `queue`/`checkpoints`/`observed_count`/`overflowed`, leaking state into the next
        // capture window; cleanliness is now guaranteed immediately at finish, not only at reset.
        let outcome = if let Some(create_index) = self.address_mismatch_create_index {
            // D7b: a successful CREATE returned an address that differed from the pre-derived
            // expected address. Fail closed — never silently truncate with Complete.
            CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::CreateAddressMismatch { create_index },
            }
        } else if self.overflowed {
            CaptureOutcome::LimitExceeded { attempted: self.observed_count, limit: self.limit }
        } else {
            // Cross-check: the RealLog subsequence must equal `result.logs()` for count, content,
            // and order. Any mismatch fails closed. Only indices/counts are surfaced, never content.
            // `real` borrows `self.queue`, so it is scoped to this block and dropped before the
            // `mem::take` below can move the queue out.
            let mismatch = {
                let real = self.real_log_subsequence();
                let first_divergence = real.iter().zip(result_logs).position(|(a, b)| *a != b);
                if real.len() != result_logs.len() {
                    // Count differs: report the first differing index, or the shorter length when
                    // the shared prefix matches (one is a prefix of the other).
                    let first_bad_index =
                        first_divergence.unwrap_or_else(|| real.len().min(result_logs.len()));
                    Some((first_bad_index, real.len(), result_logs.len()))
                } else {
                    // Equal length: a mismatch exists only if some element differs.
                    first_divergence
                        .map(|first_bad_index| (first_bad_index, real.len(), result_logs.len()))
                }
            };
            match mismatch {
                Some((first_bad_index, observed, expected)) => CaptureOutcome::InvariantViolation {
                    reason: CaptureInvariantError::RealLogMismatch {
                        first_bad_index,
                        observed,
                        expected,
                    },
                },
                None => {
                    let stream = std::mem::take(&mut self.queue)
                        .into_iter()
                        .map(QueuedLog::into_log)
                        .collect();
                    CaptureOutcome::Complete(stream)
                }
            }
        };
        // Uniform every-exit cleanup (R9 item 4): leave the inspector inactive and fully reset so
        // nothing — queue, checkpoints, the native counter, the sticky overflow flag, or the sticky
        // address-mismatch flag — can leak into the next `start_capture`.
        self.queue.clear();
        self.checkpoints.clear();
        self.create_recipients.clear();
        self.observed_count = 0;
        self.overflowed = false;
        self.address_mismatch_create_index = None;
        self.selfdestruct_journal_mark = None;
        self.selfdestruct_wrote_journal = false;
        self.active = false;
        outcome
    }

    fn abort_capture(&mut self) {
        self.queue.clear();
        self.checkpoints.clear();
        self.create_recipients.clear();
        self.observed_count = 0;
        self.overflowed = false;
        self.address_mismatch_create_index = None;
        self.selfdestruct_journal_mark = None;
        self.selfdestruct_wrote_journal = false;
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

    fn call(&mut self, _context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        // Every CALL-type frame (incl. zero-value / DELEGATECALL / STATICCALL / CALLCODE) gets a
        // checkpoint so real logs it captured roll back if it reverts.
        self.push_checkpoint();
        // `transfer_value()` is `Some` only for a real `CallValue::Transfer`; DELEGATECALL and
        // STATICCALL carry an *apparent* value (`None`) and never generate an event. CALLCODE does
        // report a transfer value, but its caller and target are the same account, so it is filtered
        // as a self-transfer inside `observe_native_transfer`.
        self.record_call_observation(
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
        // Record that the hook was invoked at all, before anything else — including
        // the active gate — so a reader can tell "never hit" (count stays 0) from "hit while
        // inactive" (count > 0, capture_body_ran == false).
        #[cfg(test)]
        CREATE_HOOK_HITS.with(|c| c.set(c.get() + 1));
        self.push_checkpoint();
        if !self.active {
            // Not armed: no capture body ran, and therefore no address derivation ran either.
            #[cfg(test)]
            CREATE_GATE_SNAPSHOT.with(|s| {
                s.set((self.create_recipients.len(), self.checkpoints.len(), false, false))
            });
            return None;
        }
        // Capture the expected recipient for every CREATE/CREATE2 frame. The created-address
        // derivation needs the context, so it is computed here and stored for `create_end` to
        // verify against the actual outcome address (release-safe consistency guard).
        let value = inputs.value();
        let caller = inputs.caller();
        // Whether this frame's created-address derivation read journal state. A `CREATE` derives
        // its address from the caller's account nonce, which lives in the journal, so this branch
        // performs a journal read; a `CREATE2` derives its address from the salt and init-code hash
        // and reads no journal state. The flag is produced by the derivation itself rather than
        // assumed, so the test-only gate snapshot can report "the derivation ran" distinctly from
        // "the hook never ran" instead of collapsing both into one uninformative `false`.
        let (created_address, derivation_ran) = match inputs.scheme() {
            CreateScheme::Create => {
                let nonce = context
                    .journal_ref()
                    .evm_state()
                    .get(&caller)
                    .map(|account| account.info.nonce)
                    .unwrap_or_default();
                (inputs.created_address(nonce), true)
            }
            _ => (inputs.created_address(0), false),
        };
        // Test-only: when armed, push a WRONG expected address so `create_end` detects `e != a`
        // on a real successful CREATE and exercises the D7b fail-closed branch. One-shot: the flag
        // is consumed here so only the armed frame is affected. No effect in production builds.
        #[cfg(test)]
        if !value.is_zero() && FORCE_CREATE_ADDRESS_MISMATCH.with(|f| f.replace(false)) {
            self.create_recipients.push(Some(Address::repeat_byte(0xFE)));
            self.record_create_observation(value, caller, created_address);
            CREATE_GATE_SNAPSHOT.with(|s| {
                s.set((self.create_recipients.len(), self.checkpoints.len(), true, derivation_ran))
            });
            return None;
        }
        self.create_recipients.push(if !value.is_zero() { Some(created_address) } else { None });
        self.record_create_observation(value, caller, created_address);
        #[cfg(test)]
        CREATE_GATE_SNAPSHOT.with(|s| {
            s.set((self.create_recipients.len(), self.checkpoints.len(), true, derivation_ran))
        });
        // A production build has no gate snapshot to consume the flag, and the derivation's
        // observable effect is the returned address, so the flag is deliberately inert there.
        #[cfg(not(test))]
        let _ = derivation_ran;
        None
    }

    fn create_end(
        &mut self,
        _context: &mut CTX,
        _inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        if !self.active {
            return;
        }
        let expected = self.create_recipients.pop();
        if !outcome.result.is_ok() {
            // (a) CREATE reverted — revoke the checkpoint
            self.pop_checkpoint(true);
        } else if outcome.address.is_none() {
            // (b) nonce overflow: is_ok() but no address — revoke
            self.pop_checkpoint(true);
        } else if let (Some(Some(e)), Some(a)) = (expected, outcome.address) {
            if e != a {
                // (c) Successful CREATE but pre-derived address differs from actual — fail closed.
                // Record the depth at which the mismatch was detected so finish_capture can surface
                // InvariantViolation { CreateAddressMismatch } instead of silently truncating.
                let create_index = self.create_recipients.len();
                self.address_mismatch_create_index = Some(create_index);
                self.pop_checkpoint(true);
            } else {
                self.pop_checkpoint(false);
            }
        } else {
            // No endowment was recorded for this frame (expected was None) — commit
            self.pop_checkpoint(false);
        }
        #[cfg(test)]
        CREATE_END_BALANCE.with(|s| s.set((self.create_recipients.len(), self.checkpoints.len())));
        // Record the REAL completion the EVM handed us (plus the sticky observation count at this
        // instant, which must never have been decremented by the revocation above) so tests assert
        // completion validity directly rather than inferring it from the flattened stream.
        #[cfg(test)]
        CREATE_END_OUTCOME.with(|s| {
            s.set((
                outcome.result.is_ok(),
                outcome.address.is_none(),
                self.observed_count,
                outcome.address,
            ))
        });
    }

    fn step(&mut self, interp: &mut Interpreter<INTR>, context: &mut CTX) {
        if interp.bytecode.opcode() != opcode::SELFDESTRUCT {
            return;
        }
        // Record that this opcode's `step` was reached at all, before the active gate, so a
        // reader can tell "never hit" (count stays 0) from "hit while inactive" (count > 0,
        // active_branch_ran == false). No effect in production builds (`#[cfg(test)]`).
        #[cfg(test)]
        SELFDESTRUCT_GATE_SNAPSHOT.with(|s| {
            let (hits, active_ran, journal_ran) = s.get();
            s.set((hits + 1, active_ran, journal_ran));
        });
        // Inactive windows are strict no-ops: no journal read, no field write (gasless pre-checks
        // and other non-capturing simulations must never pay for or observe this).
        if !self.active {
            return;
        }
        #[cfg(test)]
        SELFDESTRUCT_GATE_SNAPSHOT.with(|s| {
            let (hits, _, journal_ran) = s.get();
            s.set((hits, true, journal_ran));
        });
        #[cfg(test)]
        SELFDESTRUCT_GATE_SNAPSHOT.with(|s| {
            let (hits, active_ran, _) = s.get();
            s.set((hits, active_ran, true));
        });
        self.selfdestruct_journal_mark = Some(context.journal_ref().journal().len());
    }

    fn step_end(&mut self, _interp: &mut Interpreter<INTR>, context: &mut CTX) {
        if !self.active {
            return;
        }
        if let Some(pre) = self.selfdestruct_journal_mark.take() {
            self.selfdestruct_wrote_journal = context.journal_ref().journal().len() > pre;
        }
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        // Inactive windows are strict no-ops: do not even consume the flag, or a stale flag could
        // be silently absorbed here and lost to the next armed window.
        if !self.active {
            return;
        }
        // Pinned revm populates this callback from `journal().last()`, keyed only on the
        // interpreter result. A Cancun existing-contract SELFDESTRUCT-to-self writes NO journal
        // entry, so `last()` is then a previous, unrelated entry — e.g. the enclosing CALL's own
        // balance transfer — which this callback would replay as a second native transfer. Accept
        // the payload only when THIS opcode actually appended an entry. Structural, not
        // content-based: two legitimate identical transfers each push their own entry and are both
        // still observed.
        if !core::mem::take(&mut self.selfdestruct_wrote_journal) {
            return;
        }
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
    fn inactive_inspector_push_pop_checkpoint_and_create_recipients_stay_clean() {
        // push_checkpoint / pop_checkpoint / create_recipients.push are all gated on self.active.
        // An inspector that was never started must leave all internal state untouched.
        let mut inspector = RcsInspector::new(10);
        inspector.push_checkpoint();
        inspector.push_checkpoint();
        inspector.push_checkpoint();
        assert!(inspector.checkpoints.is_empty(), "inactive push_checkpoint must not add entries");
        inspector.pop_checkpoint(false);
        inspector.pop_checkpoint(true);
        assert!(
            inspector.checkpoints.is_empty(),
            "inactive pop_checkpoint must not modify anything"
        );
        assert!(
            inspector.create_recipients.is_empty(),
            "inactive inspector must leave create_recipients untouched"
        );
        assert!(inspector.queue.is_empty(), "inactive inspector must leave queue untouched");
        assert_eq!(
            inspector.observed_count, 0,
            "inactive inspector must leave observed_count at 0"
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

    /// Asserts the inspector is fully reset the instant a budget-overflow finish returns — not only
    /// after the next `start_capture`.
    fn assert_capture_state_is_clean(inspector: &RcsInspector) {
        assert!(!inspector.active, "inspector must be inactive after finish");
        assert!(inspector.queue.is_empty(), "queue must be cleared on the failing exit");
        assert!(
            inspector.checkpoints.is_empty(),
            "checkpoints must be cleared on the failing exit"
        );
        assert_eq!(inspector.observed_count, 0, "observed_count must be reset on the failing exit");
        assert!(!inspector.overflowed, "overflow flag must be reset on the failing exit");
        assert!(
            inspector.create_recipients.is_empty(),
            "create_recipients must be cleared on the failing exit"
        );
        assert_eq!(
            inspector.selfdestruct_journal_mark, None,
            "selfdestruct_journal_mark must be cleared on the failing exit"
        );
        assert!(
            !inspector.selfdestruct_wrote_journal,
            "selfdestruct_wrote_journal must be cleared on the failing exit"
        );
    }

    // ── T1a: finish_capture resets all state (includes create_recipients) on LimitExceeded ──────────
    #[test]
    fn finish_capture_clears_state_on_limit_exceeded_exit() {
        // A `LimitExceeded` finish must leave the inspector fully reset IMMEDIATELY (spec §5.2, R9
        // item 4): the earlier implementation returned on this path before draining the queue.
        let mut inspector = RcsInspector::new(1);
        inspector.start_capture();
        inspector.push_checkpoint();
        observe_n(&mut inspector, NativeTransferKind::Call, 2);
        // Drive the SELFDESTRUCT-watermark fields to a non-initial state first, so the clearing
        // assertion below is not vacuous.
        inspector.selfdestruct_journal_mark = Some(7);
        inspector.selfdestruct_wrote_journal = true;
        assert!(matches!(
            inspector.finish_capture(&[]),
            CaptureOutcome::LimitExceeded { attempted: 2, limit: 1 }
        ));
        assert_capture_state_is_clean(&inspector);
    }

    #[test]
    fn finish_capture_clears_state_on_invariant_exit() {
        // An `InvariantViolation` (RealLog mismatch) finish must ALSO leave the inspector fully reset
        // immediately, never leaking the captured queue into the next transaction (R9 item 4).
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.push_checkpoint();
        inspector.record_real_log(real_log(1));
        // Drive the SELFDESTRUCT-watermark fields to a non-initial state first, so the clearing
        // assertion below is not vacuous.
        inspector.selfdestruct_journal_mark = Some(7);
        inspector.selfdestruct_wrote_journal = true;
        // `result.logs()` is empty while one real log was captured → RealLogMismatch.
        assert!(matches!(
            inspector.finish_capture(&[]),
            CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::RealLogMismatch { observed: 1, expected: 0, .. }
            }
        ));
        assert_capture_state_is_clean(&inspector);
    }

    #[test]
    fn new_selfdestruct_state_cleared_on_every_finish_exit_and_abort() {
        // Both new fields are driven to a NON-initial state first (so the clearing
        // assertion is not vacuous), then cleared on every exit: Complete, LimitExceeded,
        // InvariantViolation, and abort_capture.

        // Complete.
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.selfdestruct_journal_mark = Some(7);
        inspector.selfdestruct_wrote_journal = true;
        assert_eq!(inspector.selfdestruct_journal_mark, Some(7));
        assert!(inspector.selfdestruct_wrote_journal);
        assert!(matches!(inspector.finish_capture(&[]), CaptureOutcome::Complete(_)));
        assert_capture_state_is_clean(&inspector);

        // LimitExceeded.
        let mut inspector = RcsInspector::new(1);
        inspector.start_capture();
        inspector.push_checkpoint();
        observe_n(&mut inspector, NativeTransferKind::Call, 2);
        inspector.selfdestruct_journal_mark = Some(7);
        inspector.selfdestruct_wrote_journal = true;
        assert_eq!(inspector.selfdestruct_journal_mark, Some(7));
        assert!(inspector.selfdestruct_wrote_journal);
        assert!(matches!(
            inspector.finish_capture(&[]),
            CaptureOutcome::LimitExceeded { attempted: 2, limit: 1 }
        ));
        assert_capture_state_is_clean(&inspector);

        // InvariantViolation.
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.push_checkpoint();
        inspector.record_real_log(real_log(1));
        inspector.selfdestruct_journal_mark = Some(7);
        inspector.selfdestruct_wrote_journal = true;
        assert_eq!(inspector.selfdestruct_journal_mark, Some(7));
        assert!(inspector.selfdestruct_wrote_journal);
        assert!(matches!(inspector.finish_capture(&[]), CaptureOutcome::InvariantViolation { .. }));
        assert_capture_state_is_clean(&inspector);

        // abort_capture.
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.selfdestruct_journal_mark = Some(7);
        inspector.selfdestruct_wrote_journal = true;
        assert_eq!(inspector.selfdestruct_journal_mark, Some(7));
        assert!(inspector.selfdestruct_wrote_journal);
        inspector.abort_capture();
        assert_capture_state_is_clean(&inspector);
    }

    #[test]
    fn unconsumed_selfdestruct_flag_does_not_cross_windows() {
        // A SELFDESTRUCT that exits abnormally so the `selfdestruct` callback never fires
        // leaves `selfdestruct_wrote_journal` set; the NEXT window must not inherit it, and a
        // following ordinary (non-SELFDESTRUCT) transfer in that window must not be mis-attributed
        // or doubled.
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.selfdestruct_wrote_journal = true; // simulates the abnormal-exit leak
        let _ = inspector.finish_capture(&[]);
        assert_capture_state_is_clean(&inspector);

        inspector.start_capture();
        assert!(!inspector.selfdestruct_wrote_journal, "the next window must start clean");
        inspector.observe_native_transfer(
            addr(1),
            addr(2),
            U256::from(1u64),
            NativeTransferKind::Call,
        );
        match inspector.finish_capture(&[]) {
            CaptureOutcome::Complete(logs) => {
                assert_eq!(
                    logs.len(),
                    1,
                    "the ordinary transfer must not be mis-attributed or doubled"
                )
            }
            other => panic!("expected Complete with one transfer, got {other:?}"),
        }
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
        // transfer_value=None (DELEGATECALL/STATICCALL apparent value) generates no event.
        let mut i = armed(10);
        i.record_call_observation(None, addr(1), addr(2));
        assert_eq!(i.observed_count, 0);
    }

    #[test]
    fn call_internal_nonzero_transfer_is_observed() {
        let mut i = armed(10);
        i.record_call_observation(Some(U256::from(5u64)), addr(1), addr(2));
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
        i.record_call_observation(None, addr(1), addr(2));
        assert_eq!(i.observed_count, 0);
    }

    #[test]
    fn call_self_transfer_generates_no_event() {
        // CALLCODE reports a transfer value but caller == target, so it is a self-transfer.
        let mut i = armed(10);
        i.record_call_observation(Some(U256::from(5u64)), addr(1), addr(1));
        assert_eq!(i.observed_count, 0);
    }

    #[test]
    fn create_top_level_generates_no_event() {
        // Zero endowment generates no event regardless of depth.
        let mut i = armed(10);
        i.record_create_observation(U256::ZERO, addr(1), addr(2));
        assert_eq!(i.observed_count, 0);
    }

    #[test]
    fn create_internal_endowment_is_observed_to_created_address() {
        let mut i = armed(10);
        let created = addr(9);
        i.record_create_observation(U256::from(7u64), addr(1), created);
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
        i.record_create_observation(U256::ZERO, addr(1), addr(2));
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

/// Runtime test fixture for the builder capture path. Kept `pub(crate)` so test 33 (spec §8, in this
/// file) asserts a REAL `evm.transact`-driven `RealLogMismatch` outcome — one runtime source, not a
/// re-implementation. NOTE: the loop-level fail-closed test 35 in `context.rs` does NOT consume this
/// fixture; it drives the real candidate loop and forces the mismatch inside the loop's own
/// `RcsInspector::finish_capture` via the `FORCE_REALLOG_MISMATCH` seam.
#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;
    use alloy_consensus::{SignableTransaction, TxLegacy};
    use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
    use alloy_op_evm::{OpEvmFactory, OpTx};
    use alloy_primitives::{Signature, TxKind};
    use op_revm::OpSpecId;
    use revm::context::{BlockEnv, CfgEnv};
    use revm::database::InMemoryDB;
    use revm::state::{AccountInfo, Bytecode};

    /// Produces a REAL [`CaptureOutcome::InvariantViolation`] carrying a
    /// [`CaptureInvariantError::RealLogMismatch`] from a production `evm.transact` on an
    /// `OpEvm<_, RcsInspector, _>` — NOT a hand-constructed outcome. A contract emits one real `LOG0`
    /// (captured through the composite-forwarded `log` hook), then `finish_capture` runs against a
    /// deliberately tampered `result.logs()` slice so the RealLog cross-check diverges (observed 1 vs
    /// expected 2). This is the runtime source asserted by test 33 (the runtime `InvariantViolation`
    /// outcome). The loop-level test 35 in `context.rs` does NOT consume this fixture — it forces the
    /// mismatch through the real candidate loop's own inspector via `FORCE_REALLOG_MISMATCH`.
    pub(crate) fn real_runtime_reallogmismatch_outcome() -> CaptureOutcome {
        let caller = Address::from([0xAA; 20]);
        let contract = Address::from([0xC7; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo { balance: U256::from(1_000_000_000u64), ..Default::default() },
        );
        // Runtime bytecode: `LOG0(0, 0); STOP` — emits one real (empty) log then returns successfully.
        db.insert_account_info(
            contract,
            AccountInfo {
                code: Some(Bytecode::new_raw(Bytes::from(vec![
                    0x60, 0x00, 0x60, 0x00, 0xa0, 0x00,
                ]))),
                ..Default::default()
            },
        );
        let tx = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Call(contract),
            value: U256::ZERO,
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        let op_tx = OpTx::from_recovered_tx(&tx, caller);
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(op_tx).expect("tx executes");
        assert!(result.result.is_success(), "the emitting tx itself succeeds");
        assert_eq!(result.result.logs().len(), 1, "one real log persisted");
        // Force divergence at runtime: append a fabricated log so the captured RealLog subsequence
        // (1) no longer matches the claimed `result.logs()` (2).
        let mut tampered = result.result.logs().to_vec();
        tampered.push(native_transfer_log(caller, NATIVE_ASSET_ADDRESS, U256::from(1u64)));
        evm.components_mut().1.finish_capture(&tampered)
    }
}

/// Production-shaped GATING runtime integration test (design §8, R9 item 2). Builds a real
/// `alloy_op_evm::OpEvm<_, RcsInspector, _>` — the `RcsInspector` is the inner inspector of the
/// baked-in `PostExecCompositeInspector` — via the same factory the flashblocks path uses, and
/// executes every candidate through the production entry point **`evm.transact`** (matching
/// `context.rs`'s candidate simulation) — no `transact_raw` is used; the runtime-forced-mismatch case
/// drives a real `evm.transact` and then compares against a deliberately tampered `result.logs()`
/// slice. It asserts capture happens THROUGH the composite wrapper at runtime — the "inner implements
/// the hook, the wrapper must forward it" property (§4/§5.3) that a helper-level unit test cannot
/// prove — plus CREATE2 endowment, parent-frame revert rollback, a runtime-forced real-log mismatch,
/// and that the composite's own SDM/post-exec warming is preserved. A green CI pipeline does NOT
/// replace this gate.
#[cfg(test)]
mod gating_integration_tests {
    use super::*;
    use alloy_consensus::{SignableTransaction, TxEip7702, TxLegacy};
    use alloy_eips::eip7702::Authorization;
    use alloy_evm::{Evm, EvmEnv, EvmFactory, FromRecoveredTx};
    use alloy_op_evm::{post_exec, OpEvmFactory, OpTx};
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

    /// The shared prologue for a `CALL(gas, dst, value, 0, 0, 0, 0)`: pushes the four zero
    /// ret/args words, the `value`, and `PUSH20 dst`, leaving the stack ready for `GAS; CALL`.
    /// Extracted so the hand-written CALL fixtures below don't each repeat the opcode prefix.
    fn call_prefix(value: u8, dst: Address) -> Vec<u8> {
        // PUSH1 0 (retSize) PUSH1 0 (retOffset) PUSH1 0 (argsSize) PUSH1 0 (argsOffset)
        // PUSH1 value       PUSH20 dst
        let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, value, 0x73];
        code.extend_from_slice(dst.as_slice());
        code
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
    /// production `evm.transact` → `finish_capture(result.logs())`) mirrors the builder's real path;
    /// `transact` is the same entry point `context.rs` uses (R9 decision), so the gate exercises the
    /// production `PostExecCompositeInspector` forwarding and gasless/base-fee handling.
    /// `OpSpecId::JOVIAN` maps through op-revm to `SpecId::PRAGUE`, enabling EIP-7702 authority-tuple
    /// resolution. Tests that exercise DELEGATECALL attribution with delegated code depend on the
    /// JOVIAN→PRAGUE mapping being in effect for the OpEvm spec selection.
    fn run_capturing_tx(db: InMemoryDB, target: Address, budget: usize) -> (CaptureOutcome, bool) {
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(budget),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(call_tx(target)).expect("tx executes");
        let success = result.result.is_success();
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());
        (outcome, success)
    }

    /// Runtime bytecode: `CALL(gas, dst, value=1, 0, 0, 0, 0); STOP` — one internal 1-wei transfer.
    fn call_value_1_to(dst: Address) -> Vec<u8> {
        let mut code = call_prefix(0x01, dst);
        code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
        code
    }

    /// Runtime bytecode: `DELEGATECALL(gas, dst, 0, 0, 0, 0); POP; STOP`.
    /// The caller delegates execution to `dst`; any CALL/CREATE inside `dst`'s code
    /// runs with the delegating address as the execution context.
    fn delegatecall_to_then_stop(dst: Address) -> Vec<u8> {
        // Stack order for DELEGATECALL (top first): gas, addr, argsOff, argsSize, retOff, retSize
        // Push in reverse: retSize, retOff, argsSize, argsOff, addr, gas
        let mut code = vec![
            0x60, 0x00, // PUSH1 0  (retSize)
            0x60, 0x00, // PUSH1 0  (retOffset)
            0x60, 0x00, // PUSH1 0  (argsSize)
            0x60, 0x00, // PUSH1 0  (argsOffset)
            0x73, // PUSH20
        ];
        code.extend_from_slice(dst.as_slice());
        code.extend_from_slice(&[
            0x62, 0x07, 0xA1, 0x20, // PUSH3 500000 (gas)
            0xF4, // DELEGATECALL
            0x50, // POP
            0x00, // STOP
        ]);
        code
    }

    /// Runtime bytecode intended to run inside a DELEGATECALL context.
    /// When invoked, this code:
    ///   1. CALL `call_dst` with 1 wei
    ///   2. CREATE with 1 wei and empty init code
    ///   3. STOP
    ///
    /// Both operations attribute to the delegating address (execution context).
    fn delegated_call_and_create(call_dst: Address) -> Vec<u8> {
        // CALL: retSize=0, retOffset=0, argsSize=0, argsOffset=0, value=1, addr=call_dst, gas
        let mut code = call_prefix(1, call_dst);
        // GAS; CALL; POP
        code.extend_from_slice(&[0x5A, 0xF1, 0x50]);
        // CREATE(value=1, offset=0, size=0): PUSH1 0 (size) PUSH1 0 (offset) PUSH1 1 (value) CREATE
        code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0xF0]);
        // POP; STOP
        code.extend_from_slice(&[0x50, 0x00]);
        code
    }

    #[test]
    fn internal_call_transfer_captured_through_composite() {
        // caller → contract (value 0); contract makes an internal CALL transferring 1 wei to dst.
        // Proves the composite forwards `call` to the inner RcsInspector at runtime, and that
        // `transfer_value`/`transfer_from`/`transfer_to` are read correctly. The top-level
        // caller→contract call carries zero transfer value so it generates no event.
        let contract = Address::from([0xCC; 20]);
        let dst = Address::from([0xDD; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(contract, account(1_000, Some(call_value_1_to(dst))));

        let (outcome, success) = run_capturing_tx(db, contract, 16);
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
        // A CREATE with a non-zero endowment yields one native Transfer from the creator to
        // the created address (recipient derivation runs through the composite-forwarded `create`).
        // The factory is pre-inserted with nonce=0; CREATE at nonce=0 produces a deterministic addr.
        let factory = Address::from([0xF1; 20]);
        let expected_created = factory.create(0);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(factory, account(1_000, Some(create_endowment_7())));

        let (outcome, success) = run_capturing_tx(db, factory, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "one CREATE endowment transfer expected");
                assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
                assert_eq!(stream[0].topics()[1], factory.into_word());
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(7u64).to_be_bytes::<32>());
                // Recipient (topic2) must be the exact CREATE-derived address.
                assert_eq!(
                    stream[0].topics()[2],
                    expected_created.into_word(),
                    "recipient must be the exact nonce-derived CREATE address"
                );
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

        let (outcome, success) = run_capturing_tx(db, victim, 16);
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

    /// Runtime bytecode: `ADDRESS; SELFDESTRUCT` — a Cancun existing-contract SELFDESTRUCT whose
    /// beneficiary is itself. On the pinned Cancun-or-later branch this writes NO journal entry.
    fn address_then_selfdestruct() -> Vec<u8> {
        vec![0x30, 0xff] // ADDRESS SELFDESTRUCT
    }

    /// Runtime bytecode: `STOP` — the control body for the SELFDESTRUCT-to-self fixtures.
    fn stop_only() -> Vec<u8> {
        vec![0x00] // STOP
    }

    /// Runtime bytecode: two sequential `CALL(gas, dst, value=1, 0,0,0,0)` to the same `dst`,
    /// each with its own `GAS; CALL; POP` so the second call's args sit above the first's leftover
    /// return value. Used to prove the fix is not content-based: two legitimate identical transfers.
    fn call_value_1_twice_to(dst: Address) -> Vec<u8> {
        let mut code = call_prefix(0x01, dst);
        code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
        code.extend_from_slice(&call_prefix(0x01, dst));
        code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
        code
    }

    /// Runtime bytecode: `PUSH1 1; PUSH1 0; SSTORE` prologue, then `CALL(gas, dst, value=1, …); STOP`
    /// — an unrelated journal-writing opcode immediately before a value-bearing internal CALL.
    fn sstore_then_call_value_1_to(dst: Address) -> Vec<u8> {
        let mut code = vec![0x60, 0x01, 0x60, 0x00, 0x55]; // PUSH1 1; PUSH1 0; SSTORE
        code.extend_from_slice(&call_value_1_to(dst));
        code
    }

    /// Runtime bytecode: `CALL(value=1, victim)`, then `SSTORE`, then `CALL(value=0, victim2); STOP`.
    /// Used to prove a journal-writing opcode earlier in the SAME window does not arm a later,
    /// unrelated, no-write `SELFDESTRUCT`.
    fn call_1_then_sstore_then_call_0_to(victim: Address, victim2: Address) -> Vec<u8> {
        let mut code = call_prefix(0x01, victim);
        code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
        code.extend_from_slice(&[0x60, 0x01, 0x60, 0x00, 0x55]); // PUSH1 1; PUSH1 0; SSTORE
        code.extend_from_slice(&call_prefix(0x00, victim2));
        code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
        code
    }

    #[test]
    fn existing_contract_selfdestruct_to_self_is_not_captured() {
        // Cancun existing-contract SELFDESTRUCT-to-self writes NO journal entry. A stale-callback
        // replay bug would double-count the enclosing CALL's own transfer here.
        let entry = Address::from([0x51; 20]);
        let victim = Address::from([0x52; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
        db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

        let (outcome, success) = run_capturing_tx(db, entry, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(
                    stream.len(),
                    1,
                    "expected exactly the enclosing CALL's transfer, no replay from SELFDESTRUCT"
                );
                assert_eq!(stream[0].topics()[1], entry.into_word());
                assert_eq!(stream[0].topics()[2], victim.into_word());
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(1u64).to_be_bytes::<32>());
            }
            other => panic!("expected Complete with exactly one transfer, got {other:?}"),
        }
    }

    #[test]
    fn stop_control_matches_selfdestruct_to_self_capture_count() {
        // Control for the test above: the victim just STOPs instead of self-destructing. The
        // identical single-transfer count isolates SELFDESTRUCT-to-self as the only variable.
        let entry = Address::from([0x51; 20]);
        let victim = Address::from([0x52; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
        db.insert_account_info(victim, account(0, Some(stop_only())));

        let (outcome, success) = run_capturing_tx(db, entry, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1);
                assert_eq!(stream[0].topics()[1], entry.into_word());
                assert_eq!(stream[0].topics()[2], victim.into_word());
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(1u64).to_be_bytes::<32>());
            }
            other => panic!("expected Complete with exactly one transfer, got {other:?}"),
        }
    }

    #[test]
    fn selfdestruct_to_self_consumes_no_native_budget() {
        // Sharpest discriminator: at budget 1, a stale replay would double-count and overflow to
        // LimitExceeded instead of completing.
        let entry = Address::from([0x51; 20]);
        let victim = Address::from([0x52; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
        db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

        let (outcome, success) = run_capturing_tx(db, entry, 1);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => assert_eq!(stream.len(), 1),
            other => panic!("expected Complete (budget 1 is not exceeded), got {other:?}"),
        }
    }

    #[test]
    fn stop_control_at_budget_1_completes() {
        // Control for the test above: proves budget 1 is genuinely at the limit, so the previous
        // test's pass is not slack from an oversized budget.
        let entry = Address::from([0x51; 20]);
        let victim = Address::from([0x52; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
        db.insert_account_info(victim, account(0, Some(stop_only())));

        let (outcome, success) = run_capturing_tx(db, entry, 1);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => assert_eq!(stream.len(), 1),
            other => panic!("expected Complete at budget 1, got {other:?}"),
        }
    }

    #[test]
    fn existing_contract_selfdestruct_to_beneficiary_still_captured() {
        // Regression guard: an existing contract that SELFDESTRUCTs to a DIFFERENT, non-zero-balance
        // beneficiary must still be observed exactly once — guards against over-suppression.
        let victim = Address::from([0x55; 20]);
        let beneficiary = Address::from([0x56; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(victim, account(500, Some(selfdestruct_to(beneficiary))));

        let (outcome, success) = run_capturing_tx(db, victim, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "genuine SELFDESTRUCT transfer must still be captured");
                assert_eq!(stream[0].topics()[1], victim.into_word());
                assert_eq!(stream[0].topics()[2], beneficiary.into_word());
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(500u64).to_be_bytes::<32>());
            }
            other => panic!("expected Complete with one SELFDESTRUCT transfer, got {other:?}"),
        }
    }

    #[test]
    fn two_identical_native_transfers_are_both_observed() {
        // The fix must be structural, not content-based. Two legitimate identical transfers in
        // one transaction must both still be observed.
        let entry = Address::from([0x57; 20]);
        let dst = Address::from([0x58; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_twice_to(dst))));

        let (outcome, success) = run_capturing_tx(db, entry, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 2, "two identical transfers must both be observed");
                assert_eq!(stream[0].topics(), stream[1].topics());
                assert_eq!(stream[0].data.data, stream[1].data.data);
            }
            other => panic!("expected Complete with two transfers, got {other:?}"),
        }
    }

    #[test]
    fn nested_selfdestruct_to_self_after_unrelated_journal_entry_is_not_captured() {
        // The stale entry can come from anywhere earlier in the (flat, transaction-wide) journal,
        // not just the immediately enclosing frame. A shape two frames deep with an intervening
        // SSTORE between the value-bearing CALL and the SELFDESTRUCT.
        let entry = Address::from([0x61; 20]);
        let mid = Address::from([0x62; 20]);
        let victim = Address::from([0x63; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to(mid))));
        db.insert_account_info(mid, account(10, Some(sstore_then_call_value_1_to(victim))));
        db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

        let (outcome, success) = run_capturing_tx(db, entry, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(
                    stream.len(),
                    2,
                    "expected only the two real value-bearing CALLs, no SELFDESTRUCT replay"
                );
            }
            other => panic!("expected Complete with exactly two transfers, got {other:?}"),
        }
    }

    #[test]
    fn journal_writing_opcode_does_not_arm_a_later_selfdestruct() {
        // An earlier journal-writing opcode (SSTORE) in the SAME window must not leak into a
        // later, unrelated, no-write SELFDESTRUCT. The watermark is taken per-opcode, not merely
        // "did the journal grow at some point in this window".
        let entry = Address::from([0x64; 20]);
        let victim = Address::from([0x65; 20]);
        let victim2 = Address::from([0x66; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(
            entry,
            account(10, Some(call_1_then_sstore_then_call_0_to(victim, victim2))),
        );
        db.insert_account_info(victim2, account(0, Some(address_then_selfdestruct())));

        let (outcome, success) = run_capturing_tx(db, entry, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(
                    stream.len(),
                    1,
                    "only the real value-bearing CALL should be observed; the SSTORE must not \
                     arm the later no-write SELFDESTRUCT"
                );
            }
            other => panic!("expected Complete with exactly one transfer, got {other:?}"),
        }
    }

    /// Runtime bytecode: two sequential `CALL(gas, dst, value=1, 0,0,0,0)` to two DIFFERENT
    /// destinations, each with its own `GAS; CALL; POP`.
    fn call_value_1_to_two(a: Address, b: Address) -> Vec<u8> {
        let mut code = call_prefix(0x01, a);
        code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
        code.extend_from_slice(&call_prefix(0x01, b));
        code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
        code
    }

    /// A `quota` audit rule matching `Transfer(address,address,uint256)` on
    /// [`NATIVE_ASSET_ADDRESS`], for driving a real capture stream through `rcs-filter` screening.
    fn quota_native_transfer_rule() -> rcs_filter::rules::RuleSet {
        let json = format!(
            r#"{{
              "id": "quota-native-transfer",
              "event_abis": {{
                "transfer": {{
                  "type": "event", "name": "Transfer",
                  "inputs": [
                    {{ "name": "from", "type": "address", "indexed": true }},
                    {{ "name": "to", "type": "address", "indexed": true }},
                    {{ "name": "value", "type": "uint256", "indexed": false }}
                  ],
                  "anonymous": false
                }}
              }},
              "audit_types": ["quota"],
              "condition": {{ "==": [{{ "var": "transfer.address" }}, "{native:#x}"] }},
              "action": "audit",
              "audit_timeout_action": "allow"
            }}"#,
            native = NATIVE_ASSET_ADDRESS
        );
        let raw: rcs_filter::rules::RawRule = serde_json::from_str(&json).expect("valid rule json");
        rcs_filter::rules::load_rules(1, 0, vec![raw])
    }

    #[test]
    fn selfdestruct_to_self_quota_actions_report_one_transfer() {
        // Third control dimension: drive the real capture stream through rcs-filter screening and
        // assert exactly one quota ActionItem, not two.
        let entry = Address::from([0x51; 20]);
        let victim = Address::from([0x52; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
        db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

        let (outcome, success) = run_capturing_tx(db, entry, 16);
        assert!(success, "top-level tx should succeed");
        let stream = match outcome {
            CaptureOutcome::Complete(stream) => stream,
            other => panic!("expected Complete, got {other:?}"),
        };

        let rules = quota_native_transfer_rule();
        let input = rcs_filter::handle::ScreenInput {
            tx_hash: B256::ZERO,
            origin: entry,
            tx_to: Some(victim),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &stream,
        };
        match rcs_filter::matching::evaluate(&rules, &input) {
            rcs_filter::matching::MatchOutcome::Audit { actions, .. } => {
                let quota = actions.get("quota").expect("quota audit_type key present");
                assert_eq!(quota.len(), 1, "expected exactly one quota action, not a replay");
                assert_eq!(quota[0].params.get("value").and_then(|v| v.as_str()), Some("1"));
            }
            other => panic!("expected Audit with quota actions, got {other:?}"),
        }
    }

    #[test]
    fn stop_control_quota_actions_report_one_transfer() {
        // Control for the test above: identical single ActionItem.
        let entry = Address::from([0x51; 20]);
        let victim = Address::from([0x52; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
        db.insert_account_info(victim, account(0, Some(stop_only())));

        let (outcome, success) = run_capturing_tx(db, entry, 16);
        assert!(success, "top-level tx should succeed");
        let stream = match outcome {
            CaptureOutcome::Complete(stream) => stream,
            other => panic!("expected Complete, got {other:?}"),
        };

        let rules = quota_native_transfer_rule();
        let input = rcs_filter::handle::ScreenInput {
            tx_hash: B256::ZERO,
            origin: entry,
            tx_to: Some(victim),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &stream,
        };
        match rcs_filter::matching::evaluate(&rules, &input) {
            rcs_filter::matching::MatchOutcome::Audit { actions, .. } => {
                let quota = actions.get("quota").expect("quota audit_type key present");
                assert_eq!(quota.len(), 1);
                assert_eq!(quota[0].params.get("value").and_then(|v| v.as_str()), Some("1"));
            }
            other => panic!("expected Audit with quota actions, got {other:?}"),
        }
    }

    #[test]
    fn genuine_and_self_targeted_selfdestructs_in_one_tx_report_only_the_genuine() {
        // A genuine SELFDESTRUCT and a to-self SELFDESTRUCT in the same transaction, in both call
        // orders. An over-broad guard that suppressed the genuine one, or a flag that carried from
        // the first SELFDESTRUCT to the second, would under- or over-report here.
        let entry = Address::from([0x71; 20]);
        let victim_a = Address::from([0x72; 20]); // genuine: selfdestructs to `beneficiary`
        let victim_b = Address::from([0x73; 20]); // to-self: contributes nothing
        let beneficiary = Address::from([0x74; 20]);

        let run = |first: Address, second: Address| -> Vec<Log> {
            let mut db = InMemoryDB::default();
            db.insert_account_info(caller(), account(1_000_000_000, None));
            db.insert_account_info(entry, account(10, Some(call_value_1_to_two(first, second))));
            db.insert_account_info(victim_a, account(0, Some(selfdestruct_to(beneficiary))));
            db.insert_account_info(victim_b, account(0, Some(address_then_selfdestruct())));

            let (outcome, success) = run_capturing_tx(db, entry, 16);
            assert!(success, "top-level tx should succeed");
            match outcome {
                CaptureOutcome::Complete(stream) => stream,
                other => panic!("expected Complete, got {other:?}"),
            }
        };

        for (first, second, label) in
            [(victim_a, victim_b, "genuine-then-self"), (victim_b, victim_a, "self-then-genuine")]
        {
            let stream = run(first, second);
            assert_eq!(
                stream.len(),
                3,
                "{label}: expected the two CALL transfers plus the one genuine SELFDESTRUCT transfer"
            );
            let genuine =
                stream.iter().filter(|log| log.topics()[2] == beneficiary.into_word()).count();
            assert_eq!(genuine, 1, "{label}: exactly one transfer must reach the beneficiary");
        }
    }

    #[test]
    fn inactive_inspector_selfdestruct_leaves_new_state_untouched() {
        // A real, NON-capturing OpEvm run (never armed) executing a SELFDESTRUCT-to-self
        // must leave the new watermark fields at their initial values and must never read the
        // journal for the watermark — the seam proves the hook was reached but the active branch
        // never ran, distinguishing "never hit" from "hit while inactive".
        reset_selfdestruct_gate_seams();
        let entry = Address::from([0x51; 20]);
        let victim = Address::from([0x52; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
        db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        // Deliberately never call start_capture.
        let result = evm.transact(call_tx(entry)).expect("tx executes");
        assert!(result.result.is_success());

        let inspector = evm.components_mut().1;
        assert_eq!(inspector.selfdestruct_journal_mark, None);
        assert!(!inspector.selfdestruct_wrote_journal);

        let (hits, active_ran, journal_ran) = SELFDESTRUCT_GATE_SNAPSHOT.with(|s| s.get());
        assert!(hits > 0, "the step hook must still be reached for the SELFDESTRUCT opcode");
        assert!(!active_ran, "the active branch must not run while inactive");
        assert!(!journal_ran, "the watermark journal read must never execute while inactive");
    }

    #[test]
    fn inactive_inspector_selfdestruct_normal_transfer_leaves_new_state_untouched() {
        // The to-self shape above writes NO journal entry, so on field values alone it would
        // pass equally against an implementation whose step/step_end gates were removed (an
        // ungated step/step_end would measure a zero delta either way). This is the
        // discriminating shape: a normal transfer (existing contract, beneficiary != victim,
        // balance > 0) DOES write a journal entry, so an ungated step/step_end would leave
        // `selfdestruct_wrote_journal == true` here -- which is exactly what this test rules
        // out. This pins the specific regression where step/step_end lose their inactive gate
        // while the selfdestruct hook's own gate is retained; it is not a claim that any gate
        // failure in general leaves the flag true.
        reset_selfdestruct_gate_seams();
        let victim = Address::from([0x81; 20]);
        let beneficiary = Address::from([0x82; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(victim, account(500, Some(selfdestruct_to(beneficiary))));

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        // Deliberately never call start_capture.
        let result = evm.transact(call_tx(victim)).expect("tx executes");
        assert!(result.result.is_success());

        let inspector = evm.components_mut().1;
        // Read BEFORE any start_capture/finish_capture, so a reset cannot stand in for the gate.
        assert_eq!(inspector.selfdestruct_journal_mark, None);
        assert!(!inspector.selfdestruct_wrote_journal);

        let (hits, active_ran, journal_ran) = SELFDESTRUCT_GATE_SNAPSHOT.with(|s| s.get());
        assert!(hits > 0, "the step hook must still be reached for the SELFDESTRUCT opcode");
        assert!(!active_ran, "the active branch must not run while inactive");
        assert!(!journal_ran, "the watermark journal read must never execute while inactive");
    }

    #[test]
    fn inactive_then_armed_inspector_still_captures() {
        // The SAME inspector instance that just ran inactive must still capture correctly
        // once armed — proves the inactive gate does not poison a later window.
        let entry = Address::from([0x51; 20]);
        let victim = Address::from([0x52; 20]);
        let second_caller = Address::from([0xBB; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(second_caller, account(1_000_000_000, None));
        db.insert_account_info(entry, account(10, Some(call_value_1_to(victim))));
        db.insert_account_info(victim, account(0, Some(address_then_selfdestruct())));

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        // First: run inactive (never start_capture).
        let result = evm.transact(call_tx_from(caller(), entry)).expect("tx executes");
        assert!(result.result.is_success());

        // Now arm the SAME inspector instance and run a second, independent tx from a different
        // sender (so nonce 0 is valid for both, per `call_tx_from`'s doc comment).
        evm.components_mut().1.start_capture();
        let result2 = evm.transact(call_tx_from(second_caller, entry)).expect("tx executes");
        assert!(result2.result.is_success());
        let outcome = evm.components_mut().1.finish_capture(result2.result.logs());
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "exactly one virtual log for the value-bearing CALL");
                assert_eq!(stream[0].topics()[1], entry.into_word());
                assert_eq!(stream[0].topics()[2], victim.into_word());
            }
            other => panic!("expected Complete with one transfer, got {other:?}"),
        }
    }

    /// Runtime bytecode: `CREATE(value, 0, len)` where the appended init code (copied into memory
    /// via `CODECOPY`) deploys a contract whose runtime body is [`address_then_selfdestruct`]
    /// (`PUSH2 0x30FF; PUSH1 0; MSTORE; PUSH1 2; PUSH1 30; RETURN`), then `CALL`s the precomputed
    /// deployment address with zero value to run that child's SELFDESTRUCT within the SAME
    /// transaction that created it.
    fn create_endowment_then_call_child(value: u8, child_addr: Address) -> Vec<u8> {
        let init_code: Vec<u8> =
            vec![0x61, 0x30, 0xff, 0x60, 0x00, 0x52, 0x60, 0x02, 0x60, 0x1e, 0xf3];
        let init_len = init_code.len() as u8;
        let mut head = vec![
            0x60, init_len, // PUSH1 init_len   (CODECOPY size)
            0x60, 0x00, // PUSH1 <codeoffset> -- patched below (CODECOPY offset)
            0x60, 0x00, // PUSH1 0          (CODECOPY destOffset)
            0x39, // CODECOPY
            0x60, init_len, // PUSH1 init_len   (CREATE size)
            0x60, 0x00, // PUSH1 0          (CREATE offset)
            0x60, value, // PUSH1 value      (CREATE value)
            0xf0,  // CREATE
            0x50,  // POP (discard the dynamic created address; we call a precomputed one)
        ];
        head.extend_from_slice(&call_prefix(0x00, child_addr)); // CALL(gas, child_addr, 0, 0,0,0,0)
        head.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
        let codeoffset = head.len() as u8;
        head[3] = codeoffset; // patch the PUSH1 <codeoffset> immediate
        head.extend_from_slice(&init_code);
        head
    }

    #[test]
    fn created_in_tx_selfdestruct_to_self_is_still_filtered_not_double_suppressed() {
        // A SELFDESTRUCT-to-self by a contract CREATED in this same transaction writes a REAL
        // AccountDestroyed journal entry (the flag is true), and the observation is dropped by
        // the pre-existing from==to filter -- not by the new guard.
        // Pins that the new guard did not become a second, over-broad suppression mechanism.
        let factory = Address::from([0x75; 20]);
        let value = 7u8;
        let child = factory.create(0);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(
            factory,
            account(10, Some(create_endowment_then_call_child(value, child))),
        );

        let (outcome, success) = run_capturing_tx(db, factory, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(
                    stream.len(),
                    1,
                    "only the CREATE endowment transfer; the created-in-tx self-destruct \
                     contributes nothing"
                );
                assert_eq!(stream[0].topics()[1], factory.into_word());
                assert_eq!(
                    stream[0].data.data.as_ref(),
                    &U256::from(value as u64).to_be_bytes::<32>()
                );
            }
            other => panic!(
                "expected Complete with exactly the CREATE endowment transfer, got {other:?}"
            ),
        }
    }

    /// Runtime bytecode: `CALL(gas, dst, value=0, 0,0,0,0); STOP` — a zero-value child call whose
    /// failure is ignored.
    fn call_value_0_to(dst: Address) -> Vec<u8> {
        let mut code = call_prefix(0x00, dst);
        code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
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

        let (outcome, success) = run_capturing_tx(db, parent, 16);
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
        let mut code = call_prefix(0x01, dst);
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

        let (outcome, success) = run_capturing_tx(db, contract, 16);
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
            code.extend_from_slice(&call_prefix(0x01, dst)); // ret/args, value 1, PUSH20 dst
            code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
        }
        code.push(0x00); // STOP
        code
    }

    #[test]
    fn inspector_state_does_not_cross_transactions_through_real_evm() {
        // Two candidate txs share ONE inspector (as in the real per-block loop), distinct senders so
        // neither depends on the other committing. tx1 makes TWO internal transfers against a budget
        // of 1, so it overflows. R10 item 6 / R9 item 4: `finish_capture` clears ALL capture state on
        // EVERY exit — including the `LimitExceeded` path — so the inspector is left clean IMMEDIATELY
        // when tx1 finishes; nothing residual is carried to the next `start_capture` for it to wipe.
        // tx2 therefore starts from an already-clean inspector regardless (start_capture's reset is
        // belt-and-suspenders). If the every-exit cleanup regressed, tx2 would inherit tx1's transfers
        // or be born overflowed and the asserts below would fail.
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

        // tx1 -> c1: two transfers over a budget of 1 ⇒ overflow (LimitExceeded).
        evm.components_mut().1.start_capture();
        let r1 = evm.transact(call_tx_from(sender_a, c1)).expect("tx1 executes");
        let o1 = evm.components_mut().1.finish_capture(r1.result.logs());
        assert!(
            matches!(o1, CaptureOutcome::LimitExceeded { .. }),
            "tx1 must overflow the native-transfer budget, got {o1:?}"
        );
        // R10 item 6: the `LimitExceeded` exit cleared ALL state IMMEDIATELY — assert the inspector is
        // clean right now, BEFORE tx2's `start_capture`, so cross-tx isolation does not depend on the
        // next reset. (Fields are visible: this test module is a descendant of `rcs_capture`.)
        {
            let insp = evm.components_mut().1;
            assert!(insp.queue.is_empty(), "queue cleared immediately on the LimitExceeded exit");
            assert!(insp.checkpoints.is_empty(), "checkpoints cleared immediately");
            assert_eq!(insp.observed_count, 0, "observed_count reset immediately");
            assert!(!insp.overflowed, "overflow flag reset immediately");
            assert!(!insp.active, "inspector inactive immediately after finish");
            assert!(
                insp.create_recipients.is_empty(),
                "create_recipients cleared immediately on the LimitExceeded exit"
            );
        }

        // tx2 -> c2 on the SAME (already-clean) inspector.
        evm.components_mut().1.start_capture();
        let r2 = evm.transact(call_tx_from(sender_b, c2)).expect("tx2 executes");
        let o2 = evm.components_mut().1.finish_capture(r2.result.logs());
        match o2 {
            CaptureOutcome::Complete(s) => {
                assert_eq!(
                    s.len(),
                    1,
                    "tx2 sees only its own transfer (state cleared on every exit)"
                );
                assert_eq!(s[0].topics()[2], d2.into_word(), "tx2's transfer targets d2 only");
            }
            other => {
                panic!("tx2 expected clean Complete(1) from an already-clean tx1, got {other:?}")
            }
        }
    }

    /// Runtime bytecode: `CREATE2(value=7, offset=0, size=0, salt=0); STOP` — a 7-wei endowment with
    /// empty init code. CREATE2 pops value(top), offset, size, salt, so the pushes are salt, size,
    /// offset, value.
    fn create2_endowment_7() -> Vec<u8> {
        // PUSH1 0 (salt) PUSH1 0 (size) PUSH1 0 (offset) PUSH1 7 (value) CREATE2 STOP
        vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x07, 0xf5, 0x00]
    }

    #[test]
    fn internal_create2_endowment_captured_through_composite() {
        // A CREATE2 with a non-zero endowment yields one native Transfer from the creator to the
        // CREATE2-derived address, through the composite-forwarded `create` hook on the production
        // `evm.transact` path (distinct from the plain-CREATE case).
        // The bytecode uses salt=0 and empty init code; the expected address is deterministic.
        let factory = Address::from([0xF2; 20]);
        let expected_created = factory.create2(B256::ZERO, keccak256(b""));
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));

        let (outcome, success) = run_capturing_tx(db, factory, 16);
        assert!(success, "top-level tx should succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "one CREATE2 endowment transfer expected");
                assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
                assert_eq!(stream[0].topics()[1], factory.into_word());
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(7u64).to_be_bytes::<32>());
                // Recipient (topic2) must be the exact CREATE2-derived address.
                assert_eq!(
                    stream[0].topics()[2],
                    expected_created.into_word(),
                    "recipient must be the exact salt+init_code_hash CREATE2 address"
                );
            }
            other => panic!("expected Complete with one CREATE2 transfer, got {other:?}"),
        }
    }

    /// Runtime bytecode: `LOG0(0, 0); STOP` — emits one real (empty) log then returns successfully.
    fn log_then_stop() -> Vec<u8> {
        vec![0x60, 0x00, 0x60, 0x00, 0xa0, 0x00]
    }

    /// Runtime bytecode: `CALL(gas, child, value=1, 0,0,0,0); POP; REVERT(0, 0)` — a successful child
    /// value-call (which emits a real log) followed by the PARENT frame reverting.
    fn call_child_then_revert(child: Address) -> Vec<u8> {
        let mut code = call_prefix(0x01, child);
        // GAS CALL POP  PUSH1 0 (size) PUSH1 0 (offset) REVERT
        code.extend_from_slice(&[0x5a, 0xf1, 0x50, 0x60, 0x00, 0x60, 0x00, 0xfd]);
        code
    }

    #[test]
    fn parent_frame_revert_rolls_back_child_transfer_and_real_log_through_composite() {
        // R9 item 2b / spec §8 test 32. A grandchild frame receives a qualifying native transfer and
        // emits a real log; its PARENT frame then reverts (while the top-level tx survives, ignoring
        // the failed sub-call). Through `evm.transact`, both the virtual transfer and the real log
        // must truncate at the parent checkpoint, and `result.logs()` (the reverted subtree's logs are
        // discarded) must match the now-empty RealLog subsequence → Complete(empty): the tx screens as
        // if the events never happened.
        let root = Address::from([0xA0; 20]); // calls parent (value 0), ignores its revert, succeeds
        let parent = Address::from([0xA1; 20]); // calls child (value 1) then REVERTs
        let child = Address::from([0xA2; 20]); // receives value + emits a real log, returns ok
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(root, account(0, Some(call_value_0_to(parent))));
        db.insert_account_info(parent, account(1_000, Some(call_child_then_revert(child))));
        db.insert_account_info(child, account(0, Some(log_then_stop())));

        let (outcome, success) = run_capturing_tx(db, root, 16);
        assert!(success, "the top-level tx ignores the parent frame's revert and succeeds");
        match outcome {
            CaptureOutcome::Complete(stream) => assert!(
                stream.is_empty(),
                "a parent-frame revert must truncate the child transfer AND its real log, got {stream:?}"
            ),
            other => panic!("expected Complete(empty) after a parent-frame revert, got {other:?}"),
        }
    }

    #[test]
    fn runtime_forced_real_log_mismatch_fails_closed_through_composite() {
        // R9 item 2c / spec §8 test 33 (R10: the shared runtime SOURCE for test 35). A real tx emits a
        // real log (captured through the composite-forwarded `log` hook), then `finish_capture` runs
        // against a `result.logs()` forced to DIVERGE from the captured subsequence, so the cross-check
        // fails closed with `InvariantViolation { RealLogMismatch }`. Test 33 asserts the RUNTIME
        // OUTCOME; the builder `Screen::Deny` + no-pending-buffer + no-partial-action guarantees are
        // proven together on the real screening path by test 35 (context.rs), which consumes this same
        // fixture. Stopping at `InvariantViolation` does NOT close the fail-closed loop — that is test
        // 35's job.
        let outcome = super::test_fixtures::real_runtime_reallogmismatch_outcome();
        assert!(
            matches!(
                outcome,
                CaptureOutcome::InvariantViolation {
                    reason: CaptureInvariantError::RealLogMismatch { observed: 1, expected: 2, .. }
                }
            ),
            "a runtime real-log divergence must fail closed with RealLogMismatch, got {outcome:?}"
        );
    }

    /// Runtime bytecode: `PUSH1 0; SLOAD; POP; STOP` — reads (warms) storage slot 0.
    const WARMING_CONTRACT_CODE: [u8; 5] = [0x60, 0x00, 0x54, 0x50, 0x00];

    #[test]
    fn composite_post_exec_warming_preserved_with_rcs_inspector_inner() {
        // R9 item 2d / spec §8 item 9. Making `RcsInspector` the composite's INNER inspector must NOT
        // replace the composite's own SDM/post-exec warming. Drive two warming txs through the
        // production `evm.transact` on an `OpEvm<_, RcsInspector, _>` with post-exec tracking active:
        // the second tx re-touches a block-warmed slot and MUST still earn a warming refund, exactly
        // as it does with no capture inspector, while capture runs alongside (a clean empty Complete
        // each time — an SLOAD moves no native value).
        let target = Address::from([0x2C; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(target, account(0, Some(WARMING_CONTRACT_CODE.to_vec())));

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );

        // `transact` does not commit here, so nonce 0 is reused across both txs (as in the upstream
        // OpEvm post-exec test); block-scoped warming is carried on the composite across the two txs.
        let mut warm_once = |tx_index: u64| {
            evm.begin_post_exec_tx(post_exec::PostExecTxContext {
                tx_index,
                kind: post_exec::PostExecTxKind::Normal,
            });
            evm.components_mut().1.start_capture();
            let result = evm.transact(call_tx(target)).expect("warming tx executes");
            let outcome = evm.components_mut().1.finish_capture(result.result.logs());
            assert!(
                matches!(outcome, CaptureOutcome::Complete(ref s) if s.is_empty()),
                "capture must run alongside warming (no native transfer from an SLOAD), got {outcome:?}"
            );
            evm.take_last_post_exec_tx_result().refund_total
        };

        assert_eq!(warm_once(0), 0, "the first tx warms the slot but earns no refund");
        assert!(
            warm_once(1) > 0,
            "the composite's SDM warming must still credit a refund on the second tx — making \
             RcsInspector the inner inspector did not replace the composite's post-exec behavior"
        );
    }

    // ── helpers for CREATE / DELEGATECALL tests ───────────────────────────────

    fn create_tx_with_value(from: Address, value: U256) -> OpTx {
        let tx = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Create,
            value,
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        OpTx::from_recovered_tx(&tx, from)
    }

    /// Runs a top-level CREATE tx (no bytecode body — empty init-code) through the real
    /// composite-wrapped OpEvm and returns the capture outcome and success flag.
    fn run_capturing_create_tx(
        db: InMemoryDB,
        from: Address,
        value: U256,
        budget: usize,
    ) -> (CaptureOutcome, bool) {
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(budget),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(create_tx_with_value(from, value)).expect("tx executes");
        let success = result.result.is_success();
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());
        (outcome, success)
    }

    // ── T1b: inactive inspector — CREATE tx ──────────────────────────────────

    #[test]
    fn inactive_create_tx_mutates_no_capture_state() {
        // A CREATE tx runs through the production EVM path without ever calling start_capture.
        // The inspector must leave create_recipients, checkpoints, and queue empty throughout.
        let deployer = Address::from([0xDE; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(deployer, account(1_000_000_000, None));

        let (outcome, _) = run_capturing_create_tx(db, deployer, U256::ZERO, 10);
        assert!(
            matches!(outcome, CaptureOutcome::Complete(ref s) if s.is_empty()),
            "inactive inspector must produce an empty Complete on a CREATE tx"
        );
    }

    // ── T1c: D-ISO — inactive inspector on value-bearing CREATE tx ───────────

    #[test]
    fn inactive_inspector_diso_create_tx_no_mutation() {
        // When start_capture() is never called, the create hook's inactive-gate returns
        // immediately without pushing to create_recipients. This is the D-ISO invariant
        // for the CREATE hook: a value-bearing CREATE must not mutate capture-private state.
        let deployer = Address::from([0xDF; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(deployer, account(1_000_000_000, None));
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        // Deliberately do NOT call start_capture() — inspector stays inactive.
        evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");
        let inspector = evm.components_mut().1;
        assert!(
            inspector.create_recipients.is_empty(),
            "inactive create hook must not push to create_recipients"
        );
        assert!(inspector.checkpoints.is_empty(), "inactive push_checkpoint must not add entries");
    }

    // ── T1d: create_recipients balanced push/pop ─────────────────────────────

    #[test]
    fn create_recipients_push_pop_balanced() {
        // A CREATE followed by a reverted inner CALL means:
        //   create hook  → push_checkpoint + create_recipients.push(Some(addr))
        //   create_end (success) → pop(false) does NOT revert; create_recipients.pop()
        // After finish_capture the create_recipients vec must be drained to zero.
        let factory = Address::from([0xF2; 20]);
        let expected_created = factory.create2(B256::ZERO, keccak256(b""));
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));

        // After the tx the inspector has been reset by finish_capture.
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(call_tx(factory)).expect("tx executes");
        let logs = result.result.logs();
        let _outcome = evm.components_mut().1.finish_capture(logs);

        let inspector = evm.components_mut().1;
        assert!(
            inspector.create_recipients.is_empty(),
            "create_recipients must be empty after finish_capture; addr={expected_created:?}"
        );
        assert!(inspector.checkpoints.is_empty(), "checkpoints must be empty after finish_capture");
    }

    // ── DELEGATECALL attribution baseline ────────────────────────────────────

    #[test]
    fn delegatecall_carries_no_transfer_event() {
        // A plain DELEGATECALL with no value transfer must not generate a native Transfer.
        let delegator = Address::from([0xD0; 20]);
        let implementation = Address::from([0xD1; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        // delegator has balance but its code only does a DELEGATECALL with zero value
        db.insert_account_info(
            delegator,
            account(1_000, Some(delegatecall_to_then_stop(implementation))),
        );
        // implementation: just STOP (no further transfer)
        db.insert_account_info(implementation, account(0, Some(vec![0x00])));

        let (outcome, success) = run_capturing_tx(db, delegator, 10);
        assert!(success);
        match outcome {
            CaptureOutcome::Complete(stream) => assert!(
                stream.is_empty(),
                "a DELEGATECALL with no value must not produce a native Transfer"
            ),
            other => panic!("expected Complete(empty), got {other:?}"),
        }
    }

    // ── T2c: DELEGATECALL simulation — CALL+CREATE attributed to delegating context ──

    #[test]
    fn delegatecall_nested_call_and_create_attributed_to_delegator() {
        // delegator holds balance and has code that DELEGATECALLs to `implementation`.
        // `implementation`'s code does CALL(1 wei, call_recipient) and CREATE(1 wei, empty).
        // Both operations run in `delegator`'s context → attributed to `delegator`.
        let delegator = Address::from([0xE0; 20]);
        let implementation = Address::from([0xE1; 20]);
        let call_recipient = Address::from([0xE2; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(
            delegator,
            account(1_000_000, Some(delegatecall_to_then_stop(implementation))),
        );
        db.insert_account_info(
            implementation,
            account(0, Some(delegated_call_and_create(call_recipient))),
        );
        db.insert_account_info(call_recipient, account(0, Some(vec![0x00])));

        let (outcome, success) = run_capturing_tx(db, delegator, 16);
        assert!(success, "top-level tx must succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 2, "expected two transfers: CALL(1) + CREATE(1)");
                for log in &stream {
                    assert_eq!(log.address, NATIVE_ASSET_ADDRESS);
                    // Both transfers originate from delegator (execution context for DELEGATECALL)
                    assert_eq!(
                        log.topics()[1],
                        delegator.into_word(),
                        "transfer.from must be the delegating address, not the implementation"
                    );
                }
            }
            other => panic!("expected Complete with 2 transfers, got {other:?}"),
        }
    }

    // ── T2a: EIP-7702 type-4 tx — transfers attributed to the authority address ──────────────

    #[test]
    fn eip7702_type4_delegated_transfers_attributed_to_authority() {
        // A genuine EIP-7702 type-4 transaction (TxEip7702) sets the delegation designation for
        // `authority` to `delegate_impl`. When the tx executes, the EVM runs `delegate_impl`'s
        // code in the context of `authority`. CALL(1 wei) + CREATE(1 wei) inside `delegate_impl`
        // must both be attributed to `authority` (the effective sender of those transfers).
        let delegate_impl = Address::from([0xE1; 20]);
        let call_recipient = Address::from([0xE2; 20]);
        let auth = Authorization { chain_id: U256::ZERO, address: delegate_impl, nonce: 0 }
            .into_signed(Signature::test_signature());
        let authority =
            auth.recover_authority().expect("deterministic authority from test_signature");
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(authority, account(1_000_000, None));
        db.insert_account_info(
            delegate_impl,
            account(0, Some(delegated_call_and_create(call_recipient))),
        );
        db.insert_account_info(call_recipient, account(0, None));
        let tx = TxEip7702 {
            chain_id: 1,
            nonce: 0,
            gas_limit: 2_000_000,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            to: authority,
            value: U256::ZERO,
            authorization_list: vec![auth],
            input: Default::default(),
            access_list: Default::default(),
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        let op_tx = OpTx::from_recovered_tx(&tx, caller());
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(op_tx).expect("tx executes");
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(
                    stream.len(),
                    2,
                    "EIP-7702 delegated code must capture CALL(1) + CREATE(1)"
                );
                for log in &stream {
                    assert_eq!(log.address, NATIVE_ASSET_ADDRESS);
                    assert_eq!(
                        log.topics()[1],
                        authority.into_word(),
                        "transfer.from must be authority (EIP-7702 execution context)"
                    );
                }
            }
            other => panic!("expected Complete with 2 transfers, got {other:?}"),
        }
    }

    // ── T2b: 2-hop DELEGATECALL chain attribution ────────────────────────────

    #[test]
    fn delegatecall_chain_inner_call_and_create_captured() {
        // root → DELEGATECALL → relay → DELEGATECALL → implementation
        // `implementation`'s CALL and CREATE run in `root`'s context throughout the 2-hop chain.
        let root = Address::from([0xC0; 20]);
        let relay = Address::from([0xC1; 20]);
        let implementation = Address::from([0xC2; 20]);
        let call_recipient = Address::from([0xC3; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(root, account(1_000_000, Some(delegatecall_to_then_stop(relay))));
        db.insert_account_info(relay, account(0, Some(delegatecall_to_then_stop(implementation))));
        db.insert_account_info(
            implementation,
            account(0, Some(delegated_call_and_create(call_recipient))),
        );
        db.insert_account_info(call_recipient, account(0, Some(vec![0x00])));

        let (outcome, success) = run_capturing_tx(db, root, 16);
        assert!(success, "2-hop DELEGATECALL chain tx must succeed");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 2, "expected two transfers through 2-hop chain");
                for log in &stream {
                    assert_eq!(
                        log.topics()[1],
                        root.into_word(),
                        "both transfers must be attributed to root (outermost delegating context)"
                    );
                }
            }
            other => panic!("expected Complete with 2 transfers, got {other:?}"),
        }
    }

    // ── T4a: CREATE2 EIP-684 collision revokes endowment ─────────────────────

    #[test]
    fn internal_create2_eip684_collision_endowment_revoked() {
        // When a CREATE2 target already has nonce > 0 (EIP-684 collision), the deployment
        // fails: outcome.result.is_ok() == false. The `create_end` hook must detect this and
        // call pop_checkpoint(true), rolling back the 7-wei endowment from the queue.
        let factory = Address::from([0xF3; 20]);
        let collision_target = factory.create2(B256::ZERO, keccak256(b""));
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));
        // Pre-populate the CREATE2 target with nonce=1 to trigger EIP-684 collision.
        db.insert_account_info(collision_target, AccountInfo { nonce: 1, ..Default::default() });

        let (outcome, success) = run_capturing_tx(db, factory, 16);
        assert!(success, "top-level CALL to factory must succeed even if inner CREATE2 fails");
        match outcome {
            CaptureOutcome::Complete(stream) => assert!(
                stream.is_empty(),
                "EIP-684 collision must revoke the CREATE2 endowment; stream must be empty"
            ),
            other => panic!("expected Complete(empty) after CREATE2 collision, got {other:?}"),
        }
    }

    // ── T4b: revoked CREATE still consumes observed_count budget ─────────────

    #[test]
    fn revoked_create_still_consumes_native_budget() {
        // observed_count increments in observe_native_transfer regardless of later revocation.
        // With budget=1, after a CREATE2 collision (revoked), observed_count == 1 before
        // finish_capture; after finish_capture it resets to 0 and the stream is empty.
        let factory = Address::from([0xF4; 20]);
        let collision_target = factory.create2(B256::ZERO, keccak256(b""));
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));
        db.insert_account_info(collision_target, AccountInfo { nonce: 1, ..Default::default() });

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(1),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(call_tx(factory)).expect("tx executes");
        // After transact, before finish_capture: observed_count is sticky (== 1).
        assert_eq!(
            evm.components_mut().1.observed_count,
            1,
            "observed_count must be 1 after revoked CREATE2 (sticky counter, not rolled back)"
        );
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert!(stream.is_empty(), "revoked endowment must not appear in the final stream")
            }
            other => panic!("expected Complete(empty), got {other:?}"),
        }
    }

    /// Runtime bytecode: CALL(gas, call_dst, 1 wei, 0, 0, 0, 0); POP; CREATE2(7 wei, salt=0, empty)
    fn call_value_1_then_create2_endowment(call_dst: Address) -> Vec<u8> {
        let mut code = call_prefix(0x01, call_dst);
        // GAS CALL POP
        code.extend_from_slice(&[0x5a, 0xf1, 0x50]);
        // PUSH1 0 (salt) PUSH1 0 (size) PUSH1 0 (offset) PUSH1 7 (value) CREATE2
        code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x07, 0xf5]);
        code
    }

    // ── T4b-ext: budget=N, then N+1 colliding CREATE → LimitExceeded ──────────────────────────

    #[test]
    fn revoked_create_with_prior_transfer_exceeds_budget_limit() {
        // Proves that observed_count from a revoked CREATE is real budget consumption:
        // with limit=1, one CALL (fills the budget) plus a revoked CREATE2 collision
        // (one more observation) must produce LimitExceeded{attempted:2, limit:1}.
        let factory = Address::from([0xF5; 20]);
        let call_recipient = Address::from([0xF6; 20]);
        let collision_target = factory.create2(B256::ZERO, keccak256(b""));
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(
            factory,
            account(1_000, Some(call_value_1_then_create2_endowment(call_recipient))),
        );
        db.insert_account_info(call_recipient, account(0, None));
        db.insert_account_info(collision_target, AccountInfo { nonce: 1, ..Default::default() });

        let (outcome, success) = run_capturing_tx(db, factory, 1);
        assert!(success, "top-level CALL to factory must succeed even on budget overflow");
        assert!(
            matches!(outcome, CaptureOutcome::LimitExceeded { attempted: 2, limit: 1 }),
            "one CALL + one revoked CREATE2 must exceed budget=1; got {outcome:?}"
        );
    }

    // ── top_level_create_with_value_emits_transfer ───────────────────────────

    #[test]
    fn top_level_create_with_value_emits_transfer() {
        // A top-level CREATE tx with non-zero value sends the endowment from the deployer
        // to the derived contract address. The inspector must capture that as one Transfer.
        let deployer = Address::from([0xDE; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(deployer, account(1_000_000_000, None));

        let (outcome, _) = run_capturing_create_tx(db, deployer, U256::from(5u64), 10);
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "one Transfer expected for top-level CREATE endowment");
                assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);
                assert_eq!(
                    stream[0].topics()[1],
                    deployer.into_word(),
                    "transfer.from must be the deployer"
                );
                assert_eq!(
                    stream[0].topics()[2],
                    deployer.create(0).into_word(),
                    "transfer.to must be the contract address derived from deployer + nonce 0"
                );
            }
            other => panic!("expected Complete with one Transfer, got {other:?}"),
        }
    }

    // ── D7b: successful-CREATE address mismatch fails closed (InvariantViolation) ────────────────
    #[test]
    fn create_address_mismatch_through_real_create_end_fails_closed() {
        // D7b: a successful CREATE whose actual deployed address differs from the pre-derived
        // expected address stored in `create_recipients` must produce
        // `InvariantViolation { CreateAddressMismatch }`, NEVER `Complete`. The
        // `FORCE_CREATE_ADDRESS_MISMATCH` seam pushes `Address::repeat_byte(0xFE)` as the expected
        // recipient in the real `create` hook, so `create_end` sees a genuine `e != a` on an
        // actually-successful, value-bearing CREATE (is_ok=true, address=Some(real)).
        // Falsifiable: deleting the D7b branch in `create_end` causes this test to fail
        // (`Complete(empty)` instead of `InvariantViolation`); deleting the variant causes a
        // compile error in this `matches!`.
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        FORCE_CREATE_ADDRESS_MISMATCH.with(|f| f.set(true));
        let (outcome, success) = run_capturing_create_tx(db, caller(), U256::from(1), 16);
        assert!(success, "the CREATE must succeed (is_ok=true) for D7b to be reachable");
        assert!(
            matches!(
                outcome,
                CaptureOutcome::InvariantViolation {
                    reason: CaptureInvariantError::CreateAddressMismatch { .. }
                }
            ),
            "successful CREATE with mismatched pre-derived/actual address must fail closed with \
             CreateAddressMismatch, got {outcome:?}"
        );
    }

    #[test]
    fn address_mismatch_flag_cleared_by_both_finish_and_abort() {
        // The sticky mismatch flag must not survive either exit path: `finish_capture` reports it
        // exactly once and clears it, and `abort_capture` clears it without reporting. A flag left
        // set would make the NEXT capture window fail closed on a stale mismatch.
        //
        // (a) armed flag → finish_capture surfaces it AND clears it.
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.address_mismatch_create_index = Some(3);
        let outcome = inspector.finish_capture(&[]);
        assert_eq!(
            outcome,
            CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::CreateAddressMismatch { create_index: 3 }
            },
            "an armed mismatch flag must surface as InvariantViolation on finish_capture"
        );
        assert!(
            inspector.address_mismatch_create_index.is_none(),
            "finish_capture must clear the sticky mismatch flag on its exit path"
        );

        // (b) armed flag → abort_capture clears it.
        inspector.start_capture();
        inspector.address_mismatch_create_index = Some(7);
        inspector.abort_capture();
        assert!(
            inspector.address_mismatch_create_index.is_none(),
            "abort_capture must clear the sticky mismatch flag on its exit path"
        );
        assert!(!inspector.active, "abort_capture must leave the inspector disarmed");
    }

    // ── screen_tx integration tests ───────────────────────────────────────────
    // These tests run a real OpEvm transaction, capture the native transfer
    // stream, then feed it directly into FilterHandle::screen_tx, proving
    // the full pipeline from EVM execution through the rule engine.

    use rcs_filter::rules::{load_rules, RawRule};
    use rcs_filter::test_support::TestClock;
    use rcs_filter::{BufferStatus, FilterConfig, FilterHandle, Screen, ScreenInput};
    use std::sync::Arc;

    const DENY_NATIVE_RULE: &str = r#"{
        "id": "deny-native",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"transfer.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "action": "deny"
    }"#;

    const AUDIT_NATIVE_RULE: &str = r#"{
        "id": "audit-1",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"transfer.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "audit_types": ["type_a"],
        "action": "audit"
    }"#;

    const AUDIT_NATIVE_RULE_2: &str = r#"{
        "id": "audit-2",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"transfer.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "audit_types": ["type_b"],
        "action": "audit"
    }"#;

    const DUAL_ALIAS_DENY_NATIVE_RULE: &str = r#"{
        "id": "dual-alias-deny",
        "event_abis": {
            "t1": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            },
            "t2": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"t1.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "action": "deny"
    }"#;

    /// Same Transfer ABI as the rule above, but the condition is on the transaction-level `value`
    /// binding rather than on any event field. Deployed rules may still be written against that
    /// legacy binding, so it must keep working now that a virtual `transfer.*` log set exists
    /// alongside it.
    const LEGACY_VALUE_DENY_RULE: &str = r#"{
        "id": "legacy-value-deny",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"value"},"1"]},
        "action": "deny"
    }"#;

    /// A second audit rule carrying the SAME `audit_type` as [`AUDIT_NATIVE_RULE`]. Two same-type
    /// rules matching the same physical log must collapse to one action, which is what distinguishes
    /// a genuine `(audit_type, log_index)` dedup from an implicit "one rule, one action" placement.
    const AUDIT_NATIVE_RULE_SAME_TYPE: &str = r#"{
        "id": "audit-same-type",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"transfer.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "audit_types": ["type_a"],
        "action": "audit"
    }"#;

    /// An audit rule whose condition is satisfied by transaction-level data alone, so it also holds
    /// when the declared event matches NO physical log. That is the all-null binding case: the rule
    /// is satisfied, yet no Audit action may be emitted because there is no physical event behind it.
    const AUDIT_NULL_BINDING_RULE: &str = r#"{
        "id": "audit-null-binding",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"value"},"0"]},
        "audit_types": ["type_null"],
        "action": "audit"
    }"#;

    /// Dual-alias rule over two aliases of the SAME Transfer ABI, whose condition never fires. It
    /// isolates the matcher-binding budget from the rule's own verdict: any Deny on the larger
    /// stream can then only come from the binding counter, never from the rule matching.
    const DUAL_ALIAS_BUDGET_RULE: &str = r#"{
        "id": "dual-alias-budget",
        "event_abis": {
            "t1": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            },
            "t2": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"t1.address"},"0x000000000000000000000000000000000000dead"]},
        "action": "deny"
    }"#;

    fn deny_native_handle() -> FilterHandle {
        let raw: RawRule = serde_json::from_str(DENY_NATIVE_RULE).unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        assert_eq!(rules.rules.len(), 1, "deny-native rule must compile cleanly");
        FilterHandle::for_test(
            FilterConfig::default(),
            rules,
            Arc::new(TestClock::new(1_751_000_000)),
        )
    }

    fn audit_native_handle() -> FilterHandle {
        let raw: RawRule = serde_json::from_str(AUDIT_NATIVE_RULE).unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        assert_eq!(rules.rules.len(), 1, "audit-1 rule must compile cleanly");
        FilterHandle::for_test(
            FilterConfig::default(),
            rules,
            Arc::new(TestClock::new(1_751_000_000)),
        )
    }

    fn run_capturing_call_tx_with_value(
        db: InMemoryDB,
        from: Address,
        target: Address,
        value: U256,
        budget: usize,
    ) -> (CaptureOutcome, bool) {
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(budget),
        );
        evm.components_mut().1.start_capture();
        let tx = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Call(target),
            value,
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        let op_tx = OpTx::from_recovered_tx(&tx, from);
        let result = evm.transact(op_tx).expect("tx executes");
        let success = result.result.is_success();
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());
        (outcome, success)
    }

    /// Internal CREATE with 7-wei endowment produces a native transfer captured in the real
    /// stream; the deny-native rule fires on that log through the full screen_tx path.
    #[test]
    fn original_nt2_internal_create_denied_through_real_screen_tx() {
        let factory = Address::from([0xF3; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(factory, account(1_000, Some(create_endowment_7())));

        let (outcome, success) = run_capturing_tx(db, factory, 16);
        assert!(success, "internal-CREATE tx must succeed");
        let stream = match outcome {
            CaptureOutcome::Complete(s) => s,
            other => panic!("expected Complete, got {other:?}"),
        };
        assert!(!stream.is_empty(), "CREATE endowment must produce a native transfer");

        let handle = deny_native_handle();
        let input = ScreenInput {
            tx_hash: B256::from([0x11; 32]),
            origin: caller(),
            tx_to: Some(factory),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &stream,
        };
        assert_eq!(handle.screen_tx(&input), Screen::Deny);
    }

    /// A top-level value CALL (tx.value = 1) produces a single native Transfer as the root
    /// frame hook fires; the deny-native rule fires on that real synthetic log.
    #[test]
    fn top_level_native_denied_through_real_screen_tx() {
        let target = Address::from([0xBB; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));

        let (outcome, success) =
            run_capturing_call_tx_with_value(db, caller(), target, U256::from(1u64), 16);
        assert!(success, "top-level value CALL must succeed");
        let stream = match outcome {
            CaptureOutcome::Complete(s) => s,
            other => panic!("expected Complete, got {other:?}"),
        };
        assert_eq!(stream.len(), 1, "exactly one top-level native transfer expected");
        assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);

        let handle = deny_native_handle();
        let input = ScreenInput {
            tx_hash: B256::from([0x22; 32]),
            origin: caller(),
            tx_to: Some(target),
            nonce: 0,
            value: U256::from(1u64),
            block_height: 0,
            logs: &stream,
        };
        assert_eq!(handle.screen_tx(&input), Screen::Deny);
    }

    /// A top-level value CALL to a contract that itself does an internal value CALL produces
    /// two native Transfers (one per frame); a single deny-native rule fires on the first
    /// match, proving the rule works across a mixed root+nested stream.
    #[test]
    fn one_transfer_rule_denies_both_toplevel_and_internal_real_stream() {
        let inner_dst = Address::from([0xDD; 20]);
        let contract = Address::from([0xCC; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(contract, account(1_000, Some(call_value_1_to(inner_dst))));

        let (outcome, success) =
            run_capturing_call_tx_with_value(db, caller(), contract, U256::from(1u64), 16);
        assert!(success, "tx must succeed");
        let stream = match outcome {
            CaptureOutcome::Complete(s) => s,
            other => panic!("expected Complete, got {other:?}"),
        };
        assert_eq!(stream.len(), 2, "root-frame + internal-frame native transfers expected");

        let handle = deny_native_handle();
        let input = ScreenInput {
            tx_hash: B256::from([0x33; 32]),
            origin: caller(),
            tx_to: Some(contract),
            nonce: 0,
            value: U256::from(1u64),
            block_height: 0,
            logs: &stream,
        };
        assert_eq!(handle.screen_tx(&input), Screen::Deny);
    }

    /// The capture budget is enforced at the EVM level: budget=2 fits two native transfers
    /// (Complete), while budget=1 causes LimitExceeded at the second transfer.
    /// Falsifiable: reducing call count or raising budget would change the outcome.
    #[test]
    fn real_stream_native_budget_limit_plus_one_denies() {
        let a = Address::from([0xE1; 20]);
        let b = Address::from([0xE2; 20]);
        let contract = Address::from([0xE3; 20]);

        // budget=2: both transfers captured → Complete(2)
        let mut db1 = InMemoryDB::default();
        db1.insert_account_info(caller(), account(1_000_000_000, None));
        db1.insert_account_info(contract, account(1_000, Some(call_value_1_twice(a, b))));
        let (outcome1, _) = run_capturing_tx(db1, contract, 2);
        assert!(
            matches!(&outcome1, CaptureOutcome::Complete(s) if s.len() == 2),
            "budget=2 with 2 transfers must produce Complete(2), got {outcome1:?}"
        );

        // budget=1: second transfer overflows → LimitExceeded{attempted:2, limit:1}
        let mut db2 = InMemoryDB::default();
        db2.insert_account_info(caller(), account(1_000_000_000, None));
        db2.insert_account_info(contract, account(1_000, Some(call_value_1_twice(a, b))));
        let (outcome2, _) = run_capturing_tx(db2, contract, 1);
        assert!(
            matches!(outcome2, CaptureOutcome::LimitExceeded { attempted: 2, limit: 1 }),
            "budget=1 with 2 transfers must produce LimitExceeded{{2,1}}, got {outcome2:?}"
        );
    }

    /// AuditPending → advance to Approved → AuditApproved (same logs); then empty-log
    /// re-screen → inconsistent → Drop.  Exercises the full audit lifecycle end-to-end.
    #[test]
    fn real_stream_screen_tx_and_screen_existing_consistent() {
        let dst = Address::from([0xF4; 20]);
        let contract = Address::from([0xF5; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(contract, account(1_000, Some(call_value_1_to(dst))));

        let (outcome, success) = run_capturing_tx(db, contract, 16);
        assert!(success);
        let stream = match outcome {
            CaptureOutcome::Complete(s) => s,
            other => panic!("expected Complete, got {other:?}"),
        };
        assert_eq!(stream.len(), 1, "one native transfer expected");

        let handle = audit_native_handle();
        let tx_hash = B256::from([0x55; 32]);

        let input = ScreenInput {
            tx_hash,
            origin: caller(),
            tx_to: Some(contract),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &stream,
        };
        assert_eq!(handle.screen_tx(&input), Screen::AuditPending);

        handle.advance_to_approved_for_test(&tx_hash);

        assert_eq!(handle.screen_tx(&input), Screen::AuditApproved, "same logs → consistent");

        let empty_input = ScreenInput {
            tx_hash,
            origin: caller(),
            tx_to: Some(contract),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &[],
        };
        assert_eq!(
            handle.screen_tx(&empty_input),
            Screen::Drop,
            "empty logs → inconsistent → Drop"
        );
    }

    /// A real stream containing one native Transfer + one real LOG0 (superset of the rule's
    /// expected shape), screened by a rule whose condition reads the LEGACY transaction-level
    /// `value` binding rather than any event field. That binding must keep working unchanged now
    /// that the virtual `transfer.*` log set exists alongside it.
    #[test]
    fn legacy_value_rule_still_fires_on_real_capture_superset_stream() {
        let dst = Address::from([0xC6; 20]);
        let contract = Address::from([0xB6; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(contract, account(1_000, Some(call_value_1_then_log0(dst))));

        let (outcome, success) = run_capturing_tx(db, contract, 16);
        assert!(success);
        let stream = match outcome {
            CaptureOutcome::Complete(s) => s,
            other => panic!("expected Complete, got {other:?}"),
        };
        assert_eq!(stream.len(), 2, "one native transfer + one real log expected");
        assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS, "first log must be native transfer");
        assert_ne!(stream[1].address, NATIVE_ASSET_ADDRESS, "second log must be the real log");

        let raw: RawRule = serde_json::from_str(LEGACY_VALUE_DENY_RULE).unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        assert_eq!(rules.rules.len(), 1, "legacy-value-deny rule must compile cleanly");

        // Fires: the rule's condition is on the transaction-level `value` binding, which is `1`.
        let firing = FilterHandle::for_test(
            FilterConfig::default(),
            rules.clone(),
            Arc::new(TestClock::new(1_751_000_000)),
        );
        let firing_input = ScreenInput {
            tx_hash: B256::from([0x66; 32]),
            origin: caller(),
            tx_to: Some(contract),
            nonce: 0,
            value: U256::from(1u64),
            block_height: 0,
            logs: &stream,
        };
        assert_eq!(firing.screen_tx(&firing_input), Screen::Deny);

        // Negative control: the SAME rules and the SAME captured stream, with only the
        // transaction-level `value` changed to zero, must NOT fire. This proves the rule matched on
        // the legacy tx-level binding — not on the event's own `transfer.value` (which is 1 wei in
        // the captured log and is untouched by this change).
        let controlled = FilterHandle::for_test(
            FilterConfig::default(),
            rules,
            Arc::new(TestClock::new(1_751_000_000)),
        );
        let controlled_input = ScreenInput { value: U256::ZERO, ..firing_input };
        assert_eq!(
            controlled.screen_tx(&controlled_input),
            Screen::Allow,
            "with tx-level value 0 the legacy rule must not fire, so the Deny above is the \
             transaction-level `value` binding and not the event's transfer.value"
        );
    }

    /// Two real streams of different widths compared under one dual-alias rule (t1 × t2 over the
    /// SAME Transfer ABI), so the binding count grows quadratically while the rule's own verdict
    /// stays fixed. The within-budget stream and the over-budget stream are then distinguishable
    /// controls rather than a single Deny that a rule which denies by itself could also produce.
    ///
    /// `dual_alias_budget_handle` admits `max_event_bindings_per_tx` so the same stream can be
    /// screened either side of the boundary.
    fn dual_alias_budget_handle(max_event_bindings_per_tx: usize) -> FilterHandle {
        let raw: RawRule = serde_json::from_str(DUAL_ALIAS_BUDGET_RULE).unwrap();
        let rules = load_rules(1, 1, vec![raw]);
        assert_eq!(rules.rules.len(), 1, "dual-alias-budget rule must compile cleanly");
        FilterHandle::for_test(
            FilterConfig { max_event_bindings_per_tx, ..Default::default() },
            rules,
            Arc::new(TestClock::new(1_751_000_000)),
        )
    }

    #[test]
    fn dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream() {
        let a = Address::from([0xE4; 20]);
        let b = Address::from([0xE5; 20]);
        let contract = Address::from([0xE6; 20]);

        // Stream-A: ONE native transfer → t1 × t2 = 1 × 1 = 1 binding.
        let mut db_a = InMemoryDB::default();
        db_a.insert_account_info(caller(), account(1_000_000_000, None));
        let (outcome_a, _) = run_capturing_call_tx_with_value(
            db_a,
            caller(),
            Address::from([0xE7; 20]),
            U256::from(1u64),
            16,
        );
        let stream_a = match outcome_a {
            CaptureOutcome::Complete(s) => s,
            other => panic!("expected Complete for stream-A, got {other:?}"),
        };
        assert_eq!(stream_a.len(), 1, "stream-A must carry exactly one native transfer");

        // Stream-B: TWO native transfers → t1 × t2 = 2 × 2 = 4 bindings.
        let mut db_b = InMemoryDB::default();
        db_b.insert_account_info(caller(), account(1_000_000_000, None));
        db_b.insert_account_info(contract, account(1_000, Some(call_value_1_twice(a, b))));
        let (outcome_b, _) = run_capturing_tx(db_b, contract, 16);
        let stream_b = match outcome_b {
            CaptureOutcome::Complete(s) => s,
            other => panic!("expected Complete for stream-B, got {other:?}"),
        };
        assert_eq!(stream_b.len(), 2, "stream-B must carry exactly two native transfers");

        let input_a = ScreenInput {
            tx_hash: B256::from([0x77; 32]),
            origin: caller(),
            tx_to: Some(contract),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &stream_a,
        };
        let input_b = ScreenInput {
            tx_hash: B256::from([0x78; 32]),
            origin: caller(),
            tx_to: Some(contract),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &stream_b,
        };

        // Within budget: 1 binding, and the rule's condition is false → Allow. This is the
        // budget-not-exceeded side of the comparison.
        assert_eq!(
            dual_alias_budget_handle(3).screen_tx(&input_a),
            Screen::Allow,
            "stream-A stays within the binding budget and must not be denied"
        );

        // limit-th binding count (4) with budget 3 → the (limit+1)-th binding is refused, so the
        // rule never even completes and the transaction fails closed with Deny.
        assert_eq!(
            dual_alias_budget_handle(3).screen_tx(&input_b),
            Screen::Deny,
            "stream-B exceeds the binding budget and must fail closed"
        );

        // Boundary control: raise the budget to exactly stream-B's binding count. The SAME stream
        // and the SAME rule now complete and, since the condition is false, Allow — proving the
        // Deny above came from the binding counter at the boundary, not from the rule's verdict.
        assert_eq!(
            dual_alias_budget_handle(4).screen_tx(&input_b),
            Screen::Allow,
            "at the exact binding count the same stream is admitted, so the Deny was the budget"
        );

        // The rule itself still denies when it genuinely matches, so the Allow/Deny split above is
        // attributable to the budget and not to a rule that can never deny.
        let deny_raw: RawRule = serde_json::from_str(DUAL_ALIAS_DENY_NATIVE_RULE).unwrap();
        let deny_rules = load_rules(1, 1, vec![deny_raw]);
        assert_eq!(deny_rules.rules.len(), 1, "dual-alias-deny rule must compile cleanly");
        let deny_handle = FilterHandle::for_test(
            FilterConfig { max_event_bindings_per_tx: 4, ..Default::default() },
            deny_rules,
            Arc::new(TestClock::new(1_751_000_000)),
        );
        assert_eq!(
            deny_handle.screen_tx(&input_b),
            Screen::Deny,
            "the same two-alias shape denies on its own verdict once the budget admits it"
        );
    }

    /// `log.address` in the lower-cased hex form the action payload carries.
    fn lower_hex(a: Address) -> String {
        format!("{a:#x}")
    }

    /// Audit-rule merging on a real captured stream, asserted on the CONTENT that reaches the buffer
    /// rather than on the screen verdict alone. Three properties are pinned, each with its own
    /// control:
    /// - (a) two rules declaring the SAME `audit_type` and matching the SAME physical log collapse
    ///   to ONE action, keyed by `(audit_type, log_index)`, carrying the decoded `transfer` inputs;
    /// - (b) a rule declaring a DIFFERENT `audit_type` is retained as its own action — the
    ///   `audit_type` is the key of the `actions` map, not a field of the action item;
    /// - (c) an all-null binding (a satisfied audit rule with no physical event behind it) produces
    ///   NO action at all, proven against the same rule firing when a physical event does back it.
    #[test]
    fn audit_type_action_merge_action_content_no_empty_audit_on_real_stream() {
        let target = Address::from([0xB8; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        let (outcome, success) =
            run_capturing_call_tx_with_value(db, caller(), target, U256::from(1u64), 16);
        assert!(success, "top-level value CALL must succeed");
        let stream = match outcome {
            CaptureOutcome::Complete(s) => s,
            other => panic!("expected Complete, got {other:?}"),
        };
        assert_eq!(stream.len(), 1, "exactly one native transfer expected");
        assert_eq!(stream[0].address, NATIVE_ASSET_ADDRESS);

        // ---- (a) + (b): same-type dedup, different-type retention, action content ----------------
        let raw1: RawRule = serde_json::from_str(AUDIT_NATIVE_RULE).unwrap(); // audit_type "type_a"
        let raw2: RawRule = serde_json::from_str(AUDIT_NATIVE_RULE_SAME_TYPE).unwrap(); // "type_a"
        let raw3: RawRule = serde_json::from_str(AUDIT_NATIVE_RULE_2).unwrap(); // audit_type "type_b"
        let rules = load_rules(1, 1, vec![raw1, raw2, raw3]);
        assert_eq!(rules.rules.len(), 3, "all three audit rules must compile cleanly");
        let handle = FilterHandle::for_test(
            FilterConfig::default(),
            rules,
            Arc::new(TestClock::new(1_751_000_000)),
        );
        let merged_tx = B256::from([0x88; 32]);
        let merged_input = ScreenInput {
            tx_hash: merged_tx,
            origin: caller(),
            tx_to: Some(target),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &stream,
        };
        assert_eq!(
            handle.screen_tx(&merged_input),
            Screen::AuditPending,
            "merged audit rules → AuditPending"
        );
        assert_eq!(
            handle.buffer_status(&merged_tx),
            Some(BufferStatus::NotSubmitted),
            "pool entry must be NotSubmitted after first screen"
        );

        let actions = handle
            .with_pool(|pool| pool.get(&merged_tx).map(|entry| entry.actions.clone()))
            .expect("the audited transaction must have a buffered entry");
        // (b) `audit_type` is the KEY of the actions map, so the two distinct types are two keys.
        let keys: Vec<&str> = actions.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["type_a", "type_b"], "audit_type is the actions-map key");

        // (a) Two rules with the same audit_type matching the same log dedup to ONE action.
        let type_a = actions.get("type_a").expect("type_a must be present");
        assert_eq!(
            type_a.len(),
            1,
            "same (audit_type, log_index) must collapse to a single action, got {type_a:?}"
        );
        let type_b = actions.get("type_b").expect("type_b must be present");
        assert_eq!(type_b.len(), 1, "the distinct audit_type keeps its own single action");

        // ...and the retained action carries the decoded transfer content, not an empty shell.
        let item = &type_a[0];
        assert_eq!(item.name, "transfer", "the action names the rule-local event alias");
        assert_eq!(
            item.address,
            lower_hex(NATIVE_ASSET_ADDRESS),
            "the action address is the triggering log's address"
        );
        let expected_from = lower_hex(caller());
        let expected_to = lower_hex(target);
        assert_eq!(item.params.get("from").and_then(|v| v.as_str()), Some(expected_from.as_str()));
        assert_eq!(item.params.get("to").and_then(|v| v.as_str()), Some(expected_to.as_str()));
        assert_eq!(item.params.get("value").and_then(|v| v.as_str()), Some("1"));

        // ---- (c) negative control: a satisfied audit rule with NO physical event ---------------
        // The condition is on the transaction-level `value`, so it holds whether or not the event
        // matched; what must differ is the presence of a physical log behind the binding.
        let null_raw: RawRule = serde_json::from_str(AUDIT_NULL_BINDING_RULE).unwrap();
        let null_rules = load_rules(1, 1, vec![null_raw]);
        assert_eq!(null_rules.rules.len(), 1, "audit-null-binding rule must compile cleanly");
        let null_handle = FilterHandle::for_test(
            FilterConfig::default(),
            null_rules,
            Arc::new(TestClock::new(1_751_000_000)),
        );

        let empty_tx = B256::from([0x89; 32]);
        let empty_input = ScreenInput {
            tx_hash: empty_tx,
            origin: caller(),
            tx_to: Some(target),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &[],
        };
        assert_eq!(
            null_handle.screen_tx(&empty_input),
            Screen::Allow,
            "an all-null binding must not produce an Audit action"
        );
        assert_eq!(
            null_handle.buffer_status(&empty_tx),
            None,
            "no buffered entry may exist for an all-null binding (no empty Audit submitted)"
        );
        assert_eq!(null_handle.buffered_len(), 0, "the buffer must stay empty");
        assert!(
            null_handle.with_pool(|pool| pool.get(&empty_tx).is_none()),
            "no action payload may be stored for an all-null binding"
        );

        // Positive counterpart on the SAME rule: with the real stream the event binds physically,
        // so the very same satisfied condition now yields the action. This is what makes the
        // Allow above evidence about the physical-event guard rather than about the condition.
        let backed_tx = B256::from([0x8A; 32]);
        let backed_input = ScreenInput {
            tx_hash: backed_tx,
            origin: caller(),
            tx_to: Some(target),
            nonce: 0,
            value: U256::ZERO,
            block_height: 0,
            logs: &stream,
        };
        assert_eq!(
            null_handle.screen_tx(&backed_input),
            Screen::AuditPending,
            "the same rule must audit once a physical event backs the binding"
        );
        let backed_actions = null_handle
            .with_pool(|pool| pool.get(&backed_tx).map(|entry| entry.actions.clone()))
            .expect("the backed transaction must have a buffered entry");
        assert_eq!(
            backed_actions.get("type_null").map(Vec::len),
            Some(1),
            "the backed binding produces exactly one type_null action"
        );
    }

    // ── Regression: root CALL value transfer ─────────────────────────────────────────────────────

    fn always_revert_code() -> Vec<u8> {
        // PUSH1 0 PUSH1 0 REVERT
        vec![0x60, 0x00, 0x60, 0x00, 0xfd]
    }

    fn call_with_value_to(value: u8, dst: Address) -> Vec<u8> {
        let mut code = call_prefix(value, dst);
        code.extend_from_slice(&[0x5a, 0xf1, 0x00]); // GAS CALL STOP
        code
    }

    #[test]
    fn top_level_call_with_value_emits_transfer() {
        // A top-level CALL tx with non-zero tx.value fires the call hook at journal depth=0.
        // The non-zero value must produce exactly one native Transfer event from the caller to
        // the target contract.
        let contract = Address::from([0xCC; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(contract, account(0, None));

        let tx = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Call(contract),
            value: U256::from(3u64),
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        let op_tx = OpTx::from_recovered_tx(&tx, caller());

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(op_tx).expect("tx executes");
        assert!(result.result.is_success(), "top-level CALL with value should succeed");
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());

        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(
                    stream.len(),
                    1,
                    "one native transfer expected from top-level call value"
                );
                assert_eq!(stream[0].topics()[1], caller().into_word(), "from = caller");
                assert_eq!(stream[0].topics()[2], contract.into_word(), "to = contract");
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(3u64).to_be_bytes::<32>());
            }
            other => panic!("expected Complete with one transfer, got {other:?}"),
        }
    }

    // ── Regression: D7a nonce-overflow endowment revocation (is_ok=true, address=None) ──────────

    #[test]
    fn create_is_ok_early_return_no_endowment_leak() {
        // D7a: when a factory account's nonce = u64::MAX the internal CREATE returns
        // CreateOutcome { address: None, is_ok: true } (revm nonce-overflow path). The
        // pre-pushed endowment event must be revoked even though is_ok is true.
        // The top-level CALL endowment (value=5) is the only Transfer that must survive.
        let factory = Address::from([0xFA; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(
            factory,
            AccountInfo {
                balance: U256::from(1_000u64),
                nonce: u64::MAX,
                code: Some(Bytecode::new_raw(Bytes::from(create_endowment_7()))),
                ..Default::default()
            },
        );

        let tx = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Call(factory),
            value: U256::from(5u64),
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        let op_tx = OpTx::from_recovered_tx(&tx, caller());

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(op_tx).expect("tx executes");
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());

        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(
                    stream.len(),
                    1,
                    "only the top-level transfer survives; nonce-overflow endowment must be revoked"
                );
                assert_eq!(stream[0].topics()[1], caller().into_word(), "from = caller");
                assert_eq!(stream[0].topics()[2], factory.into_word(), "to = factory");
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(5u64).to_be_bytes::<32>());
            }
            other => panic!("expected Complete with one transfer, got {other:?}"),
        }
    }

    // ── Regression: child-frame revert rollback ───────────────────────────────────────────────────

    #[test]
    fn root_success_child_revert_keeps_root_drops_child() {
        // Root CALL (value=5) succeeds; the contract makes an internal CALL (value=3) that
        // reverts. The root Transfer survives; the reverted child Transfer is removed.
        let contract = Address::from([0xC2; 20]);
        let revert_target = Address::from([0xED; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(
            contract,
            account(1_000, Some(call_with_value_to(0x03, revert_target))),
        );
        db.insert_account_info(revert_target, account(0, Some(always_revert_code())));

        let tx = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Call(contract),
            value: U256::from(5u64),
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        let op_tx = OpTx::from_recovered_tx(&tx, caller());

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(op_tx).expect("tx executes");
        assert!(result.result.is_success(), "root tx should succeed");
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());

        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "only root Transfer survives; child dropped");
                assert_eq!(stream[0].topics()[1], caller().into_word(), "from = caller");
                assert_eq!(stream[0].topics()[2], contract.into_word(), "to = contract");
                assert_eq!(stream[0].data.data.as_ref(), &U256::from(5u64).to_be_bytes::<32>());
            }
            other => panic!("expected Complete with one transfer, got {other:?}"),
        }
    }

    // ── Regression: root failure drops all transfers ──────────────────────────────────────────────

    #[test]
    fn whole_tx_root_failure_drops_all() {
        // Root tx reverts. No Transfer event should survive.
        let revert_target = Address::from([0xEC; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(revert_target, account(0, Some(always_revert_code())));

        let tx = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Call(revert_target),
            value: U256::from(5u64),
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        let op_tx = OpTx::from_recovered_tx(&tx, caller());

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(op_tx).expect("tx executes");
        assert!(!result.result.is_success(), "root tx should revert");
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());

        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert!(stream.is_empty(), "root revert must drop all transfers");
            }
            other => panic!("expected Complete(empty), got {other:?}"),
        }
    }

    // ── Regression: early CREATE failure produces no event or address leak ────────────────────────

    #[test]
    fn create_early_failure_no_address_or_event_leak() {
        // A factory issues CREATE with value=7 but has zero balance; the CREATE fails
        // (insufficient balance). The pre-pushed endowment event must not survive.
        let factory = Address::from([0xFB; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(factory, account(0, Some(create_endowment_7())));

        let (outcome, success) = run_capturing_tx(db, factory, 16);
        assert!(success, "top-level zero-value tx should succeed even if inner CREATE fails");
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert!(
                    stream.is_empty(),
                    "failed CREATE with zero-value top-level must leave an empty stream"
                );
            }
            other => panic!("expected Complete(empty), got {other:?}"),
        }
    }

    // ── Regression: second start_capture resets all inspector state ───────────────────────────────

    #[test]
    fn next_capture_starts_clean() {
        // After a completed capture, start_capture on the same inspector starts clean.
        // Two distinct senders (both nonce=0) share one inspector so neither depends on
        // the other committing state changes.
        let sender_a = Address::from([0xA2; 20]);
        let sender_b = Address::from([0xA3; 20]);
        let contract = Address::from([0xC3; 20]);
        let revert_target = Address::from([0xEB; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(sender_a, account(1_000_000_000, None));
        db.insert_account_info(sender_b, account(1_000_000_000, None));
        db.insert_account_info(
            contract,
            account(1_000, Some(call_with_value_to(0x03, revert_target))),
        );
        db.insert_account_info(revert_target, account(0, Some(always_revert_code())));

        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );

        // First tx (sender_a): root value=5, internal CALL (value=3) reverts.
        let tx1 = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Call(contract),
            value: U256::from(5u64),
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        evm.components_mut().1.start_capture();
        let result1 = evm.transact(OpTx::from_recovered_tx(&tx1, sender_a)).expect("tx1");
        let _outcome1 = evm.components_mut().1.finish_capture(result1.result.logs());

        // Second tx (sender_b): zero value — must produce an independent, empty stream.
        let tx2 = TxLegacy {
            nonce: 0,
            gas_limit: 2_000_000,
            to: TxKind::Call(contract),
            value: U256::ZERO,
            ..Default::default()
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        evm.components_mut().1.start_capture();
        let result2 = evm.transact(OpTx::from_recovered_tx(&tx2, sender_b)).expect("tx2");
        let outcome2 = evm.components_mut().1.finish_capture(result2.result.logs());

        match outcome2 {
            CaptureOutcome::Complete(stream) => {
                assert!(stream.is_empty(), "second tx (zero value) must produce an empty stream");
            }
            other => panic!("expected Complete(empty) for second tx, got {other:?}"),
        }
    }

    // ── True nonce-overflow: is_ok=true, address=None (distinct from EIP-684 collision) ──────────

    #[test]
    fn create_nonce_overflow_is_ok_true_address_none_revoked() {
        // When a factory's nonce = u64::MAX, a CREATE with endowment returns
        // is_ok=true, address=None (nonce-overflow, not EIP-684 collision where is_ok=false).
        // The D7a path revokes the endowment; with a zero-value outer call, the stream is empty.
        let factory = Address::from([0xF6; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(
            factory,
            AccountInfo {
                balance: U256::from(1_000u64),
                nonce: u64::MAX,
                code: Some(Bytecode::new_raw(Bytes::from(create_endowment_7()))),
                ..Default::default()
            },
        );

        let (outcome, success) = run_capturing_tx(db, factory, 16);
        assert!(success, "outer zero-value call to factory must succeed");
        // Completion validity is asserted on the REAL `CreateOutcome` the EVM handed `create_end`,
        // not inferred from the flattened stream: the nonce-overflow completion is
        // `is_ok() == true` with `address == None`. The sticky observation count is also read at
        // that same instant — the revocation above must NOT have decremented it, so it still
        // carries the one phantom CREATE observation.
        let (is_ok, address_is_none, observed_at_create_end, address) =
            CREATE_END_OUTCOME.with(|s| s.get());
        assert!(is_ok, "nonce-overflow CREATE completion is is_ok() == true");
        assert!(address_is_none, "nonce-overflow CREATE completion carries address == None");
        assert!(address.is_none(), "the recorded completion address must be None");
        assert_eq!(
            observed_at_create_end, 1,
            "the revoked CREATE observation must stay counted (sticky observed_count)"
        );
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert!(
                    stream.is_empty(),
                    "nonce-overflow CREATE endowment must be revoked; stream must be empty"
                );
            }
            other => panic!("expected Complete(empty) for nonce-overflow CREATE, got {other:?}"),
        }
    }

    #[test]
    fn create2_nonce_overflow_is_ok_true_address_none_revoked() {
        // When a factory's nonce = u64::MAX, a CREATE2 with endowment returns
        // is_ok=true, address=None (nonce-overflow, not EIP-684 collision where is_ok=false).
        // The D7a path revokes the endowment; with a zero-value outer call, the stream is empty.
        let factory = Address::from([0xF7; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(
            factory,
            AccountInfo {
                balance: U256::from(1_000u64),
                nonce: u64::MAX,
                code: Some(Bytecode::new_raw(Bytes::from(create2_endowment_7()))),
                ..Default::default()
            },
        );

        let (outcome, success) = run_capturing_tx(db, factory, 16);
        assert!(success, "outer zero-value call to factory must succeed");
        // Same real-completion assertions as the CREATE case, on the CREATE2 path: `is_ok() == true`
        // with `address == None` (nonce overflow, NOT the contract-creation collision case, which
        // reports `is_ok() == false`), and a sticky observation count that survived the revocation.
        let (is_ok, address_is_none, observed_at_create_end, address) =
            CREATE_END_OUTCOME.with(|s| s.get());
        assert!(is_ok, "nonce-overflow CREATE2 completion is is_ok() == true");
        assert!(address_is_none, "nonce-overflow CREATE2 completion carries address == None");
        assert!(address.is_none(), "the recorded completion address must be None");
        assert_eq!(
            observed_at_create_end, 1,
            "the revoked CREATE2 observation must stay counted (sticky observed_count)"
        );
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert!(
                    stream.is_empty(),
                    "nonce-overflow CREATE2 endowment must be revoked; stream must be empty"
                );
            }
            other => panic!("expected Complete(empty) for nonce-overflow CREATE2, got {other:?}"),
        }
    }

    // ── T1 proof-boundary: inactive gate at create-return ────────────────────────────────────────

    #[test]
    fn inactive_create_no_mutation_and_no_derivation_at_create_return() {
        // Inspector is never activated. The `create` hook must push_checkpoint then immediately
        // gate-return without touching `create_recipients` or running address derivation.
        // The CREATE_GATE_SNAPSHOT seam records the state at the exact return point so we can
        // assert cleanliness BEFORE `create_end` fires.
        reset_create_gate_seams();
        let deployer = Address::from([0xDA; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(deployer, account(1_000_000_000, None));
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        // Deliberately do NOT call start_capture().
        evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");
        let inspector = evm.components_mut().1;
        assert!(!inspector.active);
        assert!(inspector.create_recipients.is_empty());
        assert!(inspector.checkpoints.is_empty());
        let (cr_len, cp_len, capture_body_ran, derivation_ran) =
            CREATE_GATE_SNAPSHOT.with(|s| s.get());
        assert_eq!(cr_len, 0, "create_recipients must be untouched at create hook return");
        assert_eq!(cp_len, 0, "checkpoints must be untouched at create hook return");
        assert!(!capture_body_ran, "the capture body must not run while inactive");
        assert!(!derivation_ran, "address derivation must not run while inactive");
    }

    // ── CREATE address derivation: a computed value, distinguishable from "never ran" ────────────

    #[test]
    fn derivation_ran_true_when_journal_read_for_address_derivation() {
        // An armed inspector running a real CREATE-scheme deployment with a non-zero endowment: the
        // `create` hook must report that the created-address derivation actually ran — i.e. that the
        // journal read supplying the deployer nonce executed. The assertion is on the value the hook
        // COMPUTED, so replacing the computation with a literal `false` fails this test.
        reset_create_gate_seams();
        let deployer = Address::from([0xDC; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(deployer, account(1_000_000_000, None));
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let _ =
            evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");

        let (_, _, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
        assert!(capture_body_ran, "the capture body must have run for an armed inspector");
        assert!(
            derivation_ran,
            "the CREATE-scheme created-address derivation reads the caller nonce from the journal; \
             the hook must report that the derivation ran"
        );
    }

    #[test]
    fn derivation_ran_false_when_create_hook_not_hit() {
        // The not-hit sentinel: an inspector that was never armed leaves BOTH the capture-body flag
        // and the derivation flag clear, so `derivation_ran == false` here cannot be confused with
        // the "ran, but no journal read was needed" state (which reports capture_body_ran == true).
        reset_create_gate_seams();
        let deployer = Address::from([0xDD; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(deployer, account(1_000_000_000, None));
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        // Deliberately do NOT call start_capture().
        let _ =
            evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");

        let (_, _, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
        assert!(!capture_body_ran, "the capture body must not run while inactive");
        assert!(!derivation_ran, "the not-hit sentinel must report that no derivation ran");
    }

    // ── distinguish "create hook never invoked" from "invoked while inactive" ──────────────────

    #[test]
    fn non_create_control_leaves_create_hook_hits_zero() {
        // A real transaction with NO `CREATE` at any depth (pure `CALL` only) must leave the
        // create-hook hit counter at zero, which is what makes `derivation_ran == false` in
        // `derivation_ran_false_when_create_hook_not_hit` a statement about a hook that DID run
        // (because the create hook was never entered) rather than one that never fired.
        reset_create_gate_seams();
        let contract = Address::from([0xC3; 20]);
        let dst = Address::from([0xD4; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(contract, account(1_000, Some(call_value_1_to(dst))));

        let (outcome, success) = run_capturing_tx(db, contract, 16);
        assert!(success, "pure-CALL tx must succeed");
        assert_eq!(
            CREATE_HOOK_HITS.with(|c| c.get()),
            0,
            "a transaction with no CREATE at any depth must never invoke the create hook"
        );
        assert!(
            matches!(outcome, CaptureOutcome::Complete(ref s) if s.len() == 1),
            "the internal CALL transfer really ran: {outcome:?}"
        );
    }

    #[test]
    fn inactive_create_hook_hit_is_nonzero_with_derivation_false() {
        // The other side of the control above: an UNARMED inspector that really does execute a
        // CREATE records a hook hit while reporting neither a capture body nor a derivation —
        // proving `derivation_ran_false_when_create_hook_not_hit`'s `false` comes from a hook that
        // DID run, not one that never fired.
        reset_create_gate_seams();
        let deployer = Address::from([0xD6; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(deployer, account(1_000_000_000, None));
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        // Deliberately do NOT call start_capture().
        evm.transact(create_tx_with_value(deployer, U256::from(5u64))).expect("tx executes");

        assert!(
            CREATE_HOOK_HITS.with(|c| c.get()) > 0,
            "the create hook must have been invoked for a real CREATE, even while unarmed"
        );
        let (_, _, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
        assert!(!capture_body_ran, "the capture body must not run while inactive");
        assert!(!derivation_ran, "no address derivation runs while inactive");
    }

    #[test]
    fn derivation_ran_false_but_capture_body_ran_for_create2() {
        // The third state, and the reason the sentinel is not a bare bool: CREATE2 derives its
        // address from salt + init-code hash and reads no journal state, so the capture body runs
        // while the derivation flag stays clear. A reader can therefore always distinguish
        // "hook never ran" (both false) from "ran, no journal read needed" (body true, derive false)
        // from "derived" (both true).
        reset_create_gate_seams();
        let factory = Address::from([0xDE; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(caller(), account(1_000_000_000, None));
        db.insert_account_info(factory, account(1_000, Some(create2_endowment_7())));

        let (outcome, success) = run_capturing_tx(db, factory, 16);
        assert!(success, "CREATE2 endowment tx must succeed");
        assert!(
            matches!(outcome, CaptureOutcome::Complete(_)),
            "CREATE2 endowment must complete, got {outcome:?}"
        );

        let (_, _, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
        assert!(capture_body_ran, "the capture body must have run for an armed inspector");
        assert!(
            !derivation_ran,
            "CREATE2 needs no journal read for the created address, so the derivation flag is clear"
        );
        // The hook was genuinely hit, so this `false` can never be misread as
        // "the hook never ran" — only as "ran, no journal read needed".
        assert!(
            CREATE_HOOK_HITS.with(|c| c.get()) > 0,
            "the create hook must have been invoked for a real CREATE2 endowment"
        );
    }

    // ── T1 proof-boundary: create_recipients balanced inside create_end ──────────────────────────

    #[test]
    fn create_recipients_balanced_before_finish_capture() {
        // Each `create` hook push must be balanced by `create_end`'s pop, not deferred to
        // `finish_capture`. The CREATE_END_BALANCE seam captures the lengths immediately after
        // the last pop inside `create_end` — before the test calls `finish_capture` — so the
        // assertion is on the boundary that matters: within the EVM frame, not at cleanup.
        let deployer = Address::from([0xDB; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(deployer, account(1_000_000_000, None));
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result =
            evm.transact(create_tx_with_value(deployer, U256::from(7u64))).expect("tx executes");
        // Read seam BEFORE finish_capture — proves push/pop balanced in create_end itself.
        let (cr_len, cp_len) = CREATE_END_BALANCE.with(|s| s.get());
        assert_eq!(
            cr_len, 0,
            "create_recipients must be empty after create_end — push/pop balanced"
        );
        assert_eq!(cp_len, 0, "checkpoints must be empty after create_end — push/pop balanced");
        let logs = result.result.logs();
        let _outcome = evm.components_mut().1.finish_capture(logs);
    }

    // ── T1 proof-boundary: abort_capture clears mid-window state ─────────────────────────────────

    #[test]
    fn abort_capture_mid_window_leaves_state_clean() {
        // abort_capture must reset all inspector state even when called with entries mid-flight:
        // a partially-filled queue, an open checkpoint, and an in-progress create_recipients entry.
        let mut inspector = RcsInspector::new(10);
        inspector.start_capture();
        inspector.push_checkpoint();
        inspector.observe_native_transfer(
            Address::from([0x01u8; 20]),
            Address::from([0x02u8; 20]),
            U256::from(5u64),
            NativeTransferKind::Call,
        );
        inspector.create_recipients.push(Some(Address::from([0x03u8; 20])));
        inspector.abort_capture();
        assert!(!inspector.active, "must be inactive after abort");
        assert!(inspector.queue.is_empty(), "queue must be cleared by abort");
        assert!(inspector.checkpoints.is_empty(), "checkpoints must be cleared by abort");
        assert_eq!(inspector.observed_count, 0, "observed_count must be reset by abort");
        assert!(!inspector.overflowed, "overflow flag must be reset by abort");
        assert!(
            inspector.create_recipients.is_empty(),
            "create_recipients must be cleared by abort"
        );
    }

    // ── T1 proof-boundary: two consecutive captures are independent ───────────────────────────────

    #[test]
    fn two_consecutive_txs_capture_independently() {
        // State from one capture window must not carry over into the next. tx1 captures one
        // internal transfer; tx2 hits an empty contract (no code, no value) and must produce an
        // empty stream — not the leftovers from tx1.
        let sender1 = Address::from([0xA4; 20]);
        let sender2 = Address::from([0xA5; 20]);
        let target = Address::from([0xC4; 20]);
        let recv = Address::from([0xC5; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(sender1, account(1_000_000_000, None));
        db.insert_account_info(sender2, account(1_000_000_000, None));
        db.insert_account_info(target, account(1_000, Some(call_with_value_to(5, recv))));
        db.insert_account_info(recv, account(0, None));
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result1 = evm.transact(call_tx_from(sender1, target)).expect("tx1 executes");
        let outcome1 = evm.components_mut().1.finish_capture(result1.result.logs());
        match outcome1 {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(stream.len(), 1, "tx1 must produce 1 transfer")
            }
            other => panic!("expected Complete with 1 transfer for tx1, got {other:?}"),
        }
        evm.components_mut().1.start_capture();
        let result2 = evm.transact(call_tx_from(sender2, recv)).expect("tx2 executes");
        let outcome2 = evm.components_mut().1.finish_capture(result2.result.logs());
        match outcome2 {
            CaptureOutcome::Complete(stream) => {
                assert!(stream.is_empty(), "tx2 must produce 0 transfers — no carry-over from tx1")
            }
            other => panic!("expected Complete(empty) for tx2, got {other:?}"),
        }
    }

    // ── T2 proof-boundary: EIP-7702 origin-is-delegated path ─────────────────────────────────────

    #[test]
    fn eip7702_origin_itself_delegated_runs_inner_call_and_create() {
        // EIP-7702 case where the tx origin (signer) IS the delegation authority. The tx target is
        // also the origin, so the delegated code runs in the origin's context. The delegated code
        // (delegated_call_and_create) issues CALL(1 wei) + CREATE(1 wei), both attributed to the
        // origin. Two synthetic logs must be captured with from==origin on both.
        //
        // Hardfork basis: OpSpecId::JOVIAN maps through op-revm to SpecId::PRAGUE, enabling
        // EIP-7702 authority-tuple resolution.
        let delegate_impl = Address::from([0xE3; 20]);
        let call_recipient = Address::from([0xE4; 20]);
        // When origin == authority == tx sender, the EVM increments the sender's nonce from 0
        // to 1 before processing the authorization list. auth.nonce must therefore be 1 so that
        // the nonce check inside authorization processing succeeds (auth.nonce == 1 == origin.nonce
        // after increment). With nonce 0, the check would fail (0 != 1) and no delegation code
        // would be installed, causing the call to origin to be a no-op.
        let auth = Authorization { chain_id: U256::ZERO, address: delegate_impl, nonce: 1 }
            .into_signed(Signature::test_signature());
        let origin = auth.recover_authority().expect("deterministic authority from test_signature");
        let mut db = InMemoryDB::default();
        db.insert_account_info(origin, account(1_000_000, None));
        db.insert_account_info(
            delegate_impl,
            account(0, Some(delegated_call_and_create(call_recipient))),
        );
        db.insert_account_info(call_recipient, account(0, None));
        let tx = TxEip7702 {
            chain_id: 1,
            nonce: 0,
            gas_limit: 2_000_000,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            to: origin, // origin calls itself — delegation applies
            value: U256::ZERO,
            authorization_list: vec![auth],
            input: Default::default(),
            access_list: Default::default(),
        }
        .into_signed(Signature::new(
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        let op_tx = OpTx::from_recovered_tx(&tx, origin);
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            db,
            EvmEnv::new(
                CfgEnv::new_with_spec(OpSpecId::JOVIAN),
                BlockEnv { gas_limit: 30_000_000, ..Default::default() },
            ),
            RcsInspector::new(16),
        );
        evm.components_mut().1.start_capture();
        let result = evm.transact(op_tx).expect("tx executes");
        assert!(result.result.is_success(), "the delegated execution must succeed");
        // The EXACT deployed address the EVM reported for the inner CREATE, captured from the real
        // `CreateOutcome` handed to `create_end`. Asserting against this rules out every "any
        // plausible address" relaxation: a fourth address that is not the real one fails here.
        let (create_is_ok, _, _, deployed_address) = CREATE_END_OUTCOME.with(|s| s.get());
        assert!(create_is_ok, "the inner CREATE must report a successful completion");
        let deployed = deployed_address.expect("a successful CREATE reports its deployed address");
        let outcome = evm.components_mut().1.finish_capture(result.result.logs());
        match outcome {
            CaptureOutcome::Complete(stream) => {
                assert_eq!(
                    stream.len(),
                    2,
                    "origin-delegated code must capture CALL(1) + CREATE(1)"
                );
                for log in &stream {
                    assert_eq!(log.address, NATIVE_ASSET_ADDRESS);
                    assert_eq!(
                        log.topics()[1],
                        origin.into_word(),
                        "transfer.from must be origin (signer itself carries the delegation)"
                    );
                    assert_eq!(
                        log.data.data.as_ref(),
                        &U256::from(1u64).to_be_bytes::<32>(),
                        "each delegated native movement carries exactly 1 wei"
                    );
                }
                // Per-transfer recipients: the CALL log pays the call recipient, and the CREATE log
                // pays the deployed contract — the exact address, not merely "some other address".
                assert_eq!(
                    stream[0].topics()[2],
                    call_recipient.into_word(),
                    "CALL transfer.to must be the call recipient"
                );
                assert_eq!(
                    stream[1].topics()[2],
                    deployed.into_word(),
                    "CREATE transfer.to must be the exact deployed address"
                );
                assert_ne!(
                    stream[1].topics()[2],
                    Address::ZERO.into_word(),
                    "CREATE transfer.to must not be the zero address"
                );
                assert_ne!(
                    stream[1].topics()[2],
                    origin.into_word(),
                    "CREATE transfer.to must not be the caller"
                );
                assert_ne!(
                    stream[1].topics()[2],
                    call_recipient.into_word(),
                    "CREATE transfer.to must not be the CALL recipient"
                );
            }
            other => panic!("expected Complete with 2 transfers, got {other:?}"),
        }
    }
}
