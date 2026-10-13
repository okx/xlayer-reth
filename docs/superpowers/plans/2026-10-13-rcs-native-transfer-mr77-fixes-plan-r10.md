# MR77 RCS Filter — r10 Rework Implementation Plan (Nested CREATE Endowment + Top-level native Transfer Unification)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-submit MR77 at a new head SHA that satisfies the design-r10 acceptance on top of the current implementation at head `21a311e9`. The r10 design (spec §8.r10) carries forward all 19 acceptance IDs from r9 with three assertion-wording strengthenings and five inherited-completeness writebacks. Corrections A (`test-utils` Cargo feature) and B (complete Lark publication) from r9 carry forward. No new design decisions — D1–D9/D7a/D-ISO/D7b mechanism UNCHANGED. Plan baseline = `21a311e9` (unchanged from r8/r9).

**Architecture:** Two production files change (spec §5 as rewritten in r6/r7): `crates/builder/src/flashblocks/rcs_capture.rs` (value-only CALL/CREATE decisions; `create_end` three-way branch; `finish_capture` CreateAddressMismatch outcome; `derivation_ran` computed sentinel; the `CaptureInvariantError::CreateAddressMismatch` variant) and `crates/builder/src/flashblocks/context.rs` (D7b `fail_closed_screen` arm). PLUS one PROD config change: `crates/rcs-filter/Cargo.toml` (`test-utils` feature) and `crates/builder/Cargo.toml` (`[dev-dependencies]` enabling it). The approved D1/D2/D4/D5/D7/D7a/D-ISO mechanism is already present at `21a311e9` and is NOT re-litigated. `crates/rcs-filter/src/{handle.rs,matching.rs}` take **no** production change (D3/T3f). All real-capture→`screen_tx` end-to-end tests live in `crates/builder`'s `#[cfg(test)]`, not in `rcs-filter` (which has no `OpEvm`/builder dependency).

**Tech Stack:** Rust, revm (pinned: revm-handler 20.0.3 / revm-inspector 21.0.3 / revm-interpreter 37.0.3), op-revm (JOVIAN⇒PRAGUE⇒EIP-7702 active), `alloy_op_evm::OpEvm` composite inspector, `cargo`/`cargo nextest`, `just`. Repo `github/xlayer-reth`, branch `xl/xlop-1217/0928-1135`, reused MR https://gitlab.okg.com/github/xlayer-reth/-/merge_requests/77 (current head `21a311e9`). Jira XLOP-1228 (Task) under Epic XLOP-1217. Packages: `xlayer-builder`, `rcs-filter`.

**Spec:** Lark design doc https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac (revision **r10**, canonical sha256 `87cb906d8da9422db3c5d75ecf4464cf84b5e20b62e5725419596c0e5d203c68`). Design PASS `starport-rcs-design-review-r10-pass-20261013` (Bus **recvxUNFLdFDzO**), approval_mode=delegated_agent. This is a **design** PASS only: the MR implementation remains CHANGES_REQUESTED at `21a311e9` and must be re-submitted at a new head SHA (Stages 3.0/5.0); grants no merge/deploy/release/MR-approve/scope expansion. The plan argues from this spec; executors read both.

## Global Constraints

Every task's requirements implicitly include this section (verbatim from spec §5/§7/§8.E/§8.r10).

