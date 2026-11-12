//! Shared RCS capture: the EVM inspector that observes internal native-token (OKB) balance
//! transfers and rule-configured call frames during simulation, plus the capture-control seam and
//! the panic-safe RAII guard that drive it.
//!
//! This module exists so every execution path that must identify executed code — the payload
//! builder's candidate loop, the deposit/sequencer path, and the audit RPC's simulation — runs
//! the **same** inspector from **one** implementation. Two inspectors that merely behave alike
//! would drift the moment one of them changes, which would make "equal identification" a
//! coincidence rather than a property.
//!
//! The module is deliberately EVM-**agnostic**: it owns [`RcsCaptureScope`] and [`CaptureGuard`]
//! but names no concrete EVM type, so it needs no `alloy-op-evm` dependency. Each consuming
//! crate supplies a local newtype adapter over its own EVM value, which is what keeps
//! `impl RcsCaptureScope for <that crate's EVM>` legal under the orphan rule.
//!
//! Builder-side capture of internal native-token (OKB) balance transfers during simulation.
//!
//! This module adapts EVM execution to the risk-control screening input. During the real-user
//! candidate simulation the inspector records internal native-value movements — non-zero internal
//! `CALL` transfers, `CREATE`/`CREATE2` endowments, and `SELFDESTRUCT` beneficiary transfers — as
//! virtual `Transfer(address,address,uint256)` logs whose address is the native-asset identifier,
//! interleaved in execution order with the real EVM logs. The flattened stream is handed to
//! screening only; it never reaches a receipt, RPC log, bloom, the indexer, or consensus state.
//!
//! The same inspector also observes **call frames of rule-configured addresses**, exposing the
//! runtime code hash that actually executed and, for the selected address's own first-level
//! `DELEGATECALL`, the target address and the code hash that executed there. Only frames touching
//! the active snapshot's selector set are observed, so observation cost scales with operator
//! configuration rather than with transaction call depth. An empty selector set makes the whole
//! path inert.
//!
//! The screening modules stay free of any EVM / frame-lifecycle dependency: the inspector, its
//! capture-control seam, and the capture outcomes are all owned by this module. The pure
//! observation value type is owned by the screening crate so both the builder and the RPC layer
//! can hand screening the same type.
use std::collections::HashSet;
use std::sync::Arc;

use crate::callstack::{CallObservation, SnapshotStamp};
use alloy_primitives::{address, keccak256, Address, Bytes, Log, LogData, B256, U256};
use revm::{
    bytecode::opcode,
    context_interface::{ContextTr, CreateScheme, JournalTr},
    inspector::{JournalExt, NoOpInspector},
    interpreter::{
        interpreter_types::{Jumps, LegacyBytecode},
        CallInputs, CallOutcome, CallScheme, CreateInputs, CreateOutcome, Interpreter,
        InterpreterTypes,
    },
    Inspector,
};

/// Hard ceiling on concurrently open observed frames.
///
/// The EVM's own call-depth limit is 1024, so a balanced frame stack can never legitimately grow
/// past that plus the outermost transaction frame. A stack that does is an invariant failure, not
/// a deep transaction, and it fails closed rather than silently dropping frames — a dropped frame
/// would turn an unobserved call into an apparent no-match.
pub const MAX_OPEN_FRAMES: usize = 1025;

/// The native-asset identifier used as the `log.address` of every virtual native transfer.
///
/// This is a native-coin identifier matched by **address value** (not checksum-string case), never
/// an ERC20 contract: the screening side never calls `decimals`/`balanceOf`/`transfer` on it.
pub const NATIVE_ASSET_ADDRESS: Address = address!("EeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE");

/// Which observation families one capture window produces.
///
/// The call-frame logic is the SAME code in both modes — that is what keeps identification
/// structurally equal across paths rather than equal by coincidence. Only the native-transfer
/// side is switched off, and only because paths that do not consume virtual native logs must not
/// pay a native budget that could fail their capture for a reason unrelated to call frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureMode {
    /// Virtual native-transfer logs interleaved with real logs, the real-log cross-check, the
    /// native budget, AND call-frame observations. Exactly the pre-existing behaviour; this is
    /// what the payload builder's candidate loop uses, bit-for-bit unchanged.
    #[default]
    NativeAndCallstack,
    /// Call-frame observations only. `observe_native_transfer`, `record_real_log` and
    /// `push_checkpoint` become genuine no-ops, the queue stays empty, `observed_count` and
    /// `overflowed` are never touched, and `finish_capture` skips both the native-budget check
    /// and the real-log cross-check. The only live budget is the callstack observation budget.
    ///
    /// A consumer in this mode screens against the **raw** EVM logs of its own execution result,
    /// never against `CaptureOutcome::Complete`'s `logs` (which is empty here by construction).
    CallstackOnly,
}

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
    /// The observed-frame stack exceeded [`MAX_OPEN_FRAMES`], so frame parentage — and therefore
    /// direct-delegatecall discrimination — can no longer be established. `depth` is the open
    /// frame count at detection.
    FrameStackOverflow { depth: usize },
    /// The observed-frame stack was unbalanced at the end of the capture window: a frame entry
    /// was never matched by its exit, so the set may be missing or misattributing observations.
    FrameStackUnbalanced { remaining: usize },
}

/// The outcome of finishing a capture window. Never an `Option`: an enabled filter can never
/// silently fall back to raw `result.logs()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// Emitted only by the no-op inspector (filter disabled); the caller uses raw `result.logs()`.
    Passthrough,
    /// A validated, flattened ordered stream of real + virtual logs plus the ordered call-frame
    /// observations; the RealLog cross-check passed and both observation sets are complete.
    ///
    /// `observations` may legitimately be empty: that means capture ran and genuinely saw no
    /// frame touching the selector set, which is the ordinary no-match case. It is never used to
    /// represent "capture did not run".
    Complete { logs: Vec<Log>, observations: CapturedObservations },
    /// A per-transaction observation budget was exceeded. Fail closed.
    LimitExceeded { attempted: usize, limit: usize, budget: ObservationBudget },
    /// An observation invariant failed. Fail closed.
    InvariantViolation { reason: CaptureInvariantError },
}

