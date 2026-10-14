# MR77 RCS Filter — r11 Implementation Plan (Nested CREATE Endowment + Top-level native Transfer Unification)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-submit MR77 at a new head SHA that satisfies the design-r11 acceptance on top of the current implementation at head `a0e5d7b7`. The r11 design preserves all r10 acceptance IDs unchanged and adds five narrow acceptance-strengthening IDs from the Stage-5.0 round-4 CHANGES_REQUESTED. D1–D9/D7a/D-ISO/D7b mechanism UNCHANGED from r10. Corrections A (`test-utils` feature) and B (complete Lark publication) carry forward from r9–r10. Plan baseline = **a0e5d7b7**.

**Architecture:** Two production files change (spec §5): `crates/builder/src/flashblocks/rcs_capture.rs` (value-only CALL/CREATE decisions; `create_end` three-way branch; `finish_capture` CreateAddressMismatch outcome; `derivation_ran` computed sentinel with separate hook-hit counter; the `CaptureInvariantError::CreateAddressMismatch` variant) and `crates/builder/src/flashblocks/context.rs` (D7b `fail_closed_screen` arm). PLUS one PROD config: `crates/rcs-filter/Cargo.toml` (`test-utils` feature) and `crates/builder/Cargo.toml` (`[dev-dependencies]`). `crates/rcs-filter/src/{handle.rs,matching.rs}` take **no** production change (D3/T3f). All real-capture→`screen_tx` end-to-end tests live in `crates/builder`'s `#[cfg(test)]` — builder is the only crate that can run `alloy_op_evm::OpEvm` AND call `rcs_filter::FilterHandle::screen_tx`.

**Tech Stack:** Rust, revm (pinned: revm-handler 20.0.3 / revm-inspector 21.0.3 / revm-interpreter 37.0.3), op-revm (JOVIAN⇒PRAGUE⇒EIP-7702 active), `alloy_op_evm::OpEvm` composite inspector, `cargo`/`cargo nextest`, `just`. Repo `github/xlayer-reth`, branch `xl/xlop-1217/0928-1135`, reused MR https://gitlab.okg.com/github/xlayer-reth/-/merge_requests/77 (current head `a0e5d7b7`). Jira XLOP-1228 (Task) under Epic XLOP-1217. Packages: `xlayer-builder`, `rcs-filter`.

**Spec:** Lark design doc https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac (revision **r11**, canonical sha256 `49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836`). Design PASS `starport-rcs-design-review-r11-pass-20261013` (Bus **recvxVksp5gum4**), approval_mode=delegated_agent. This is a **design** PASS only: the MR implementation remains CHANGES_REQUESTED at `a0e5d7b7` and must be re-submitted at a new head SHA (Stages 3.0/5.0); grants no merge/deploy/release/MR-approve/scope expansion.

## Global Constraints

Every task's requirements implicitly include this section.

- **Scope:** only the r10+r11 corrections/strengthenings + their tests + this plan/spec. No change to D1–D9 + D7a + D-ISO + D7b mechanism. No merge/deploy/release/force-push. No extra repo scope. No new Jira. Reuse MR77 and branch `xl/xlop-1217/0928-1135`; preserve others' edits and the `deps/optimism` submodule pointer (never touch).
- **Production change sites — THREE files (spec §5):** `crates/builder/src/flashblocks/rcs_capture.rs` (D7b + r10-T1-DERIVE), `crates/builder/src/flashblocks/context.rs` (D7b `fail_closed_screen` arm), `crates/rcs-filter/Cargo.toml` + `crates/builder/Cargo.toml` (test-utils feature). `crates/rcs-filter/src/{handle.rs,matching.rs}` get **no** production change.
- **Test home:** all real-capture→`screen_tx` tests in `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]`. `rcs-filter` tests are KEEP-only.
- **FAIL rule (binding):** every TEST present+passing, every PROD in diff, every SPEC stated, every KEEP retained. Missing any one = FAIL.
- **Evidence (§8.E + r11-EVIDENCE-CONTRACT):** (a) full raw `cargo nextest` output for BOTH `xlayer-builder` AND `rcs-filter` workspace-wide at submission SHA, with exact command, exit code, per-test-ID pass/fail/duration; (b) `cargo tree -e features -p xlayer-node` at submission SHA showing `rcs-filter/test-utils` NOT enabled in default production dep graph; (c) `cargo test -p xlayer-builder` output proving cross-crate tests available; (d) `cargo +nightly fmt --all --check` 0 + `cargo clippy --all-targets --workspace -- -D warnings` 0/0.
- **r11-NT2 (SPEC):** NT2 evidence must state EITHER genuine reproduction (exact pre-fix baseline/SHA, fixture, actual vs post-fix output) OR honest non-reproduction (scenario cannot be reproduced, exact baseline/fixture attempted, actual result). No fabricated RED baseline.
- **r11-MR-TABLE (SPEC):** MR description must list only tests existing at submission SHA by exact names; drop names confirmed non-existent; label r11; link full Lark URL + canonical sha256.
- **Budgets:** `max_native_transfers_per_tx` and `max_event_bindings_per_tx` default 10000; `observed_count` never rolls back.
- **Isolation / D-ISO / D9:** inactive/gasless/sequencer/cached paths do zero capture-private mutation; virtual logs never reach receipt/RPC/bloom/indexer/consensus.
- **Formatting/lint:** scoped `cargo +nightly fmt -p <pkg>` per package; `--all --check` is record evidence; never auto-format submodule.