- **Scope:** only the r10 corrections/strengthenings + their tests + this plan/spec. No change to the approved D1–D9 + D7a + D-ISO + D7b mechanism. No merge/deploy/release/force-push. No extra repo scope. No new Jira. Reuse MR77 and branch `xl/xlop-1217/0928-1135`; preserve others' edits and the `deps/optimism` submodule pointer (never touch the submodule).
- **Production change sites — THREE files (spec §5, §8.r10):** (a) `crates/builder/src/flashblocks/rcs_capture.rs` (D7b: `create_end` three-way + `finish_capture` mismatch outcome + `CaptureInvariantError::CreateAddressMismatch`; r10-T1-DERIVE: `derivation_ran` computed sentinel); (b) `crates/builder/src/flashblocks/context.rs` (D7b: new `fail_closed_screen` arm); (c) `crates/rcs-filter/Cargo.toml` + `crates/builder/Cargo.toml` (Correction A: `test-utils` feature). `crates/rcs-filter/src/{handle.rs,matching.rs}` get **no** production change. `ScreenInput.value` stays (D3).
- **Test home:** every real-capture→`screen_tx` test lives in `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]` — builder is the only crate that can run `alloy_op_evm::OpEvm<_, RcsInspector, _>` AND call `rcs_filter::FilterHandle::screen_tx`. `rcs-filter`'s `integration_tests.rs` tests are KEEP-only (supplementary generic matcher coverage).
- **FAIL rule (spec §8.r10, binding):** acceptance is a conjunction over classes — every **TEST** present+passing in the per-SHA log, every **PROD** change in the diff, every **SPEC** statement made, every **KEEP** test retained. A test that is absent / renamed-away / `#[ignore]`d / weakened, or any missing PROD/SPEC/KEEP/§8.E item, is a **FAIL** for the whole re-submission — a subset is a FAIL. Stage 3.0 must publish an **ID→artifact→evidence** table (ID → test fn name / code site / assertion → nextest log line or diff hunk). The 19 r10 IDs are: r10-ADVANCE-CFG, r10-ADVANCE-TEST, r10-T3-LEGACYVAL, r10-T3-DUALALIAS, r10-T3-AUDITMERGE, r10-T3-NT2-EVID, r10-DMM-LOOP, r10-DMM-ABORT, r10-T1-DERIVE, r10-OVF-ASSERT, r10-T2-ORIGIN, r10-REGRESS-KEEP, r10-EV-LOG, r10-EV-TABLE, r10-EV-CI, r10-EV-T35, r10-EV-SUPERPOWERS.
- **Evidence (§8.E), workspace-wide at the NEW head SHA (not `21a311e9`):** readable raw `cargo nextest` log over `xlayer-builder` AND `rcs-filter` with the exact command, process exit code, and named per-test-ID results; `cargo +nightly fmt --all --check` (exit 0) and `cargo clippy --all-targets --workspace -- -D warnings` (0/0); MR-table test names must all exist in the tree at that SHA; test-35 M1/M2/M3 marked retained (blob proven unchanged) vs re-tested. Pipeline 14825765 clippy job 39249795 recorded as CI evidence (NOT substitute for product-test log).
- **Budgets / sticky counter:** `max_native_transfers_per_tx` and `max_event_bindings_per_tx` both default 10000, both `>= 1`; `limit`-th allowed, `(limit+1)`-th fails closed; `observed_count` never rolls back — including for a revoked (D7a) or mismatched (D7b) CREATE observation.
- **Isolation / inactive no-op (AC1, D-ISO) & D9:** inactive/gasless/sequencer/cached/self-generated paths do zero capture-private mutation and zero journal/address derivation; virtual logs never reach a receipt, RPC log, bloom, the indexer, or consensus/commit state.
- **Formatting/lint for editing:** `cargo +nightly fmt -p <pkg>` scoped per package while editing; the `--all --check` run is the recorded §8.E evidence; never auto-format the submodule.

## Review Focus

The five round-3 failure modes; each pinned by a task below.

- **Fail-open on successful-CREATE address mismatch** — `create_end` folds `e != a` into `reverted` → silent `pop_checkpoint(true)`, so `finish_capture` can return `Complete(empty)` and a real endowment escapes the screen. Pinned in **Task 1** (D7b; r10-D-MM-LOOP, r10-DMM-ABORT).
- **T3 assertions not specific enough** — DUALALIAS allows rule-itself Deny; AUDITMERGE lacks dedup/negative-control/audit_type-as-key; LEGACYVAL lacks negative control. Pinned in **Task 2** (r10-T3-DUALALIAS two-stream + r10-T3-AUDITMERGE dedup).
- **T2-ORIGIN too vague** — "NOT zero/caller/call_recipient" not exact address. Pinned in **Task 3** (r10-T2-ORIGIN exact CreateOutcome.address).
- **Inherited completeness gaps** — derivation_ran hardcoded false; overflow no is_ok/address assert; ABORT no mismatch-flag clear. Pinned in **Task 4** (r10-T1-DERIVE, r10-OVF-ASSERT, r10-ADVANCE-CFG).
- **Unverifiable evidence** — a pipeline with no product-test job and an MR table naming deleted tests. Pinned in **Task 5** (§8.E + ID→artifact→evidence).

---

### Task 0 — Cross-crate test isolation (PROD, Correction A; spec §8.r10 r10-ADVANCE-CFG, r10-ADVANCE-TEST)