/// Which per-transaction observation budget overflowed. A fixed, low-cardinality enum used for
/// bounded diagnostics, so the two capture budgets are never conflated in an operator's view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationBudget {
    /// Internal native-token transfer observations.
    NativeTransfer,
    /// Call-frame observations.
    Callstack,
}

impl ObservationBudget {
    /// Stable lowercase token for bounded diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            ObservationBudget::NativeTransfer => "native_transfer",
            ObservationBudget::Callstack => "callstack",
        }
    }
}

/// A complete call-frame observation set together with the identity of the rule snapshot whose
/// selector set configured its capture.
///
/// Screening compares the stamp against its own snapshot and fails closed on a mismatch: an
/// observation set captured under a different selector set cannot distinguish "this frame did not
/// happen" from "this frame was never observed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedObservations {
    pub stamp: SnapshotStamp,
    pub items: Vec<CallObservation>,
}

impl CapturedObservations {
    pub fn new(stamp: SnapshotStamp, items: Vec<CallObservation>) -> Self {
        Self { stamp, items }
    }
}

/// The capture selector set plus the identity of the snapshot it came from, built once per rule
/// snapshot and handed to the inspector.
///
/// Carrying both together is what makes the same-snapshot requirement structural: an inspector
/// cannot be configured with one snapshot's addresses and then stamp its output with another's.
#[derive(Debug, Clone, Default)]
pub struct CallstackCaptureConfig {
    stamp: Option<SnapshotStamp>,
    addresses: Arc<HashSet<Address>>,
}

impl CallstackCaptureConfig {
    /// Builds a configuration from a snapshot's selector addresses and its stamp.
    pub fn new(stamp: SnapshotStamp, addresses: impl IntoIterator<Item = Address>) -> Self {
        Self { stamp: Some(stamp), addresses: Arc::new(addresses.into_iter().collect()) }
    }

    /// Whether any address is observed. `false` short-circuits every observation hook.
    pub fn is_inert(&self) -> bool {
        self.addresses.is_empty()
    }

    fn observes(&self, address: &Address) -> bool {
        self.addresses.contains(address)
    }

    /// The stamp this configuration will attach to its observations. Test accessor: production
    /// code never needs to read it back, because the inspector stamps its own output.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn stamp_for_test(&self) -> Option<SnapshotStamp> {
        self.stamp
    }
}

/// Builder-internal capture-control seam. These methods are **not** part of revm's `Inspector`
/// trait, so a generic execution loop bounds its inspector on `Inspector + RcsCaptureControl` to
/// drive capture without a runtime downcast. Only this module links revm; the screening modules
/// stay free of it.
pub trait RcsCaptureControl {
    /// Installs the capture configuration the NEXT capture window will use.
    ///
    /// Called once per candidate, before the window opens, because the snapshot a candidate is
    /// screened against is a property of the candidate and not of the block: a fresh candidate
    /// uses the snapshot pinned for the block, while a buffered one keeps the snapshot saved on
    /// its pool entry. Configuring per candidate is what keeps capture and screening on the same
    /// snapshot without ever relaxing the stamp check.
    fn configure_callstack_capture(&mut self, config: CallstackCaptureConfig);
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
    fn configure_callstack_capture(&mut self, _config: CallstackCaptureConfig) {}

    fn start_capture(&mut self) {}

    fn finish_capture(&mut self, _result_logs: &[Log]) -> CaptureOutcome {
        CaptureOutcome::Passthrough
    }

    fn abort_capture(&mut self) {}
}

/// The capture lifecycle, expressed without naming an EVM.
///
/// Moved here from the builder so the audit-RPC path can reuse the **same** guard discipline.
/// This module owns the trait but not any EVM type, so it deliberately provides **no**
/// implementation: `impl RcsCaptureScope for SomeEvm` here would require depending on
/// `alloy-op-evm` and pinning one concrete EVM type. Each consuming crate instead wraps its own
/// EVM value in a newtype it owns — `impl ForeignTrait for LocalType` is permitted, so that is
/// what keeps both call sites legal under the orphan rule.
pub trait RcsCaptureScope {
    fn scope_configure_callstack_capture(&mut self, config: CallstackCaptureConfig);
    fn scope_start_capture(&mut self);
    fn scope_finish_capture(&mut self, result_logs: &[Log]) -> CaptureOutcome;
    fn scope_abort_capture(&mut self);
}

/// Panic-safe RAII capture guard. Constructing it calls `start_capture`; on `Drop` it calls
/// `abort_capture` UNLESS it was explicitly disarmed by a successful [`CaptureGuard::finish`].
/// This makes cleanup impossible to forget on any early return or unwind — a panic caught
/// upstream still aborts exactly once — so correctness does not rest on caller discipline, and
/// the guard never aborts twice (`finish` disarms before returning; the `Err`/panic path aborts
/// once via `Drop`).
#[derive(Debug)]
pub struct CaptureGuard<'a, S: RcsCaptureScope> {
    scope: &'a mut S,
    armed: bool,
}

impl<'a, S: RcsCaptureScope> CaptureGuard<'a, S> {
    /// Arms capture: calls `start_capture` on construction.
    pub fn new(scope: &'a mut S) -> Self {
        scope.scope_start_capture();
        Self { scope, armed: true }
    }

    /// The armed scope, for driving the real `transact` through the adapter.
    pub fn scope_mut(&mut self) -> &mut S {
        self.scope
    }

    /// Closes the capture window normally and disarms the guard so its `Drop` is a no-op —
    /// exactly one lifecycle-ending call (this `finish`), never a `finish` followed by a `Drop`
    /// abort.
    pub fn finish(mut self, result_logs: &[Log]) -> CaptureOutcome {
        let outcome = self.scope.scope_finish_capture(result_logs);
        self.armed = false;
        outcome
    }
}