## Review Focus

The five r11-specific failure modes; each pinned by a task below.

- **Native LimitExceeded not through real builder** — hand-crafted `CaptureOutcome::LimitExceeded` bypasses `execute_best_transactions`. Pinned in **Task 1** (r11-NATIVE-LIMIT-DENY).
- **Hook-hit indistinguishable from not-run** — inactive CREATE triggers sentinel=0 but a hit-with-no-derivation also would. Pinned in **Task 3** (r11-DERIVE-CTRL).
- **Per-SHA evidence incomplete** — missing raw nextest output, missing cargo tree features. Pinned in **Task 5** (r11-EVIDENCE-CONTRACT).
- **NT2 evidence fabricated** — claiming RED baseline without actual reproduction. Pinned in **Task 2** (r11-NT2).
- **MR table names non-existent tests** — stale names from prior review rounds. Pinned in **Task 5** (r11-MR-TABLE).

---

### Task 0 — Cross-crate test isolation (PROD, Corrections A; r10-ADVANCE-CFG, r10-ADVANCE-TEST)

**Files:**
- Modify: `crates/rcs-filter/Cargo.toml` — add `[features] test-utils = []`
- Modify: `crates/rcs-filter/src/handle.rs` — gate `advance_to_approved_for_test` behind `#[cfg(any(test, feature = "test-utils"))]`
- Modify: `crates/builder/Cargo.toml` — add `rcs-filter = { path = "../rcs-filter", features = ["test-utils"] }` in `[dev-dependencies]`

**Interfaces:**
- Produces: `advance_to_approved_for_test` available in builder tests but NOT in production `cargo build -p rcs-filter`
- Dual-verification: production build NOT exposing symbol; builder tests compile+pass

- [ ] **Step 1 — gate the symbol:** Add `#[cfg(any(test, feature = "test-utils"))]` to `pub fn advance_to_approved_for_test` in `crates/rcs-filter/src/handle.rs`. Add `[features] test-utils = []` to `rcs-filter/Cargo.toml`. Add `rcs-filter = { path = "../rcs-filter", features = ["test-utils"] }` to `builder/Cargo.toml` `[dev-dependencies]`.
- [ ] **Step 2 — verify production isolation:** Run `cargo build -p rcs-filter`. Expected: compile success, symbol NOT exposed.
- [ ] **Step 3 — verify builder test access:** Run `cargo test -p xlayer-builder`. Expected: compile + RESIM test passes using `advance_to_approved_for_test`.
- [ ] **Step 4: commit** — `git add crates/rcs-filter/Cargo.toml crates/rcs-filter/src/handle.rs crates/builder/Cargo.toml && git commit -m "fix(rcs): test-utils feature for cross-crate advance_to_approved_for_test isolation (r10-ADVANCE-CFG)"`

---

### Task 1 — D7b fail-closed + loop-level Deny + mismatch-flag clear (PROD+TEST; r10-DMM-LOOP, r10-DMM-ABORT, r11-NATIVE-LIMIT-DENY)