**Files:**
- Modify: `crates/rcs-filter/Cargo.toml` — add `[features] test-utils = []`
- Modify: `crates/rcs-filter/src/handle.rs` — gate `advance_to_approved_for_test` behind `#[cfg(any(test, feature = "test-utils"))]`
- Modify: `crates/builder/Cargo.toml` — add `rcs-filter = { path = "../rcs-filter", features = ["test-utils"] }` in `[dev-dependencies]`

**Interfaces:**
- Produces: `advance_to_approved_for_test` available in builder tests but NOT in production `cargo build -p rcs-filter`
- Consumes: existing RESIM test at `crates/builder/src/flashblocks/rcs_capture.rs`

- [ ] **Step 1 — dual-verification (r10-ADVANCE-CFG):** Run `cargo build -p rcs-filter`. Expected: symbol NOT exposed (compile success). Run `cargo test -p xlayer-builder`. Expected: compile + RESIM test passes.
- [ ] **Step 2 — RESIM compilation test (r10-ADVANCE-TEST):** Verify the existing RESIM test at the builder's `rcs_capture.rs` test module compiles and calls `FilterHandle::advance_to_approved_for_test` successfully.
- [ ] **Step 3: commit** — `git add crates/rcs-filter/Cargo.toml crates/rcs-filter/src/handle.rs crates/builder/Cargo.toml && git commit -m "fix(rcs): test-utils feature for cross-crate advance_to_approved_for_test isolation (r10-ADVANCE-CFG)"`

---

### Task 1 — D7b: fail-closed successful-CREATE address mismatch + loop-level Deny proof + mismatch-flag clear on BOTH paths (PROD+TEST, spec §4 D7b / §5 / §8.r10 r10-DMM-LOOP, r10-DMM-ABORT)

**Files:**
- Modify: `crates/builder/src/flashblocks/rcs_capture.rs` — `create_end` three-way branch, `finish_capture` mismatch outcome, `CaptureInvariantError::CreateAddressMismatch` variant, every-exit state clear (clear the mismatch flag in BOTH `finish_capture` AND `abort_capture`).
- Modify: `crates/builder/src/flashblocks/context.rs` — `fail_closed_screen` new arm (compiler-mandatory).
- Test: `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]` + loop-level test via `LAST_SCREEN_DECISION`.

**Interfaces:**
- Produces: `CaptureInvariantError::CreateAddressMismatch { create_index: usize }` (index/count only, never addresses/log content), surfaced by `finish_capture` as `CaptureOutcome::InvariantViolation { reason: CreateAddressMismatch { .. } }`. Sticky inspector flag set in `create_end`, cleared on every `finish_capture`/`abort_capture` exit. `context.rs::fail_closed_screen` maps that outcome → `record_local_deny(LocalDenyReason::ObservationInvariant)` + `Some(Screen::Deny)`.
- Consumes: the existing `pop_checkpoint`, `LAST_SCREEN_DECISION` test-35 seam, `record_local_deny`.

- [ ] **Step 1 (r10-D-MM-GUARD, TEST — RED): write the failing fail-closed guard test** — `create_address_mismatch_through_real_create_end_fails_closed`: via a `#[cfg(test)]` seam force `e != a` on a **real successful** `create_end`; assert `finish_capture ⇒ InvariantViolation{CreateAddressMismatch}` (NEVER `Complete`). Expected: RED on `21a311e9` (the mismatch is folded into `reverted` → silent truncation → `Complete`).
- [ ] **Step 2 (PROD): implement the three-way `create_end` + `finish_capture` + enum variant** — in `create_end` distinguish: (a) `!is_ok()` → truncate (D7); (b) `is_ok && address.is_none()` → revoke/truncate (D7a); (c) `is_ok && Some(a) present && e != a` → set sticky `CreateAddressMismatch { create_index }` flag, do NOT `pop_checkpoint(true)`. In `finish_capture`, when flag is set return `InvariantViolation{CreateAddressMismatch}` alongside `LimitExceeded`/`RealLogMismatch`. Holds in debug AND release (regular match/flag, never `debug_assert!`-only). `observed_count` stays sticky.
- [ ] **Step 3 (PROD): add the `context.rs` arm** — in `fail_closed_screen` add `CaptureOutcome::InvariantViolation { reason: CaptureInvariantError::CreateAddressMismatch { .. } }` → bounded `error!` + `record_local_deny(ObservationInvariant)` + `Some(Screen::Deny)`. Compiler-mandatory via exhaustive match at `:146-149`.
- [ ] **Step 4 (r10-DMM-LOOP, TEST — GREEN loop-level):** extend the D-MM guard test to drive the builder candidate-loop path after `InvariantViolation`; assert via `LAST_SCREEN_DECISION` that `Some(Screen::Deny)` was written, `screen_tx` was NOT called, no `BufferEntry`/pending, no commit. Falsifiable: deleting the `CreateAddressMismatch` arm from `fail_closed_screen` must fail compile or the test.
- [ ] **Step 5 (r10-DMM-ABORT, TEST):** `address_mismatch_flag_cleared_by_both_finish_and_abort` — (a) set mismatch flag to `Some` → `finish_capture` clears it (assert `is_none()` after finish); (b) set to `Some` → `abort_capture` clears it (assert `is_none()` after abort). Both codepaths verified.
- [ ] **Step 6: run GREEN** — Run `cargo nextest run -p xlayer-builder create_address_mismatch_through_real_create_end_fails_closed address_mismatch_flag_cleared_by_both_finish_and_abort`. Expected: PASS.
- [ ] **Step 7: commit** — `git add crates/builder/src/flashblocks/rcs_capture.rs crates/builder/src/flashblocks/context.rs && git commit -m "fix(rcs): fail-closed on successful-CREATE address mismatch with loop-level Deny proof (r10-DMM)"`