impl<S: RcsCaptureScope> Drop for CaptureGuard<'_, S> {
    fn drop(&mut self) {
        if self.armed {
            self.scope.scope_abort_capture();
        }
    }
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
#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub static FORCE_REALLOG_MISMATCH: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

// Test-only "forced CREATE address mismatch" seam (D7b). When set, the next `create` hook on an
// active inspector pushes `Address::repeat_byte(0xFE)` as the expected recipient instead of the
// real derived address, so `create_end` sees `e != a` on a genuinely successful CREATE. One-shot:
// the flag is consumed (replaced with `false`) by the `create` hook so only the armed frame is
// affected. Kept as `//` so the struct doc below is not orphaned onto the macro.
#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub static FORCE_CREATE_ADDRESS_MISMATCH: std::cell::Cell<bool> =
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
// Never set in production (the whole seam is `#[cfg(any(test, feature = "test-utils"))]`).
#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub(crate) static CREATE_GATE_SNAPSHOT: std::cell::Cell<(usize, usize, bool, bool)> =
        const { std::cell::Cell::new((0, 0, false, false)) };
}

// Test-only CREATE-completion outcome seam. The `create_end` hook records the REAL `CreateOutcome`
// it was handed, so tests can assert the actual completion validity instead of inferring it from the
// flattened capture stream. Tuple layout:
// `(is_ok, address_is_none, observed_count, address)`. `address` is the exact deployed address the
// EVM reported, which the CREATE-recipient assertions compare against. Never set in production.
#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub(crate) static CREATE_END_OUTCOME: std::cell::Cell<(bool, bool, usize, Option<Address>)> =
        const { std::cell::Cell::new((false, false, 0, None)) };
}

// Test-only push/pop balance seam (T1-BALANCE). The `create_end` hook writes here after every
// `pop` and `pop_checkpoint` call, before `finish_capture` can run. Tuple layout:
// `(create_recipients_len, checkpoints_len)`. Proves that each `create` push is balanced by
// a `create_end` pop within the same EVM frame, not deferred to `finish_capture`. Never set
// in production.
#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub(crate) static CREATE_END_BALANCE: std::cell::Cell<(usize, usize)> =
        const { std::cell::Cell::new((0, 0)) };
}

// Test-only create-hook invocation counter. Incremented as the FIRST statement
// of the `create` hook, before the `!self.active` gate, so `0` means the hook was never invoked at
// all for this transaction — distinguishable from `CREATE_GATE_SNAPSHOT`'s `derivation_ran == false`,
// which is ambiguous between "never hit" and "hit while inactive" without this counter. Never set
// in production (the whole seam is `#[cfg(any(test, feature = "test-utils"))]`).
#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub(crate) static CREATE_HOOK_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only reset for the create-hook gate seams. Clears [`CREATE_HOOK_HITS`]
/// and [`CREATE_GATE_SNAPSHOT`] so a stale tuple left by an earlier test on the same thread (`cargo
/// test` reuses threads across tests, unlike `nextest`'s process-per-test) cannot make a later
/// assertion pass for the wrong reason.
#[cfg(any(test, feature = "test-utils"))]
pub fn reset_create_gate_seams() {
    CREATE_HOOK_HITS.with(|c| c.set(0));
    CREATE_GATE_SNAPSHOT.with(|s| s.set((0, 0, false, false)));
}

// Test-only counter of call-frame bytecode hash computations. Incremented in `initialize_interp`
// immediately before the `keccak256` over the frame's bytecode, i.e. only once the gate has decided
// the hash is actually wanted. `0` therefore means no hashing work was done at all — which is what
// the "inert selector set / inactive inspector does no callstack work" regressions assert, instead
// of trying to measure elapsed time. Never set in production (the whole seam is `#[cfg(any(test, feature = "test-utils"))]`).
#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub static CALLSTACK_HASH_COMPUTATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Test-only reset for [`CALLSTACK_HASH_COMPUTATIONS`], for the same reason
/// [`reset_create_gate_seams`] exists: `cargo test` reuses threads across tests, so a stale count
/// from an earlier test could make a later `== 0` assertion pass for the wrong reason.
#[cfg(any(test, feature = "test-utils"))]
pub fn reset_callstack_hash_counter() {
    CALLSTACK_HASH_COMPUTATIONS.with(|c| c.set(0));
}

/// Test-only count of call-frame hash computations since the last reset.
#[cfg(any(test, feature = "test-utils"))]
pub fn callstack_hash_computations() -> usize {
    CALLSTACK_HASH_COMPUTATIONS.with(|c| c.get())
}

// Test-only SELFDESTRUCT-watermark gate snapshot, in the same style as `CREATE_GATE_SNAPSHOT` /
// `CREATE_HOOK_HITS`. Tuple layout: `(hook_hits, active_branch_ran, journal_read_ran)`.
// - `hook_hits` — incremented in `step`, as the FIRST thing done once the about-to-execute opcode
//   is recognized as `SELFDESTRUCT`, before the `!self.active` gate. `0` means "a SELFDESTRUCT
//   opcode's `step` was never reached"; distinguishable from "reached while inactive".
// - `active_branch_ran` — set once the `!self.active` gate has been passed for that opcode.
// - `journal_read_ran` — set immediately before the watermark journal read itself.
// Never set in production (the whole seam is `#[cfg(any(test, feature = "test-utils"))]`).
#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub(crate) static SELFDESTRUCT_GATE_SNAPSHOT: std::cell::Cell<(usize, bool, bool)> =
        const { std::cell::Cell::new((0, false, false)) };
}