**Files:**
- Modify: `crates/builder/src/flashblocks/rcs_capture.rs` — `create_end` three-way branch, `finish_capture` mismatch outcome, `CaptureInvariantError::CreateAddressMismatch` variant, clear flag on BOTH `finish_capture` AND `abort_capture`.
- Modify: `crates/builder/src/flashblocks/context.rs` — `fail_closed_screen` new arm.

**Interfaces:**
- Produces: `CaptureInvariantError::CreateAddressMismatch { create_index: usize }` (index only). `fail_closed_screen` arm → `record_local_deny(ObservationInvariant)` + `Some(Screen::Deny)`.
- Consumes: `LAST_SCREEN_DECISION` test-35 seam, `execute_best_transactions`, `record_local_deny`.

- [ ] **Step 1 (RED): write D-MM-GUARD test** — `create_address_mismatch_through_real_create_end_fails_closed`: via `#[cfg(test)]` seam force `e != a` on real successful `create_end`; assert `finish_capture ⇒ InvariantViolation{CreateAddressMismatch}` (NEVER `Complete`). Expected: RED on `a0e5d7b7`.
- [ ] **Step 2 (PROD): implement three-way `create_end`** — (a) `!is_ok()` → truncate; (b) `is_ok && address.is_none()` → revoke; (c) `is_ok && Some(a) && e != a` → set sticky `CreateAddressMismatch { create_index }`, do NOT `pop_checkpoint(true)`. Add enum variant. `observed_count` stays sticky. Release-safe (regular match/flag, never `debug_assert!`-only).
- [ ] **Step 3 (PROD): add `context.rs` `fail_closed_screen` arm** — `CaptureOutcome::InvariantViolation { reason: CreateAddressMismatch { .. } }` → bounded `error!` + `record_local_deny(ObservationInvariant)` + `Some(Screen::Deny)`. Compiler-mandatory.
- [ ] **Step 4 (r10-DMM-LOOP, TEST):** extend D-MM-GUARD to drive builder candidate-loop path → assert `LAST_SCREEN_DECISION = Some(Screen::Deny)`, `screen_tx` NOT called, no `BufferEntry`/pending, no commit. Falsifiable: deleting arm fails compile or test.
- [ ] **Step 5 (r10-DMM-ABORT, TEST):** `address_mismatch_flag_cleared_by_both_finish_and_abort` — (a) set flag to `Some` → `finish_capture` clears (assert `is_none()`); (b) set to `Some` → `abort_capture` clears (assert `is_none()`).
- [ ] **Step 6 (r11-NATIVE-LIMIT-DENY, TEST):** `native_transfer_limit_exceeded_through_real_builder_path_denies` — add candidates exceeding `max_native_transfers_per_tx`, drive through `execute_best_transactions` (the REAL builder loop, not hand-crafted outcomes). Assert: `Screen::Deny`, no pending in BufferPool, no commit, no receipt. Normal control: Allow/Audit when within limit. Falsifiable.
- [ ] **Step 7: run GREEN** — Run `cargo nextest run -p xlayer-builder create_address_mismatch_through_real_create_end_fails_closed address_mismatch_flag_cleared_by_both_finish_and_abort native_transfer_limit_exceeded_through_real_builder_path_denies`. Expected: PASS.
- [ ] **Step 8: commit** — `git add crates/builder/src/flashblocks/rcs_capture.rs crates/builder/src/flashblocks/context.rs && git commit -m "fix(rcs): fail-closed CREATE address mismatch + real-builder LimitExceeded Deny chain (r10-DMM + r11-NATIVE-LIMIT)"`

---

### Task 2 — Real builder-OpEvm → `screen_tx` end-to-end (TEST+SPEC; r10-T3-LEGACYVAL, r10-T3-DUALALIAS, r10-T3-AUDITMERGE, r10-T3-NT2-EVID, r11-NT2)

**Files:**
- Test: `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]`.
- SPEC: state in plan/MR that `matching.rs`/`handle.rs` need **no** production change (T3f).
- KEEP: `crates/rcs-filter/src/integration_tests.rs` t3a–t3f retained as supplementary.

**Interfaces:** Consumes Task 0 (`advance_to_approved_for_test`), Task 1 D7b. `FilterHandle::screen_tx`, `quota_hash::encode_and_hash`.