---

### Task 2 — Real builder-OpEvm → `screen_tx` end-to-end with r10-strengthened T3 assertions (TEST+SPEC, spec §8.r10 r10-T3-LEGACYVAL, r10-T3-DUALALIAS, r10-T3-AUDITMERGE, r10-T3-NT2-EVID)

**Files:**
- Test: `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]` (real `OpEvm` → `finish_capture` → `Complete(stream)` → `rcs_filter::FilterHandle::screen_tx`). Shared harness constructs a `FilterHandle` with given `RuleSet` and screens the real captured stream.
- SPEC: state in plan/MR that `matching.rs`/`handle.rs` need **no** production change (T3f).
- KEEP: `crates/rcs-filter/src/integration_tests.rs` t3a–t3f retained as supplementary generic matcher coverage, not relabelled.

**Interfaces:**
- Consumes: Task 0 (`advance_to_approved_for_test` for RESIM), Task 1 D7b outcomes; the existing `run_capturing_tx`-style harness; `FilterHandle::screen_tx` (`handle.rs:273`, `(&self, input: &ScreenInput) -> Screen`); `quota_hash::encode_and_hash`.

- [ ] **Step 1 — original NT2, honest RED anchor (r10-T3-NT2-EVID, SPEC):** `original_nt2_internal_create_denied_through_real_screen_tx` — real `OpEvm` tx with an internal CREATE carrying non-zero endowment → real `Complete(stream)` → `FilterHandle` with ONE `transfer.*` deny rule → `assert_eq!(screen_tx, Screen::Deny)`. Show RED→GREEN only if the original internal-CREATE NT2 reproduces on the specified pre-D1 baseline; **if it cannot**, record the exact fixture/deployment/baseline difference and the actual result (r4 §13 AC3.6) — never write "RED pre-D1" as settled fact, never substitute a top-level CREATE.
- [ ] **Step 2 — top-level + one-rule-both:** `top_level_native_denied_through_real_screen_tx` (top-level CALL with `tx.value` → one `transfer.*` rule → `Deny`); `one_transfer_rule_denies_both_toplevel_and_internal_real_stream` (one real tx producing BOTH a top-level transfer and an internal-CREATE endowment; one rule → `Deny`; stream contains both distinct, no dedup D8).
- [ ] **Step 3 — budgets split:** `real_stream_native_budget_limit_plus_one_denies` (`max_native_transfers_per_tx = N`, `N+1` observations → `LimitExceeded` → `fail_closed_screen` → `Deny` WITHOUT `screen_tx`); a matcher-binding budget test on a real `Complete(stream)` fed to `screen_tx` where `max_event_bindings_per_tx` is exceeded at `limit+1` → `Deny` (distinct budget).
- [ ] **Step 4 — RESIM via public `screen_tx`:** `real_stream_screen_tx_re_sim_consistent` — drive the PUBLIC `screen_tx` twice on the real stream: first → `AuditPending` (`NotSubmitted`); transition pool to `Approved` via `advance_to_approved_for_test`; second → `AuditApproved` because `quota_hash::encode_and_hash(&actions)` matches stored hash. Negative control: changed-log re-sim → different `quota_hash` → `Screen::Drop`. Do NOT assert `screen_tx == screen_existing` (private) and do NOT edit the state machine.
- [ ] **Step 5 (r10-T3-LEGACYVAL):** `legacy_value_rule_still_fires_on_real_capture_superset_stream` — legacy tx-level `value` condition rule STILL fires under the superset `transfer.*` stream. Negative control: `ScreenInput.value = 0` with same stream → `Allow`, proving the rule fires via the tx-level `value` binding not only `transfer.value`.
- [ ] **Step 6 (r10-T3-DUALALIAS):** `dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream` — TWO-STREAM comparison with DISTINGUISHABLE budget-boundary controls:
  - Stream-A: 1 native transfer, within `max_event_bindings_per_tx=3` → `Allow` or `Audit` (budget-not-exceeded side).
  - Stream-B: 2 native transfers, `(N+1)² = (2+1)² = 9 ≥ 3` → `Deny` (budget-exceeded side).
  - OR assert the actual `checked_binding_count` and `complete_binding_count` from the budget error path.
  - A bare rule-itself Deny cannot satisfy the assertion — the two-stream comparison with distinguishable boundaries is REQUIRED.