/// Test-only reset for [`SELFDESTRUCT_GATE_SNAPSHOT`], for the same reason
/// [`reset_create_gate_seams`] exists: `cargo test` reuses threads across tests.
#[cfg(any(test, feature = "test-utils"))]
pub fn reset_selfdestruct_gate_seams() {
    SELFDESTRUCT_GATE_SNAPSHOT.with(|s| s.set((0, false, false)));
    SELFDESTRUCT_CALLBACK_HITS.with(|c| c.set(0));
    SELFDESTRUCT_STEP_OBSERVATIONS.with(|o| o.borrow_mut().clear());
}

// Test-only direct evidence that the `selfdestruct` hook itself was entered. Needed because the
// absence of a `SelfDestruct`-kind observation in the final stream is NOT proof the callback never
// fired: a reverted frame can remove logs on its own. Incremented as the FIRST statement of the
// hook, before the `active` gate, so `0` means "never entered" -- distinguishable from "entered
// while inactive". Never set in production (the whole seam is `#[cfg(any(test, feature = "test-utils"))]`).
#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub(crate) static SELFDESTRUCT_CALLBACK_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// Test-only direct observation of the victim SELFDESTRUCT's actual `InstructionResult`, recorded
// from inside the production `step` / `step_end` hooks rather than inferred from
// `SELFDESTRUCT_CALLBACK_HITS`: that counter is 0 for every result other than `SelfDestruct`,
// including the aborted paths this observation exists to distinguish from the measured OutOfGas
// halt. `RefCell`, not `Cell` like the two seams above, because a `Vec` is not `Copy`.
#[cfg(any(test, feature = "test-utils"))]
use revm::interpreter::{interpreter_types::LoopControl, InstructionResult};

/// One `step`-side record, completed by `step_end` once the halting instruction's result is known.
/// `end: None` until `step_end` completes it; distinguishable from "completed with no result" by
/// construction, so a loop that breaks before `step_end` runs leaves a record that is detectably
/// incomplete rather than silently matching a bare length check.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelfdestructStepObservation {
    pub(crate) opcode: u8,
    pub(crate) gas_remaining_before: u64,
    pub(crate) end: Option<SelfdestructStepEnd>,
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelfdestructStepEnd {
    pub(crate) depth: usize,
    pub(crate) instruction_result: Option<InstructionResult>,
    pub(crate) gas_remaining_after: u64,
    pub(crate) journal_delta: usize,
}

#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    pub(crate) static SELFDESTRUCT_STEP_OBSERVATIONS: std::cell::RefCell<Vec<SelfdestructStepObservation>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Pushes a new partial record from `step`'s existing gated region. Test-only.
#[cfg(any(test, feature = "test-utils"))]
fn record_selfdestruct_step(opcode: u8, gas_remaining_before: u64) {
    SELFDESTRUCT_STEP_OBSERVATIONS.with(|o| {
        o.borrow_mut().push(SelfdestructStepObservation {
            opcode,
            gas_remaining_before,
            end: None,
        });
    });
}

/// Completes the most recently pushed record from inside `step_end`'s existing
/// `if let Some(pre) = self.selfdestruct_journal_mark.take()` branch. Test-only.
#[cfg(any(test, feature = "test-utils"))]
fn record_selfdestruct_step_end(
    depth: usize,
    instruction_result: Option<InstructionResult>,
    gas_remaining_after: u64,
    journal_delta: usize,
) {
    SELFDESTRUCT_STEP_OBSERVATIONS.with(|o| {
        if let Some(last) = o.borrow_mut().last_mut() {
            last.end = Some(SelfdestructStepEnd {
                depth,
                instruction_result,
                gas_remaining_after,
                journal_delta,
            });
        }
    });
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
    /// Selector addresses plus the identity of the snapshot they came from.
    callstack: CallstackCaptureConfig,
    /// Open call/create frames, innermost last. Every entered frame is tracked — not just
    /// observed ones — because direct-delegatecall discrimination needs the *parent* frame's
    /// execution identity, and a parent is frequently an unobserved address.
    frames: Vec<FrameRecord>,
    /// Committed call-frame observations in execution order. Deliberately never truncated on
    /// revert: code that executed and then reverted still executed, and its execution identity
    /// is exactly what a risk rule needs to see.
    observations: Vec<CallObservation>,
    /// Cumulative count of committed call-frame observations; never rolls back.
    callstack_observed_count: usize,
    /// Sticky flag set once `callstack_observed_count` exceeds `callstack_limit`.
    callstack_overflowed: bool,
    /// The configured per-transaction call-frame observation budget.
    callstack_limit: usize,
    /// Sticky frame-stack invariant failure, surfaced by `finish_capture`.
    frame_invariant: Option<CaptureInvariantError>,
    /// Which observation families this inspector produces. Fixed at construction: switching it
    /// inside a window could leave a half-built queue that the real-log cross-check would then
    /// reject for the wrong reason.
    mode: CaptureMode,
}

/// One open call or create frame.
///
/// `target` is the frame's execution identity and `bytecode` is whose code it runs; a
/// `DELEGATECALL` preserves the delegating context's `target` while changing `bytecode`, which is
/// precisely what distinguishes a proxy's own direct delegatecall from one made inside the
/// implementation's business logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameRecord {
    target: Address,
    bytecode: Address,
    /// `true` for a `CREATE`/`CREATE2` frame. A create frame executes init code, not the
    /// deployed runtime code, so it neither produces an observation nor acts as the parent of a
    /// direct-delegatecall observation: the only code hash available for it would be creation
    /// bytecode, which must never be reported as a runtime code hash.
    is_create: bool,
    /// Hash of the bytecode this frame actually loaded and ran. `None` until the frame is
    /// confirmed to execute.
    code_hash: Option<B256>,
    /// Observation staged at frame entry, promoted only once the frame really executes.
    pending: Option<PendingObservation>,
}