- [ ] **Step 1 (r10-T3-NT2-EVID + r11-NT2, SPEC):** `original_nt2_internal_create_denied_through_real_screen_tx` — real `OpEvm` tx with internal CREATE non-zero endowment → `Complete(stream)` → `FilterHandle` with `transfer.*` deny rule → `Screen::Deny`. Per r11-NT2, MUST state EITHER genuine reproduction (exact pre-fix baseline SHA, fixture input, actual pre/post-fix output) OR honest non-reproduction (scenario cannot be reproduced, exact baseline/fixture attempted, actual result). No fabricated RED baseline.
- [ ] **Step 2 — top-level + one-rule-both:** `top_level_native_denied_through_real_screen_tx`, `one_transfer_rule_denies_both_toplevel_and_internal_real_stream`.
- [ ] **Step 3 — budgets split:** `real_stream_native_budget_limit_plus_one_denies`, `real_stream_matcher_binding_budget_limit_plus_one_denies`. Distinct budgets.
- [ ] **Step 4 — RESIM via public `screen_tx`:** drive public `screen_tx` twice: first→`AuditPending`; pool→`Approved` via `advance_to_approved_for_test`; second→`AuditApproved`. Negative control: changed-log re-sim→different quota_hash→`Drop`.
- [ ] **Step 5 (r10-T3-LEGACYVAL):** `legacy_value_rule_still_fires_on_real_capture_superset_stream` — negative control: `ScreenInput.value = 0` same stream → Allow, proving rule fires via tx-level `value` binding.
- [ ] **Step 6 (r10-T3-DUALALIAS):** `dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream` — two-stream comparison with DISTINGUISHABLE budget boundaries: Stream-A (1 transfer, budget=3 → Allow/Audit), Stream-B (2 transfers, `(2+1)²=9≥3` → Deny or assert `checked_binding_count` from error path). Rule-itself Deny alone insufficient.
- [ ] **Step 7 (r10-T3-AUDITMERGE):** `audit_type_action_merge_action_content_no_empty_audit_on_real_stream` — ALL THREE: (a) same `audit_type` → ONE action per `(audit_type, log_index)` with bound `transfer.from/to/value`; (b) different `audit_type` → SEPARATE actions; (c) NEGATIVE CONTROL: all-null/no physical event → NO empty Audit. `audit_type` is KEY of actions mapping.
- [ ] **Step 8 (T3f + KEEP):** record "no matching.rs/handle.rs production change"; retain rcs-filter t3a–t3f.
- [ ] **Step 9: run** — `cargo nextest run -p xlayer-builder` for Steps 1–7; `cargo test -p rcs-filter` for KEEP. Expected: ALL PASS.
- [ ] **Step 10: commit** — `git commit -m "test(rcs): real OpEvm->screen_tx end-to-end with r10-strengthened + r11-NT2 evidence"`

---

### Task 3 — Restore regressions + true overflow + exact T2-ORIGIN + DERIVE-CTRL (TEST+PROD; r10-REGRESS-KEEP, r10-OVF-ASSERT, r10-T2-ORIGIN, r11-DERIVE-CTRL)

**Files:**
- Test: `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]`.

**Interfaces:** Consumes Task 1 D7b, Task 4 derivation_ran sentinel.

- [ ] **Step 1 (r10-REGRESS-KEEP):** restore six regressions by exact name: `top_level_call_with_value_emits_transfer`, `root_success_child_revert_keeps_root_drops_child`, `whole_tx_root_failure_drops_all`, `create_early_failure_no_address_or_event_leak`, `next_capture_starts_clean`, `create_is_ok_early_return_no_endowment_leak`.
- [ ] **Step 2 (r10-OVF-ASSERT):** `create_nonce_overflow_is_ok_true_address_none_revoked`, `create2_nonce_overflow_is_ok_true_address_none_revoked` — assert `is_ok()==true` AND `address.is_none()` AND sticky `observed_count` equals pre-overflow count.
- [ ] **Step 3 — rename mislabelled collision test:** `internal_create2_nonce_overflow_endowment_revoked` → `internal_create2_eip684_collision_endowment_revoked`.
- [ ] **Step 4 (r10-T2-ORIGIN):** `eip7702_origin_itself_delegated_runs_inner_call_and_create` — CREATE recipient equals EXACT `CreateOutcome.address` (or verified nonce-derived). Any wrong fourth address MUST FAIL. Also assert `result.is_success()`.
- [ ] **Step 5 (r11-DERIVE-CTRL, TEST):** `non_create_control_hook_hit_zero_alongside_t1_derive` — control executes real tx with NO CREATE at any depth (pure CALL-only), asserts hook-hit counter/sentinel = 0. Separate test: inactive CREATE path → hook-hit counter > 0, `derivation_ran = false`. Proves sentinel distinguishes not-hit from hit-then-early-returned. Per-test-case reset required.
- [ ] **Step 6: run** — `cargo nextest run -p xlayer-builder`. Expected: ALL PASS.
- [ ] **Step 7: commit** — `git commit -m "test(rcs): restore regressions + overflow + exact T2-ORIGIN + DERIVE-CTRL (r10 + r11)"`