- [ ] **Step 7 (r10-T3-AUDITMERGE):** `audit_type_action_merge_action_content_no_empty_audit_on_real_stream` — ALL THREE of:
  - (a) Same `audit_type` → ONE action per `(audit_type, log_index)`, with bound `transfer.from`, `transfer.to`, `transfer.value` in the action's content.
  - (b) Different `audit_type` → each retained as SEPARATE actions.
  - (c) NEGATIVE CONTROL: all-null or no physical event → NO empty `Audit` action produced. `audit_type` is the KEY of the `actions` mapping (`matching.rs:163-171`), NOT a field of `ActionItem`.
- [ ] **Step 8 (T3f, SPEC) + KEEP:** record "no `matching.rs`/`handle.rs` production change required"; retain rcs-filter t3a–t3f unchanged, not relabelled.
- [ ] **Step 9: run** — Run `cargo nextest run -p xlayer-builder` for all Step 1–7 test functions. Run `cargo test -p rcs-filter` to confirm KEEP tests pass. Expected: ALL PASS.
- [ ] **Step 10: commit** — `git add crates/builder/src/flashblocks/rcs_capture.rs && git commit -m "test(rcs): real OpEvm->screen_tx end-to-end with r10-strengthened T3 assertions"`

---

### Task 3 — Restore deleted regressions, rename collision test, add TRUE nonce-overflow, exact T2-ORIGIN (TEST+PROD, spec §8.r10 r10-REGRESS-KEEP, r10-OVF-ASSERT, r10-T2-ORIGIN)

**Files:**
- Test: `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]`.
- Rename: `internal_create2_nonce_overflow_endowment_revoked` (EIP-684 collision) → `internal_create2_eip684_collision_endowment_revoked` (PROD/rename).

**Interfaces:** Consumes Task 1's D7b outcomes and the sticky `observed_count`.

- [ ] **Step 1 — restore the six real regressions by exact name (r10-REGRESS-KEEP, KEEP):** `top_level_call_with_value_emits_transfer`, `root_success_child_revert_keeps_root_drops_child`, `whole_tx_root_failure_drops_all`, `create_early_failure_no_address_or_event_leak`, `next_capture_starts_clean`, `create_is_ok_early_return_no_endowment_leak` (CREATE caller nonce `u64::MAX` → `is_ok=true`/`address=None`, phantom revoked, surviving root V, sticky counter).
- [ ] **Step 2 — add TRUE nonce-overflow tests (r10-OVF-ASSERT, TEST):** `create_nonce_overflow_is_ok_true_address_none_revoked` and `create2_nonce_overflow_is_ok_true_address_none_revoked` — caller nonce `u64::MAX`; assert the real outcome is `is_ok()==true` AND `address.is_none()` AND the sticky `observed_count` equals the pre-overflow count (proving the revoked observation does not decrement). Distinct from the EIP-684 collision case (`nonce:1`, `is_ok=false`).
- [ ] **Step 3 — rename the mislabelled collision test:** apply the rename above. Existing collision-budget tests may stay.
- [ ] **Step 4 (r10-T2-ORIGIN, TEST):** `eip7702_origin_itself_delegated_runs_inner_call_and_create` — origin itself carries EIP-7702 delegation designation; inner CALL and inner CREATE. Per-transfer assertions: (a) CALL log: `to == call_recipient`, `value == 1 wei`; (b) CREATE log: `to == EXACT CreateOutcome.address` (or verified nonce-derived address). Any wrong fourth address MUST FAIL the test. Also assert `result.result.is_success()`. No "NOT zero/caller/call_recipient" relaxation — exact address.
- [ ] **Step 5: run** — Run `cargo nextest run -p xlayer-builder` for all restored/new tests. Expected: ALL PASS.
- [ ] **Step 6: commit** — `git add crates/builder/src/flashblocks/rcs_capture.rs && git commit -m "test(rcs): restore regressions + true overflow + exact T2-ORIGIN address assertions (r10)"`