/// An observation staged when a frame is entered but not yet confirmed to execute.
///
/// The `call` hook fires *before* the frame is initialized, and the frame can still fail to
/// execute afterwards — a depth limit, a failed value transfer, a precompile, or empty code all
/// return without ever creating an interpreter. Emitting at hook arrival would therefore report
/// calls that never ran. Each variant carries everything known at entry; the code hash that is
/// only knowable from the executing frame is filled in at commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingObservation {
    /// The selected address is about to run its own code.
    Call { address: Address },
    /// The selected address's own code is about to delegatecall `impl_address`.
    DirectDelegatecall { address: Address, code_hash: B256, impl_address: Address },
}

impl RcsInspector {
    /// Creates an inactive inspector with the given native-transfer budget and no call-frame
    /// selectors, leaving the callstack path inert.
    ///
    /// Test-only: production always builds the inspector from a pinned rule snapshot through
    /// [`Self::with_callstack`], so that the selector set and the stamp it will be screened
    /// against can never come from two different snapshots.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn new(max_native_transfers: usize) -> Self {
        Self::with_callstack(max_native_transfers, usize::MAX, CallstackCaptureConfig::default())
    }

    /// Creates an inactive inspector with both observation budgets and a capture selector set.
    ///
    /// The selector set and the snapshot stamp travel together in `callstack`, so the inspector
    /// cannot stamp its output with a snapshot other than the one that configured it.
    pub fn with_callstack(
        max_native_transfers: usize,
        max_callstack_observations: usize,
        callstack: CallstackCaptureConfig,
    ) -> Self {
        Self::with_mode(
            max_native_transfers,
            max_callstack_observations,
            callstack,
            CaptureMode::NativeAndCallstack,
        )
    }

    /// Creates an inactive inspector that observes **only** call frames.
    ///
    /// The native-transfer budget is deliberately not a parameter: in this mode it is neither
    /// applicable nor consulted, so accepting one would invite a caller to believe it does
    /// something. `usize::MAX` is stored purely so the field has a value.
    pub fn callstack_only(
        max_callstack_observations: usize,
        callstack: CallstackCaptureConfig,
    ) -> Self {
        Self::with_mode(
            usize::MAX,
            max_callstack_observations,
            callstack,
            CaptureMode::CallstackOnly,
        )
    }

    /// The one real constructor. Both budgets plus the selector set, the snapshot stamp it
    /// travels with, and the capture mode.
    pub fn with_mode(
        max_native_transfers: usize,
        max_callstack_observations: usize,
        callstack: CallstackCaptureConfig,
        mode: CaptureMode,
    ) -> Self {
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
            callstack,
            frames: Vec::new(),
            observations: Vec::new(),
            callstack_observed_count: 0,
            callstack_overflowed: false,
            callstack_limit: max_callstack_observations,
            frame_invariant: None,
            mode,
        }
    }

    /// Whether the native-transfer family is captured at all in this window.
    ///
    /// Verified against the pre-existing code: `callstack_capture_armed()` gated only the
    /// callstack work, while `observe_native_transfer`, `record_real_log` and `push_checkpoint`
    /// gated on `self.active` alone, `start_capture` set `active` unconditionally, and
    /// `finish_capture` reported a `NativeTransfer` budget overflow. So attaching the inspector
    /// to a path that does not consume virtual native logs would have built a mixed queue,
    /// consumed the native budget, and could have failed the capture for a reason unrelated to
    /// call-frame selectors. This gate is what prevents that.
    fn native_capture_armed(&self) -> bool {
        self.active && matches!(self.mode, CaptureMode::NativeAndCallstack)
    }

    /// The mode this inspector was constructed with.
    pub fn mode(&self) -> CaptureMode {
        self.mode
    }

    /// Mode-only form of [`Self::native_capture_armed`], for `finish_capture`, which must decide
    /// which checks apply independently of the `active` flag it is about to clear.
    fn native_capture_armed_at_finish(&self) -> bool {
        matches!(self.mode, CaptureMode::NativeAndCallstack)
    }

    /// Whether call-frame observation is doing any work at all for the current window.
    ///
    /// `false` for an inactive inspector and for an inert selector set, and it is the single gate
    /// every frame-state operation checks first, so "no selector" and "not armed" are structurally
    /// equivalent to the callstack path being absent.
    fn callstack_capture_armed(&self) -> bool {
        self.active && !self.callstack.is_inert()
    }

    /// Whether the innermost open frame's executed bytecode hash is needed at all.
    ///
    /// Only a frame carrying a staged candidate needs one. A staged `Call` is the selected address
    /// running its own code — which is also the only frame that can be the parent of a direct
    /// delegatecall, so recording its hash is exactly what keeps that attribution available — and a
    /// staged `DirectDelegatecall` needs the implementation's executed hash. No other frame's hash
    /// is ever read, so it is never computed: hashing whole bytecode is the one genuinely costly
    /// step in the hot `initialize_interp` hook.
    fn needs_executed_code_hash(&self) -> bool {
        self.callstack_capture_armed()
            && self.frames.last().is_some_and(|frame| frame.pending.is_some())
    }

    /// Enters a frame: records its execution identity and stages an observation candidate if the
    /// selector predicate matches. Staging allocates no observation and consumes no budget.
    ///
    /// `scheme` is `None` for a `CREATE`/`CREATE2` frame.
    ///
    /// Both gates come BEFORE any frame-state work: while the inspector is inactive (a gasless
    /// pre-check or any non-candidate simulation) and while the selector set is inert (no active
    /// rule asks about call frames) not even a [`FrameRecord`] is pushed, so a deployment with no
    /// callstack rule pays one branch per frame and nothing else. [`Self::exit_frame`] applies the
    /// same gate, and the configuration cannot change inside a window, so the stack stays balanced.
    fn enter_frame(&mut self, target: Address, bytecode: Address, scheme: Option<CallScheme>) {
        if !self.callstack_capture_armed() {
            return;
        }
        let is_create = scheme.is_none();
        let pending = self.stage(target, bytecode, scheme);
        if self.frames.len() >= MAX_OPEN_FRAMES {
            // Parentage can no longer be tracked: fail closed instead of dropping the frame,
            // which would make an unobserved call indistinguishable from an absent one.
            self.frame_invariant.get_or_insert(CaptureInvariantError::FrameStackOverflow {
                depth: self.frames.len(),
            });
            return;
        }
        self.frames.push(FrameRecord { target, bytecode, is_create, code_hash: None, pending });
    }

    /// Decides which observation, if any, a frame being entered should stage.
    fn stage(
        &self,
        target: Address,
        bytecode: Address,
        scheme: Option<CallScheme>,
    ) -> Option<PendingObservation> {
        // A create frame runs init code; see `FrameRecord::is_create`.
        let scheme = scheme?;
        // A direct delegatecall is decided FIRST. A selected proxy DELEGATECALLing ITSELF enters a
        // frame whose target and bytecode are both the proxy, which the own-code test below would
        // otherwise claim as a plain call and hide from every direct-delegatecall rule. `CALLCODE`
        // stays excluded.
        if scheme == CallScheme::DelegateCall
            && let Some(parent) = self.frames.last()
            && !parent.is_create
            && parent.bytecode == parent.target
            && self.callstack.observes(&parent.target)
        {
            // The parent issued this DELEGATECALL, so it is executing and its executed code hash
            // is known. Without it there is nothing truthful to report, so nothing is.
            let code_hash = parent.code_hash?;
            return Some(PendingObservation::DirectDelegatecall {
                address: parent.target,
                code_hash,
                impl_address: bytecode,
            });
        }
        // The selected address running its own code.
        if bytecode == target && self.callstack.observes(&target) {
            return Some(PendingObservation::Call { address: target });
        }
        None
    }

    /// Confirms that the innermost frame really executes and commits its staged observation.
    ///
    /// Called once an interpreter has been created for the frame, which is the first moment the
    /// frame is known to execute and the first moment the bytecode it actually loaded is
    /// available. The hash is taken from that bytecode — not from an account-state lookup, which
    /// under EIP-7702 names the delegation designator rather than the delegate code that runs.
    fn confirm_frame_execution(&mut self, executed_code_hash: B256) {
        if !self.callstack_capture_armed() {
            return;
        }
        let Some(frame) = self.frames.last_mut() else {
            return;
        };
        frame.code_hash = Some(executed_code_hash);
        let Some(pending) = frame.pending.take() else {
            return;
        };
        let observation = match pending {
            PendingObservation::Call { address } => {
                CallObservation::call(address, executed_code_hash)
            }
            PendingObservation::DirectDelegatecall { address, code_hash, impl_address } => {
                CallObservation::direct_delegatecall(
                    address,
                    code_hash,
                    impl_address,
                    executed_code_hash,
                )
            }
        };
        self.commit_observation(observation);
    }

    /// The single funnel where a confirmed call-frame observation is counted and appended, so
    /// counting and overflow can never drift between the call and create paths.
    ///
    /// Mirrors the native-transfer budget: the count never rolls back, the `limit`-th observation
    /// is accepted, the `(limit + 1)`-th sets a sticky overflow and is not appended, and the EVM
    /// keeps executing normally — only new appends stop.
    fn commit_observation(&mut self, observation: CallObservation) {
        self.callstack_observed_count += 1;
        if self.callstack_observed_count > self.callstack_limit {
            self.callstack_overflowed = true;
            return;
        }
        self.observations.push(observation);
    }

    /// Leaves a frame, discarding any still-staged candidate.
    ///
    /// A candidate that is still staged at exit never reached an interpreter, so the call never
    /// executed and must not appear as an observation. Already-committed observations are
    /// untouched: this is deliberately NOT the log checkpoint/truncate path, because a log
    /// describes an effect while an observation describes execution identity.
    fn exit_frame(&mut self) {
        if !self.callstack_capture_armed() {
            return;
        }
        if self.frames.pop().is_none() {
            self.frame_invariant
                .get_or_insert(CaptureInvariantError::FrameStackUnbalanced { remaining: 0 });
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
        if !self.native_capture_armed() {
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

    /// Records a real EVM log into the unified ordered queue. No-op while inactive, and in
    /// [`CaptureMode::CallstackOnly`], where the consumer screens against the raw execution logs
    /// and no log channel is synthesized at all.
    pub fn record_real_log(&mut self, log: Log) {
        if !self.native_capture_armed() {
            return;
        }
        self.queue.push(QueuedLog::Real(log));
    }

    /// Pushes a checkpoint at the current queue position on frame entry. No-op while inactive,
    /// and in [`CaptureMode::CallstackOnly`], where there is no queue to roll back. Observations
    /// deliberately do not roll back on revert — code that executed and then reverted still
    /// executed — so a callstack-only window needs no checkpoint at all.
    pub fn push_checkpoint(&mut self) {
        if !self.native_capture_armed() {
            return;
        }
        self.checkpoints.push(self.queue.len());
    }

    /// Frame exit: on success keep the frame's events; on revert/halt/early-failure truncate the
    /// queue back to that frame's checkpoint. `observed_count` never rolls back. No-op while
    /// inactive.
    pub fn pop_checkpoint(&mut self, reverted: bool) {
        // Gated on the same predicate as `push_checkpoint` so the two stay balanced in every
        // mode. (`pop` is already a no-op on an empty stack, so this is symmetry, not a fix.)
        if !self.native_capture_armed() {
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

#[cfg(any(test, feature = "test-utils"))]
impl RcsInspector {
    /// Number of open frames the inspector is currently tracking. Test-only: an inert selector set
    /// must leave this at `0` for every frame of the transaction, which is direct evidence that no
    /// frame-state work happened rather than an inference from the observation list being empty.
    pub fn open_frame_count(&self) -> usize {
        self.frames.len()
    }
}

impl RcsInspector {
    /// Clears every piece of per-transaction capture state. Used by all three lifecycle
    /// transitions so no state can leak from one capture window into the next.
    fn reset_state(&mut self) {
        self.queue.clear();
        self.checkpoints.clear();
        self.create_recipients.clear();
        self.observed_count = 0;
        self.overflowed = false;
        self.address_mismatch_create_index = None;
        self.selfdestruct_journal_mark = None;
        self.selfdestruct_wrote_journal = false;
        self.frames.clear();
        self.observations.clear();
        self.callstack_observed_count = 0;
        self.callstack_overflowed = false;
        self.frame_invariant = None;
    }
}

impl RcsCaptureControl for RcsInspector {
    fn configure_callstack_capture(&mut self, config: CallstackCaptureConfig) {
        // Only ever called between windows. Swapping the selector set mid-window would let one
        // transaction's frames be entered under one configuration and exited under another, which
        // would unbalance the frame stack; the assertion states that contract where it is relied
        // on rather than leaving it to the caller.
        debug_assert!(
            !self.active,
            "the capture configuration must be installed while the inspector is inactive"
        );
        self.callstack = config;
    }

    fn start_capture(&mut self) {
        self.reset_state();
        self.active = true;
    }

    fn finish_capture(&mut self, result_logs: &[Log]) -> CaptureOutcome {
        // Test-only controlled-outcome seam (spec §5.10.2): when armed, inject a real-log divergence
        // so the REAL cross-check below yields a genuine `RealLogMismatch` on the live candidate-loop
        // path (used by the loop-level test 35 driving `execute_best_transactions`). No effect in
        // production builds — the whole block is feature-gated.
        #[cfg(any(test, feature = "test-utils"))]
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
        // Every observed frame must have been exited by the time the window closes. A leftover
        // entry means a frame entry was not matched by its exit, so parentage — and therefore the
        // direct-delegatecall discrimination — cannot be trusted for this transaction.
        if self.frame_invariant.is_none() && !self.frames.is_empty() {
            self.frame_invariant =
                Some(CaptureInvariantError::FrameStackUnbalanced { remaining: self.frames.len() });
        }
        let outcome = if let Some(create_index) = self.address_mismatch_create_index {
            // D7b: a successful CREATE returned an address that differed from the pre-derived
            // expected address. Fail closed — never silently truncate with Complete.
            CaptureOutcome::InvariantViolation {
                reason: CaptureInvariantError::CreateAddressMismatch { create_index },
            }
        } else if let Some(reason) = self.frame_invariant {
            CaptureOutcome::InvariantViolation { reason }
        } else if self.native_capture_armed_at_finish() && self.overflowed {
            CaptureOutcome::LimitExceeded {
                attempted: self.observed_count,
                limit: self.limit,
                budget: ObservationBudget::NativeTransfer,
            }
        } else if self.callstack_overflowed {
            // A truncated observation set cannot be adjudicated: a frame missing because of the
            // budget is indistinguishable from a frame that never happened.
            CaptureOutcome::LimitExceeded {
                attempted: self.callstack_observed_count,
                limit: self.callstack_limit,
                budget: ObservationBudget::Callstack,
            }
        } else if !self.native_capture_armed_at_finish() {
            // `CaptureMode::CallstackOnly`: nothing was queued, so there is no real-log
            // subsequence to cross-check against `result_logs` and no native budget to report.
            // Reporting `logs: []` here is NOT "capture saw no logs" — this mode produces no log
            // channel at all, and its consumer screens against its own raw execution logs.
            let stamp = self.callstack.stamp.unwrap_or(INERT_SNAPSHOT_STAMP);
            CaptureOutcome::Complete {
                logs: Vec::new(),
                observations: CapturedObservations::new(
                    stamp,
                    std::mem::take(&mut self.observations),
                ),
            }
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
                    let logs = std::mem::take(&mut self.queue)
                        .into_iter()
                        .map(QueuedLog::into_log)
                        .collect();
                    // `stamp` is `None` only for an inspector built without a snapshot (the
                    // native-transfer-only constructor). Such an inspector observes no frames, so
                    // there is no snapshot identity to assert and an inert stamp is reported.
                    let stamp = self.callstack.stamp.unwrap_or(INERT_SNAPSHOT_STAMP);
                    let observations =
                        CapturedObservations::new(stamp, std::mem::take(&mut self.observations));
                    CaptureOutcome::Complete { logs, observations }
                }
            }
        };
        // Uniform every-exit cleanup (R9 item 4): leave the inspector inactive and fully reset so
        // nothing — queue, checkpoints, either observation counter, a sticky overflow flag, the
        // sticky address-mismatch flag, the frame stack, or a frame invariant — can leak into the
        // next `start_capture`.
        self.reset_state();
        self.active = false;
        outcome
    }

    fn abort_capture(&mut self) {
        self.reset_state();
        self.active = false;
    }
}

/// The stamp reported by an inspector constructed without a rule snapshot.
///
/// Such an inspector observes no call frames, so its observation list is always empty and there is
/// no snapshot identity to assert. Screening only ever compares stamps for rules that declare
/// selectors, and no such rule can exist under a snapshot that produced no selector set, so this
/// value can never be mistaken for a real one.
const INERT_SNAPSHOT_STAMP: SnapshotStamp =
    SnapshotStamp { protocol_version: 0, content_version: 0, selector_digest: B256::ZERO };

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

    fn initialize_interp(&mut self, interp: &mut Interpreter<INTR>, _context: &mut CTX) {
        // The gate comes first and the hash second. Every frame of every simulation reaches this
        // hook, so hashing before deciding whether the hash is wanted would charge the whole node
        // — including deployments with no callstack rule and the inactive gasless pre-check — for
        // a value nothing reads.
        if !self.needs_executed_code_hash() {
            return;
        }
        // The first point at which this frame is known to execute: an interpreter exists for it.
        // The hash is of the bytecode the EVM actually loaded for the frame, so an EIP-7702
        // delegated call reports the delegate code that really ran rather than the designator
        // stored on the account.
        #[cfg(any(test, feature = "test-utils"))]
        CALLSTACK_HASH_COMPUTATIONS.with(|c| c.set(c.get() + 1));
        let executed_code_hash = keccak256(interp.bytecode.bytecode_slice());
        self.confirm_frame_execution(executed_code_hash);
    }

    fn call(&mut self, _context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        // Every CALL-type frame (incl. zero-value / DELEGATECALL / STATICCALL / CALLCODE) gets a
        // checkpoint so real logs it captured roll back if it reverts.
        self.push_checkpoint();
        // Frame entry stages an observation candidate; nothing is emitted until the frame is
        // confirmed to execute in `initialize_interp`.
        self.enter_frame(inputs.target_address, inputs.bytecode_address, Some(inputs.scheme));
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
        // Discards a candidate that never reached an interpreter; a frame that executed and then
        // reverted keeps its already-committed observation.
        self.exit_frame();
    }

    fn create(&mut self, context: &mut CTX, inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        // Record that the hook was invoked at all, before anything else — including
        // the active gate — so a reader can tell "never hit" (count stays 0) from "hit while
        // inactive" (count > 0, capture_body_ran == false).
        #[cfg(any(test, feature = "test-utils"))]
        CREATE_HOOK_HITS.with(|c| c.set(c.get() + 1));
        self.push_checkpoint();
        if !self.active {
            // Not armed: no capture body ran, and therefore no address derivation ran either.
            #[cfg(any(test, feature = "test-utils"))]
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
        // A create frame is tracked for frame balance and parentage only: it runs init code, so it
        // produces no observation and never acts as a direct-delegatecall parent (see
        // `FrameRecord::is_create`).
        self.enter_frame(created_address, created_address, None);
        // Test-only: when armed, push a WRONG expected address so `create_end` detects `e != a`
        // on a real successful CREATE and exercises the D7b fail-closed branch. One-shot: the flag
        // is consumed here so only the armed frame is affected. No effect in production builds.
        #[cfg(any(test, feature = "test-utils"))]
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
        #[cfg(any(test, feature = "test-utils"))]
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
        // Balances the `create`-hook entry and discards any still-staged candidate.
        self.exit_frame();
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
        #[cfg(any(test, feature = "test-utils"))]
        CREATE_END_BALANCE.with(|s| s.set((self.create_recipients.len(), self.checkpoints.len())));
        // Record the REAL completion the EVM handed us (plus the sticky observation count at this
        // instant, which must never have been decremented by the revocation above) so tests assert
        // completion validity directly rather than inferring it from the flattened stream.
        #[cfg(any(test, feature = "test-utils"))]
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
        // active_branch_ran == false). No effect in production builds (the seam is feature-gated).
        #[cfg(any(test, feature = "test-utils"))]
        SELFDESTRUCT_GATE_SNAPSHOT.with(|s| {
            let (hits, active_ran, journal_ran) = s.get();
            s.set((hits + 1, active_ran, journal_ran));
        });
        // Inactive windows are strict no-ops: no journal read, no field write (gasless pre-checks
        // and other non-capturing simulations must never pay for or observe this).
        if !self.active {
            return;
        }
        #[cfg(any(test, feature = "test-utils"))]
        SELFDESTRUCT_GATE_SNAPSHOT.with(|s| {
            let (hits, _, journal_ran) = s.get();
            s.set((hits, true, journal_ran));
        });
        #[cfg(any(test, feature = "test-utils"))]
        SELFDESTRUCT_GATE_SNAPSHOT.with(|s| {
            let (hits, active_ran, _) = s.get();
            s.set((hits, active_ran, true));
        });
        #[cfg(any(test, feature = "test-utils"))]
        record_selfdestruct_step(interp.bytecode.opcode(), interp.gas.remaining());
        self.selfdestruct_journal_mark = Some(context.journal_ref().journal().len());
    }

    fn step_end(&mut self, _interp: &mut Interpreter<INTR>, context: &mut CTX) {
        if !self.active {
            return;
        }
        if let Some(pre) = self.selfdestruct_journal_mark.take() {
            self.selfdestruct_wrote_journal = context.journal_ref().journal().len() > pre;
            #[cfg(any(test, feature = "test-utils"))]
            record_selfdestruct_step_end(
                context.journal_ref().depth(),
                _interp.bytecode.instruction_result(),
                _interp.gas.remaining(),
                context.journal_ref().journal().len().saturating_sub(pre),
            );
        }
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        // Record that the callback was entered at all, before anything else -- including the
        // active gate -- so direct evidence distinguishes "never entered" from "entered while
        // inactive". No effect in production builds (the seam is feature-gated).
        #[cfg(any(test, feature = "test-utils"))]
        SELFDESTRUCT_CALLBACK_HITS.with(|c| c.set(c.get() + 1));
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

#[path = "../tests/capture__tests.rs"]
#[cfg(test)]
mod tests;

/// Runtime test fixture for the builder capture path. Kept `pub(crate)` so test 33 (spec §8, in this
/// file) asserts a REAL `evm.transact`-driven `RealLogMismatch` outcome — one runtime source, not a
/// re-implementation. NOTE: the loop-level fail-closed test 35 in `context.rs` does NOT consume this
/// fixture; it drives the real candidate loop and forces the mismatch inside the loop's own
/// `RcsInspector::finish_capture` via the `FORCE_REALLOG_MISMATCH` seam.
#[path = "../tests/capture__test_fixtures.rs"]
#[cfg(test)]
pub(crate) mod test_fixtures;

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
#[path = "../tests/capture__gating_integration_tests.rs"]
#[cfg(test)]
mod gating_integration_tests;

#[path = "../tests/capture__callstack_observation_tests.rs"]
#[cfg(test)]
mod callstack_observation_tests;