---

### Task 4 — derivation_ran computed sentinel + proof-boundary hardening (TEST+PROD; r10-T1-DERIVE)

**Files:**
- Modify: `crates/builder/src/flashblocks/rcs_capture.rs` — `create` hook: set `derivation_ran = true` when journal read for address derivation. Add separate "hook hit" counter.
- Test: `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]`.

**Interfaces:**
- Produces: `derivation_ran: bool` — `true` = journal read for derivation; `false` = not-hit sentinel. Hook-hit counter: `true` = hook invoked, `false` = not invoked. `false` cannot mean both.
- Consumes: r11-DERIVE-CTRL in Task 3 for the non-CREATE zero-hit control.

- [ ] **Step 1 (PROD):** In `create` hook, set `derivation_ran = true` when journal read executes. Add hook-hit counter set to `true` whenever `create` hook is entered (before the depth/value gates). Both counters reset per capture.
- [ ] **Step 2 (TEST):** `derivation_ran_true_when_journal_read_for_address_derivation` — active CREATE, non-zero → `derivation_ran == true`. `derivation_ran_false_when_create_hook_not_hit` — inactive path → `derivation_ran == false`.
- [ ] **Step 3 — proof-boundary hardening:** `inactive_create_no_mutation_and_no_derivation_at_create_return`; `create_recipients_balanced_before_finish_capture`; `abort_capture_mid_window_leaves_state_clean`; `two_consecutive_txs_capture_independently`.
- [ ] **Step 4: run** — `cargo nextest run -p xlayer-builder`. Expected: ALL PASS.
- [ ] **Step 5: commit** — `git commit -m "fix(rcs): derivation_ran computed sentinel + T1 proof-boundary hardening (r10-T1-DERIVE)"`

---

### Task 5 — §8.E evidence bundle + ID→artifact→evidence table (SPEC/KEEP; all r10-EV + r11-EVIDENCE-CONTRACT + r11-MR-TABLE)

**Files:**
- SPEC: output to MR description + `output.md` (Stage 3.0/5.0 produce; Stage 2.0 specifies).
- KEEP: `docs/superpowers/`, `.superpowers/`, Terran review ledgers.

- [ ] **Step 1 (r10-EV-LOG + r11-EVIDENCE-CONTRACT):** Stage 3.0 MUST produce for the new head SHA: **(a)** full raw `cargo nextest run -p xlayer-builder -p rcs-filter` output with exact command, exit code, per-test-ID pass/fail/duration (NOT bare counts); **(b)** `cargo tree -e features -p xlayer-node` output showing `rcs-filter/test-utils` NOT enabled in default production dep graph; **(c)** `cargo test -p xlayer-builder` output proving cross-crate tests available (test-utils enabled).
- [ ] **Step 2 (r11-MR-TABLE):** MR description body must: list only tests existing at submission SHA by exact function names; drop any names confirmed non-existent by prior review rounds; label revision **r11**; link full verified Lark doc URL + canonical sha256 `49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836`.
- [ ] **Step 3 (r10-EV-CI):** Pipeline clippy evidence recorded; `cargo +nightly fmt --all --check` exit 0 + `cargo clippy --all-targets --workspace -- -D warnings` 0/0, each with command + exit code.
- [ ] **Step 4 (r10-EV-T35):** test-35 M1/M2/M3 marked retained (blob proven byte-identical) vs re-tested.
- [ ] **Step 5 (r10-EV-SUPERPOWERS):** preserve `docs/superpowers/`, `.superpowers/`, checkpoint tarballs, Terran review ledgers.
- [ ] **Step 6 — ID→artifact→evidence table:** full table mapping each r10 + r11 ID to test function name / code site / assertion class / evidence location. 22+ IDs total.
- [ ] **Step 7: commit** — `git commit -m "spec(rcs): r11 evidence contract + ID->artifact->evidence table (r10-EV + r11-EVIDENCE + r11-MR-TABLE)"`