---

### Task 4 — T1 derivation_ran computed sentinel + proof-boundary hardening (TEST+PROD, spec §8.r10 r10-T1-DERIVE)

**Files:**
- Modify: `crates/builder/src/flashblocks/rcs_capture.rs` — `create` hook: change `derivation_ran` from hardcoded `false` to computed value (true when journal was read for address derivation).
- Test: `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]`.

**Interfaces:**
- Produces: `derivation_ran: bool` — `true` = journal was read for address derivation; `false` = a **not-hit sentinel** (the hook was NOT invoked, or was invoked but didn't need derivation).
- Separate "hook hit" counter so `false` cannot mean both "not run" and "ran but didn't derive."

- [ ] **Step 1 (PROD):** In the `create` hook, set `derivation_ran = true` when the journal read for address derivation actually executes (for CREATE scheme with non-zero endowment and active). Add a separate "hook hit" counter/bool for the `create_end` assertion (so `false` is unambiguously "not run").
- [ ] **Step 2 (r10-T1-DERIVE, TEST):** `derivation_ran_true_when_journal_read_for_address_derivation` — active CREATE with non-zero endowment → assert `derivation_ran == true` after the create hook return. `derivation_ran_false_when_create_hook_not_hit` — inactive path → `derivation_ran == false` sentinel. The two must be distinguishable.
- [ ] **Step 3 — T1 proof-boundary hardening:** `inactive_create_no_mutation_and_no_derivation_at_create_return` (assert at `create` return BEFORE `create_end` that `active==false`, `create_recipients`/checkpoints untouched, derivation did not run); `create_recipients_balanced_before_finish_capture` (assert empty inside active window BEFORE `finish_capture`); `abort_capture_mid_window_leaves_state_clean`; `two_consecutive_txs_capture_independently`.
- [ ] **Step 4: run** — Run `cargo nextest run -p xlayer-builder derivation_ran_true derivation_ran_false inactive_create_no_mutation create_recipients_balanced abort_capture_mid_window two_consecutive_txs`. Expected: ALL PASS.
- [ ] **Step 5: commit** — `git add crates/builder/src/flashblocks/rcs_capture.rs && git commit -m "fix(rcs): derivation_ran computed sentinel + T1 proof-boundary hardening (r10-T1-DERIVE)"`

---

### Task 5 — §8.E evidence bundle + ID→artifact→evidence table (SPEC/KEEP, spec §8.r10 r10-EV-LOG, r10-EV-TABLE, r10-EV-CI, r10-EV-T35, r10-EV-SUPERPOWERS)

**Files:**
- SPEC: output to `output.md` + MR description (Stage 3.0/5.0 produce; Stage 2.0 specifies).
- KEEP: `docs/superpowers/`, `.superpowers/`, checkpoint tarballs, Terran review ledgers.

**Interfaces:** Consumes: all prior Tasks' test names and file paths for the ID→test-name map.

- [ ] **Step 1 (r10-EV-LOG, SPEC):** Stage 3.0 MUST produce for the new head SHA: `cargo nextest run -p xlayer-builder -p rcs-filter` raw log with the exact command, process exit code, and NAMED per-test-ID results for every r10-* test + retained items. A bare count ("150 passed") with no raw log is NOT evidence.
- [ ] **Step 2 (r10-EV-TABLE, SPEC):** The MR description's test table MUST list ONLY tests that exist in the source at the new SHA by exact name. Design label: **r10**.
- [ ] **Step 3 (r10-EV-CI, SPEC):** Pipeline 14825765 clippy job 39249795 recorded as CI format/clippy evidence (NOT a substitute for the product-test log). `cargo +nightly fmt --all --check` exit 0 AND `cargo clippy --all-targets --workspace -- -D warnings` 0/0 at the new SHA, each with command + exit code.
- [ ] **Step 4 (r10-EV-T35, SPEC/KEEP):** test-35 M1/M2/M3 explicitly marked retained (covering blob proven byte-identical at the new SHA) vs re-tested (re-run result recorded).
- [ ] **Step 5 (r10-EV-SUPERPOWERS, KEEP):** preserve `docs/superpowers/`, `.superpowers/`, checkpoint tarballs, Terran review ledgers — do NOT delete or truncate.
- [ ] **Step 6 — publish ID→artifact→evidence table:** 19-row table mapping each r10-* ID to its test function name / code site / assertion class (TEST/PROD/SPEC/KEEP) / evidence location.
- [ ] **Step 7: commit** — `git add docs/superpowers/ && git commit -m "spec(rcs): §8.E evidence contract + ID→artifact→evidence table (r10-EV)"`

---

## r10 Acceptance ID → Test Name Mapping (for Stage 3.0 implementation)

| ID | Class | Name/Location | Assertion |
|---|---|---|---|
| r10-ADVANCE-CFG | PROD | `rcs-filter/Cargo.toml` `[features]`, `handle.rs` `#[cfg(any(test, feature="test-utils"))]`, `builder/Cargo.toml` `[dev-dependencies]` | `cargo build -p rcs-filter` NOT expose symbol; `cargo test -p xlayer-builder` compile+pass |
| r10-ADVANCE-TEST | TEST | `RESIM test at builder rcs_capture.rs` | Compiles; calls `advance_to_approved_for_test` |
| r10-T3-LEGACYVAL | TEST | `legacy_value_rule_still_fires_on_real_capture_superset_stream` | Negative control: `value=0` same stream → Allow |
| r10-T3-DUALALIAS | TEST | `dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream` | Two-stream: budget-within → Allow/Audit; budget-exceeded → Deny |
| r10-T3-AUDITMERGE | TEST | `audit_type_action_merge_action_content_no_empty_audit_on_real_stream` | Same-type→dedup; diff-type→retain; all-null→no empty Audit |
| r10-T3-NT2-EVID | SPEC | Plan/MR §8.r10 | RED on pre-D1 or non-repro record |
| r10-DMM-LOOP | TEST | `create_address_mismatch_through_real_create_end_fails_closed` (extended) | LAST_SCREEN_DECISION=Deny, screen_tx NOT called, no pending/commit |
| r10-DMM-ABORT | TEST | `address_mismatch_flag_cleared_by_both_finish_and_abort` | Set flag→finish clears; set flag→abort clears |
| r10-T1-DERIVE | PROD+TEST | `derivation_ran_true_when_journal_read_for_address_derivation` + `derivation_ran_false_when_create_hook_not_hit` | true=journal read; false=not-hit sentinel |
| r10-OVF-ASSERT | TEST | `create_nonce_overflow_is_ok_true_address_none_revoked` + `create2_nonce_overflow_is_ok_true_address_none_revoked` | is_ok()==true AND address.is_none() AND sticky observed_count |
| r10-T2-ORIGIN | TEST | `eip7702_origin_itself_delegated_runs_inner_call_and_create` | CREATE to==exact CreateOutcome.address; any wrong→FAIL |
| r10-REGRESS-KEEP | KEEP | 6 regressions: `top_level_call_with_value_emits_transfer`, `root_success_child_revert_keeps_root_drops_child`, `whole_tx_root_failure_drops_all`, `create_early_failure_no_address_or_event_leak`, `next_capture_starts_clean`, `create_is_ok_early_return_no_endowment_leak` | Present+passing |
| r10-EV-LOG | SPEC | `cargo nextest run -p xlayer-builder -p rcs-filter` raw log | Command+exit+Named per-ID results |
| r10-EV-TABLE | SPEC | MR test table | All names exist at new SHA; label r10 |
| r10-EV-CI | SPEC | Pipeline 14825765 clippy 39249795; `fmt --check` 0 + `clippy -D warnings` 0/0 | Command+exit code recorded |
| r10-EV-T35 | SPEC/KEEP | test-35 M1/M2/M3 | Retained (blob unchanged) vs re-tested |
| r10-EV-SUPERPOWERS | KEEP | `docs/superpowers/` + `.superpowers/` | Preserved intact |