---

## r11 Acceptance ID → Test Name Mapping

| ID | Class | Name/Location | Assertion |
|---|---|---|---|
| r10-ADVANCE-CFG | PROD | `rcs-filter/Cargo.toml` `[features]`, `handle.rs` gate | `cargo build -p rcs-filter` NOT expose; `cargo test -p xlayer-builder` compile+pass; `cargo tree` default prod dep graph |
| r10-ADVANCE-TEST | TEST | RESIM test at builder `rcs_capture.rs` | Compiles; calls `advance_to_approved_for_test` |
| r10-T3-LEGACYVAL | TEST | `legacy_value_rule_still_fires_on_real_capture_superset_stream` | Negative control: `value=0` same stream → Allow |
| r10-T3-DUALALIAS | TEST | `dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream` | Two-stream: budget-within → Allow/Audit; exceeded → Deny |
| r10-T3-AUDITMERGE | TEST | `audit_type_action_merge_action_content_no_empty_audit_on_real_stream` | Same→dedup; diff→retain; all-null→no empty Audit; audit_type=key |
| r10-T3-NT2-EVID | SPEC | Plan/MR + r11-NT2 | Reproduce-or-honest-non-reproduction |
| r10-DMM-LOOP | TEST | `create_address_mismatch_through_real_create_end_fails_closed` (extended) | LAST_SCREEN_DECISION=Deny, screen_tx NOT called |
| r10-DMM-ABORT | TEST | `address_mismatch_flag_cleared_by_both_finish_and_abort` | Set→finish clears; set→abort clears |
| r10-T1-DERIVE | PROD+TEST | `derivation_ran_true_when_journal_read` + `derivation_ran_false_when_not_hit` | true=journal read; false=not-hit; separate hook-hit counter |
| r10-OVF-ASSERT | TEST | `create_nonce_overflow_is_ok_true_address_none_revoked` + `create2_nonce_overflow_is_ok_true_address_none_revoked` | is_ok()==true AND address.is_none() AND sticky count |
| r10-T2-ORIGIN | TEST | `eip7702_origin_itself_delegated_runs_inner_call_and_create` | CREATE to==exact CreateOutcome.address; any wrong→FAIL |
| r10-REGRESS-KEEP | KEEP | 6 regressions by exact name | Present+passing |
| r10-EV-LOG | SPEC | Raw `cargo nextest` output both crates | Full per-test-ID output + exit code |
| r10-EV-TABLE | SPEC | MR test table | All names exist at SHA; label r10→r11 |
| r10-EV-CI | SPEC | CI pipeline + `fmt --check` 0 + `clippy -D warnings` 0/0 | Command+exit code |
| r10-EV-T35 | SPEC/KEEP | test-35 M1/M2/M3 | Retained vs re-tested |
| r10-EV-SUPERPOWERS | KEEP | `docs/superpowers/` + `.superpowers/` | Preserved intact |
| **r11-NATIVE-LIMIT-DENY** | TEST | `native_transfer_limit_exceeded_through_real_builder_path_denies` | `execute_best_transactions` → Deny; no pending/commit/receipt; normal control |
| **r11-DERIVE-CTRL** | TEST | `non_create_control_hook_hit_zero_alongside_t1_derive` | Pure CALL-only → hook-hit=0; inactive CREATE → hit>0, derivation_ran=false |
| **r11-EVIDENCE-CONTRACT** | SPEC | `cargo nextest` raw + `cargo tree -e features -p xlayer-node` + `cargo test -p xlayer-builder` | test-utils NOT in default prod; cross-crate tests available |
| **r11-NT2** | SPEC | NT2 evidence in plan/MR | Reproduce-or-honest-non-reproduction; no fabricated RED |
| **r11-MR-TABLE** | SPEC | MR description body | Exact test names, r11 label, full URL + sha256; no stale names |