# MR77 RCS Filter — r11 Implementation Plan (round-5 delta) — Nested CREATE Endowment + Top-level native Transfer Unification

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **CURRENT EXECUTABLE PLAN (artifact A1-02).** Supersedes
> `2026-10-13-rcs-native-transfer-mr77-fixes-plan-r11.md` (same design revision r11, but written before the
> baseline `a0e5d7b7` tree was read) and `2026-10-13-...-plan-r10.md` / `2026-10-12-...-plan.md`. All three are
> retained as history — do not delete them (r10-EV-SUPERPOWERS).
>
> **Why this revision exists:** the superseded r11 plan re-planned work that is *already implemented and passing*
> at the baseline, and it named a package (`xlayer-node`) that does not exist in this workspace. This plan is a
> **verified delta**: every "already done" claim below was read out of the real `a0e5d7b7` tree, and every task
> is work that is genuinely absent there.

**Goal:** Re-submit MR77 at a new head SHA that closes the five Stage-5.0 round-4 findings, so the delegated MR
acceptance passes against design revision r11.

**Architecture:** The approved mechanism (D1–D9 / D7a / D-ISO / D7b) is **already implemented** at the baseline
and must not be reopened. This plan adds two missing tests, one `#[cfg(test)]` observability seam, one
reproduction experiment, and two evidence/metadata artifacts. Production behavior of
`rcs_capture.rs` / `context.rs` / `handle.rs` / `matching.rs` does **not** change.

**Tech Stack:** Rust; revm pinned (revm-handler 20.0.3 / revm-inspector 21.0.3 / revm-interpreter 37.0.3);
op-revm (`OpSpecId::JOVIAN` ⇒ `PRAGUE` ⇒ EIP-7702 active); `alloy_op_evm::OpEvm` composite inspector;
`cargo` / `cargo nextest` / `just`. Repo `github/xlayer-reth`, branch `xl/xlop-1217/0928-1135`, reused MR
<https://gitlab.okg.com/github/xlayer-reth/-/merge_requests/77>. Jira XLOP-1228 (Task) under Epic XLOP-1217.
Workspace packages used here: `xlayer-builder`, `rcs-filter`, `xlayer-reth-node`.

**Spec:** `docs/superpowers/specs/2026-10-12-rcs-native-transfer-mr77-fixes-design.md` (in-repo mirror; canonical
publication <https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac>, revision **r11**, canonical
body sha256 `49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836`). Design PASS
`starport-rcs-design-review-r11-pass-20261013` (Bus `recvxVksp5gum4`), `approval_mode=delegated_agent`. The PASS
is **design only**: the MR implementation is still CHANGES_REQUESTED at `a0e5d7b7` and grants no
merge / deploy / release / MR-approve / scope expansion.

---

## Global Constraints

Every task's requirements implicitly include this section.

- **Plan baseline — `a0e5d7b73a669d50ed18f65d1c40c52a7a70a230`** (MR77 head, verified via `git ls-remote` and
  GitLab MR `diff_refs.head_sha`). **NOT** `21a311e9`, **NOT** `9867fd48`, **NOT** `714e1293`.
- **Baseline ≠ this workspace.** The checked-out worktree is at `714e1293` (the *pre-fix* R12 baseline, with the
  `depth > 0` gates still live at `rcs_capture.rs:215` / `:232` / `:412` and a 1 527-line `rcs_capture.rs`). The
  baseline tree is 3 718 lines. **Task 0 is mandatory** — implementing on top of `714e1293` would re-create work
  that already exists and would lose the whole r3–r11 implementation series.
- **Node package is `xlayer-reth-node`** (`bin/node/Cargo.toml:2`), carried as a reviewer-issued execution
  correction with the r11 PASS. The feature-graph evidence command is
  `cargo tree -p xlayer-reth-node -e normal,build,features` — default feature set, **no** `--all-features`, dev
  edges excluded. `xlayer-node` does not exist; do not use it.
- **No production behavior change.** `rcs_capture.rs` and `context.rs` gain only `#[cfg(test)]` code (Task 2's
  seam) and tests. `crates/rcs-filter/src/{handle.rs,matching.rs}` get **no** change at all (D3 / T3f).
- **Scope:** reuse MR77 and branch `xl/xlop-1217/0928-1135`. No merge, deploy, release, force-push, or push to a
  target branch. No extra repo scope, no new Jira. Preserve others' edits and the `deps/optimism` submodule
  pointer `0dca4b893f3d85dfa02209af9ff031822c4e3043` — never touch it.
- **FAIL rule (binding, design §8.r6):** acceptance = every TEST present **and** passing ∧ every PROD in the diff
  ∧ every SPEC stated ∧ every KEEP retained. Missing, renamed-away, `#[ignore]`d or weakened **any one** item of
  **any** class = FAIL for the whole re-submission. A subset is a FAIL.
- **Budgets:** `max_native_transfers_per_tx` and `max_event_bindings_per_tx` both default `10_000`, both must be
  `>= 1` (`0` is a startup error even when the Filter is disabled). `observed_count` is sticky — revocation never
  decrements it.
- **D9 isolation:** virtual native `Transfer` logs never reach a receipt, RPC log, bloom, the indexer, or
  consensus state. Inactive / gasless / sequencer / cached / self-generated paths perform zero capture-private
  mutation (D-ISO / AC1).
- **Formatting / lint:** `cargo +nightly fmt --all --check` exit 0 and
  `cargo clippy --all-targets --workspace -- -D warnings` 0 warnings / 0 errors, each recorded with its command
  and exit code. Never format the submodule.
- **Preserve history:** `docs/superpowers/` (all plans and specs, including the superseded ones), `.superpowers/`,
  checkpoint tarballs, Terran review ledgers (r10-EV-SUPERPOWERS).

## Carried forward — verified present at `a0e5d7b7`, do NOT re-implement

Read out of the baseline tree; Task 0 re-asserts each one mechanically.

| Item | Where, at `a0e5d7b7` |
|---|---|
| r10-ADVANCE-CFG | `crates/rcs-filter/Cargo.toml:30-31` `[features] default = []` + `test-utils`; `handle.rs:590,600` `#[cfg(any(test, feature = "test-utils"))]`; `crates/builder/Cargo.toml:152` `rcs-filter = { workspace = true, features = ["test-utils"] }` in `[dev-dependencies]` |
| r10-ADVANCE-TEST | `rcs_capture.rs::real_stream_screen_tx_and_screen_existing_consistent` |
| r10-T3-LEGACYVAL / DUALALIAS / AUDITMERGE | `legacy_value_rule_still_fires_on_real_capture_superset_stream`, `dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream`, `audit_type_action_merge_action_content_no_empty_audit_on_real_stream` |
| r10-DMM-LOOP / DMM-ABORT | `context.rs::loop_level_fail_closed_tests::loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt`; `rcs_capture.rs::address_mismatch_flag_cleared_by_both_finish_and_abort` |
| r10-T1-DERIVE | `create` hook computes `derivation_ran` (`rcs_capture.rs:490-523`); three sentinel tests |
| r10-OVF-ASSERT | `create_nonce_overflow_is_ok_true_address_none_revoked`, `create2_nonce_overflow_is_ok_true_address_none_revoked` |
| r10-T2-ORIGIN | `eip7702_origin_itself_delegated_runs_inner_call_and_create` |
| r10-REGRESS-KEEP | all six by exact name: `top_level_call_with_value_emits_transfer`, `root_success_child_revert_keeps_root_drops_child`, `whole_tx_root_failure_drops_all`, `create_early_failure_no_address_or_event_leak`, `next_capture_starts_clean`, `create_is_ok_early_return_no_endowment_leak` |
| r5-REN-COLLISION | renamed to `internal_create2_eip684_collision_endowment_revoked` |
| r5-T3-KEEP | `rcs-filter/src/integration_tests.rs` `t3a`–`t3f` retained as supplementary matcher coverage |
| test-35 | `context.rs::loop_level_fail_closed_tests::loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt`, M1/M2/M3 |

## What is genuinely missing at `a0e5d7b7` — this plan's whole scope

| Finding (round-4, Bus `recvxVecklaw7X`) | Verified gap in the baseline tree | Task |
|---|---|---|
| 1. Native `LimitExceeded` not proven through the real builder | `real_stream_native_budget_limit_plus_one_denies` asserts the `CaptureOutcome` returned by the `run_capturing_tx` helper; `execute_best_transactions` appears nowhere in `rcs_capture.rs` outside comments. No loop-level native-budget test exists. | 1 |
| 2. Inactive-CREATE hook hit indistinguishable from "hook never ran" | `CREATE_GATE_SNAPSHOT` is a `Cell<(usize, usize, bool, bool)>` initialised to `(0, 0, false, false)` with **no reset site**; the inactive-CREATE path writes exactly `(.., false, false)`. A pure-CALL transaction that never enters the `create` hook reads the identical tuple. | 2 |
| 3. Per-SHA raw evidence incomplete; default node feature tree not shown | `specs/2026-10-13-...-r10-evidence.md` §3 contains the literal placeholder `... (per-test lines)` and the note "durations elided"; no `cargo tree` output anywhere; §4.1 records `cargo check -p xlayer-builder --all-targets`, not `cargo test -p xlayer-builder`. | 4 |
| 4. NT2 not reproduced on a pre-fix baseline | r10-evidence §6.1 argues reproduction "would require reverting" the fix. That is an argument, not an attempted experiment with an observed result — and the pre-fix tree `714e1293` is in fact available. | 3 |
| 5. MR body still labels r4; lists tests that no longer exist | MR77 description says "Design revision (MR77 fixes): r4 … `7a788723`". **14** test names it lists are absent from the baseline source (verified by name search over `rcs_capture.rs`, `context.rs`, `handle.rs`, `integration_tests.rs`). | 5 |

## File Structure

| File | Responsibility, this plan |
|---|---|
| `crates/builder/src/flashblocks/context.rs` | `#[cfg(test)] mod loop_level_fail_closed_tests` gains the third loop-level test (native `LimitExceeded`) and its boundary / loop-continues controls. No production edit. |
| `crates/builder/src/flashblocks/rcs_capture.rs` | `#[cfg(test)]` seam gains a create-hook-hit counter plus a reset helper; the test module gains the non-CREATE zero-hit control and the inactive-CREATE hit>0 assertion. No production edit. |
| `docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md` | **New.** The r11 per-SHA evidence record: raw `nextest` for both packages, `cargo tree`, `cargo test -p xlayer-builder`, fmt/clippy, test-35 retained-vs-re-tested, NT2 result, ID→artifact→evidence table. |
| `docs/superpowers/specs/2026-10-12-rcs-native-transfer-mr77-fixes-design.md` + `...-r11-canonical-body.txt` + `specs/history/...-r4.md` | **New in the tree** (currently untracked workspace files): commit the r11 design mirror so the MR carries the spec its evidence cites. |
| `docs/superpowers/plans/2026-10-14-...-plan-r11.md` (this file) | **New.** Committed alongside; prior plans retained. |
| MR77 description (GitLab, not a file) | Rewritten in Task 5. |

## Review Focus

Five conditions the spec implies that no test at the baseline exercises, most likely to bite first. Each is
pinned to the task that owns the code.

- **Off-by-one at the native budget in the real loop.** `max_native_transfers_per_tx = N` with exactly `N`
  observations must **not** deny; only `N+1` may. A test that only ever drives the over-limit case would pass
  against an implementation that denies at `N`. → pinned in **Task 1, Step 4**.
- **The candidate loop must survive a fail-closed deny.** After one candidate is denied for a budget overflow,
  the loop must continue and screen the *next* candidate normally (capture state reset, no sticky deny). Nothing
  at the baseline asserts the loop's post-deny behavior. → pinned in **Task 1, Step 5**.
- **`max_native_transfers_per_tx = 0` must be a startup error even when the Filter is disabled** (AC5 / D6).
  `config.rs:145` implements it; no test drives it through the disabled-filter path. → pinned in
  **Task 1, Step 6**.
- **Thread-local test-seam bleed.** `CREATE_GATE_SNAPSHOT` has no reset site and `FORCE_*` are one-shot
  `Cell`s. Under `cargo test` (threads reused across tests, unlike `nextest`'s process-per-test) a stale tuple
  can make a later assertion pass for the wrong reason. → pinned in **Task 2, Steps 1 and 3**.
- **`derivation_ran == false` on a legitimate CREATE2.** CREATE2 derives from salt and init-code and reads no
  journal, so it legitimately reports `false`; a reader must still be able to tell it from "the hook never
  ran". → pinned in **Task 2, Step 4**.

---

### Task 0 — Re-base onto the real plan baseline and re-assert the carried-forward state

No code change. Produces the worktree every later task edits, and the mechanical proof that the r10 items are
already satisfied (so no later task silently re-implements them).

**Files:**
- Modify: none. Checkout only.
- Preserve: untracked `docs/superpowers/**`, `.superpowers/`, `superpowers-context.tar.gz`.

**Interfaces:**
- Produces: a worktree at `a0e5d7b7` on branch `xl/xlop-1217/0928-1135`, with `docs/superpowers/` containing both
  the baseline's four tracked files and this workspace's design mirror + plans.
- Consumes: nothing.

- [ ] **Step 1: stash nothing, copy the superpowers docs aside**

```bash
cd xlayer-reth
mkdir -p /tmp/sp-carry && cp -a docs/superpowers /tmp/sp-carry/
git status --short            # expect only: ?? .superpowers/  ?? docs/superpowers/  ?? superpowers-context.tar.gz
```

- [ ] **Step 2: fetch and check out the baseline**

```bash
git fetch --no-tags origin 'refs/heads/xl/xlop-1217/0928-1135:refs/remotes/origin/xl/xlop-1217/0928-1135'
git checkout -B xl/xlop-1217/0928-1135 a0e5d7b73a669d50ed18f65d1c40c52a7a70a230
git rev-parse HEAD            # expect a0e5d7b73a669d50ed18f65d1c40c52a7a70a230
git submodule status deps/optimism   # expect 0dca4b893f3d85dfa02209af9ff031822c4e3043, unchanged
```

- [ ] **Step 3: restore the carried docs over the baseline's own**

Copy `/tmp/sp-carry/superpowers/*` back into `docs/superpowers/` **without** deleting the baseline's
`plans/2026-10-13-...-plan-r10.md` or `specs/2026-10-13-...-r10-evidence.md`.

Expected afterwards: `docs/superpowers/` holds the r10 plan, the r10 evidence spec, `lark-review-doc.md`, the
`2026-10-12` plan, the superseded `2026-10-13` r11 plan, this plan, the design mirror, the r11 canonical body,
and `specs/history/...-r4.md`.

- [ ] **Step 4: re-assert the carried-forward state (fail loudly if any is missing)**

```bash
test -n "$(grep -n 'test-utils' crates/rcs-filter/Cargo.toml)" && echo CFG-OK
grep -c 'cfg(any(test, feature = "test-utils"))' crates/rcs-filter/src/handle.rs      # expect >= 2
grep -c 'features = \["test-utils"\]' crates/builder/Cargo.toml                       # expect 1
for n in real_stream_screen_tx_and_screen_existing_consistent \
         legacy_value_rule_still_fires_on_real_capture_superset_stream \
         dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream \
         audit_type_action_merge_action_content_no_empty_audit_on_real_stream \
         loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt \
         loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt \
         address_mismatch_flag_cleared_by_both_finish_and_abort \
         derivation_ran_true_when_journal_read_for_address_derivation \
         derivation_ran_false_when_create_hook_not_hit \
         derivation_ran_false_but_capture_body_ran_for_create2 \
         create_nonce_overflow_is_ok_true_address_none_revoked \
         create2_nonce_overflow_is_ok_true_address_none_revoked \
         eip7702_origin_itself_delegated_runs_inner_call_and_create \
         internal_create2_eip684_collision_endowment_revoked \
         top_level_call_with_value_emits_transfer \
         root_success_child_revert_keeps_root_drops_child \
         whole_tx_root_failure_drops_all \
         create_early_failure_no_address_or_event_leak \
         next_capture_starts_clean \
         create_is_ok_early_return_no_endowment_leak; do
  c=$(grep -Rc "fn $n" crates/builder/src/flashblocks crates/rcs-filter/src | awk -F: '{s+=$2} END {print s}')
  [ "$c" -ge 1 ] || echo "MISSING: $n"
done
```

Expected: `CFG-OK`, counts as annotated, and **no** `MISSING:` line.

- [ ] **Step 5: baseline green run (the "before" record)**

Run: `cargo nextest run -p xlayer-builder -p rcs-filter`
Expected: exit 0, all tests pass. Save the raw output to `/tmp/nextest-a0e5d7b7.log` — it is the comparison point
for Task 4 and the proof that the new tests in Tasks 1–2 are the only delta.

- [ ] **Step 6: commit the carried docs**

```bash
git add docs/superpowers
git commit -m "docs(superpowers): carry the r11 design mirror and the round-5 delta plan onto the MR77 head"
```

---

### Task 1 — r11-NATIVE-LIMIT-DENY: native budget overflow denies through the real builder loop

**Files:**
- Test: `crates/builder/src/flashblocks/context.rs`, inside the existing `#[cfg(test)] mod loop_level_fail_closed_tests`.
- Modify: none in production.

**Interfaces:**
- Consumes: `FilterHandle::for_test(config: FilterConfig, rules: RuleSet, clock: Arc<dyn Clock>)`;
  `FilterConfig { max_native_transfers_per_tx: usize, max_event_bindings_per_tx: usize, .. }`;
  `build_ctx(filter: Arc<FilterHandle>) -> FlashblocksBuilderCtx`;
  `ctx.execute_best_transactions(&mut ExecutionInfo, &mut State, &mut RecordingTxs, &pool, TransactionLimits)`;
  `LAST_SCREEN_DECISION: Cell<Option<Screen>>`; `erc20_transfer_emitter_code()`; `rcs_filter::test_support::golden`
  (`RULE_SCENARIO_A`, `token_x()`); `filter.buffered_len()`, `filter.buffer_status(&B256)`.
  The loop builds its own inspector as `RcsInspector::new(filter.max_native_transfers_per_tx())`
  (`context.rs:735`), so the budget is driven purely by `FilterConfig` — **no new production seam is needed**.
- Produces: `two_native_transfers_then_erc20_transfer_emitter_code() -> Vec<u8>` for reuse by later steps.

- [ ] **Step 1: write the fixture bytecode helper**

Add next to `create_then_erc20_transfer_emitter_code()`:

```rust
/// Runtime bytecode: two 1-wei `CALL` transfers to two distinct recipients, then the
/// `RULE_SCENARIO_A` ERC20 `Transfer` log. Two internal native observations, so a budget of 1
/// overflows at the second and a budget of 2 does not — and the real log keeps the CONTROL run
/// reachable to `AuditPending`, which is what makes the fail-closed negative meaningful.
fn two_native_transfers_then_erc20_transfer_emitter_code() -> Vec<u8>;
```

Recipients: `Address::repeat_byte(0xA1)` and `Address::repeat_byte(0xA2)`. The emitting account at
`golden::token_x()` must be funded with at least 2 wei.

- [ ] **Step 2 (RED): write the failing fail-closed test**

```rust
/// The native-transfer budget fail-closed chain, driven through the ACTUAL candidate loop rather
/// than a helper's `CaptureOutcome`: the loop's own `RcsInspector` overflows at `limit + 1`,
/// `finish_capture` returns `LimitExceeded`, and `fail_closed_screen` must turn that into a Deny
/// WITHOUT reaching `screen_tx`.
#[tokio::test]
async fn loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt() {
    // FilterConfig { max_native_transfers_per_tx: 1, ..Default::default() } + the 2-transfer factory.
    // Asserts, mirroring loop_level_create_address_mismatch_...:
    //   LAST_SCREEN_DECISION == Some(Screen::Deny)      // M2: Deny, never Drop
    //   filter.buffer_status(&candidate_hash).is_none() // M1: screen_tx was skipped
    //   filter.buffered_len() unchanged                 // M1
    //   info.executed_transactions is empty, no receipt, zero cumulative gas
    //   sender nonce and balance unchanged across the loop        // M3: no commit
}
```

Run: `cargo nextest run -p xlayer-builder loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt`
Expected: FAIL — the test function does not exist yet / the fixture helper is unresolved.

- [ ] **Step 3: implement the fixture and the test body; run GREEN**

Run: `cargo nextest run -p xlayer-builder loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt`
Expected: PASS.

Falsifiability check — record the result in Task 4's evidence: temporarily widen the
`CaptureOutcome::LimitExceeded` arm of `fail_closed_screen` to return a non-`Deny` decision; the
`LAST_SCREEN_DECISION` assertion must FAIL. Revert the temporary edit before committing.

- [ ] **Step 4 (Review Focus — off-by-one): the exactly-at-limit control**

In the same test, add a CONTROL run of the **same** candidate with
`max_native_transfers_per_tx: 2`. Assert `LAST_SCREEN_DECISION == Some(Screen::Audit)` (the candidate matches
`RULE_SCENARIO_A`) and `filter.buffer_status(&candidate_hash) == Some(BufferStatus::AuditPending)` — i.e. exactly
`N` observations do **not** deny. Without this control a deny-at-`N` implementation would pass Step 3.

Run: same command. Expected: PASS.

- [ ] **Step 5 (Review Focus — the loop survives a deny): second candidate still screens**

```rust
/// After a budget-overflow Deny the loop must keep going: the NEXT candidate in the same
/// `execute_best_transactions` call is screened normally and buffers, proving capture state was
/// reset and the deny is not sticky.
#[tokio::test]
async fn loop_level_native_limit_deny_does_not_poison_the_next_candidate() {
    // Two pooled candidates from the same sender (nonce 0 then 1): #0 hits the 2-transfer factory
    // under max_native_transfers_per_tx: 1 → Deny; #1 hits the plain erc20_transfer_emitter_code
    // factory → AuditPending.
    // Asserts: buffer_status(#0).is_none(); buffer_status(#1) == Some(BufferStatus::AuditPending);
    // buffered_len() == 1.
}
```

Run: `cargo nextest run -p xlayer-builder loop_level_native_limit_deny_does_not_poison_the_next_candidate`
Expected: first FAIL (absent), then PASS.

- [ ] **Step 6 (Review Focus — zero budget): `0` is a startup error even with the Filter disabled**

```rust
/// AC5 / D6: `max_native_transfers_per_tx == 0` is rejected at configuration validation even when
/// the Filter is disabled, so a misconfiguration can never silently disable the budget.
#[test]
fn zero_native_budget_is_a_config_error_even_when_filter_disabled();
```

Build a `FilterConfig` with `enabled: false` and `max_native_transfers_per_tx: 0`, call the crate's config
validation, and assert the `FilterError::Config` message
`"rcs-filter max_native_transfers_per_tx must be >= 1"` (`crates/rcs-filter/src/config.rs:145-149`). Put this
test in `crates/rcs-filter/src/config.rs`'s own `#[cfg(test)]` module if the validator is private there;
otherwise keep it beside the other loop-level tests.

Run: `cargo nextest run -p rcs-filter zero_native_budget_is_a_config_error_even_when_filter_disabled`
Expected: first FAIL (absent), then PASS.

- [ ] **Step 7: full package run + fmt/clippy**

```bash
cargo nextest run -p xlayer-builder -p rcs-filter
cargo +nightly fmt -p xlayer-builder -p rcs-filter
cargo clippy --all-targets --workspace -- -D warnings
```

Expected: nextest exit 0 with the baseline's test count plus the new tests; fmt clean; clippy 0/0.

- [ ] **Step 8: commit**

```bash
git add crates/builder/src/flashblocks/context.rs crates/rcs-filter/src/config.rs
git commit -m "test(rcs): native budget LimitExceeded denies through the real candidate loop (r11-NATIVE-LIMIT-DENY)"
```

---

### Task 2 — r11-DERIVE-CTRL: make the create-hook sentinel distinguish "not hit" from "hit, no derivation"

**Files:**
- Modify: `crates/builder/src/flashblocks/rcs_capture.rs` — `#[cfg(test)]` seam only: add a hit counter and a
  reset helper; write the counter at the top of the `create` hook, before the `!self.active` gate.
- Test: `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]` module.

**Interfaces:**
- Produces, both `#[cfg(test)]`, beside `CREATE_GATE_SNAPSHOT` (`rcs_capture.rs:154-165`):
  - `pub(crate) static CREATE_HOOK_HITS: std::cell::Cell<usize>` — incremented once on **every** entry to the
    `create` hook, before the active gate, so `0` means the hook was never invoked.
  - `pub(crate) fn reset_create_gate_seams()` — sets `CREATE_HOOK_HITS` to `0` and `CREATE_GATE_SNAPSHOT` to
    `(0, 0, false, false)`.
- Consumes: the existing `CREATE_GATE_SNAPSHOT` tuple `(create_recipients_len, checkpoints_len,
  capture_body_ran, derivation_ran)`; `RcsInspector::new(budget)`; `OpEvmFactory::<OpTx>::default()
  .create_evm_with_inspector(db, EvmEnv::new(CfgEnv::new_with_spec(OpSpecId::JOVIAN), BlockEnv { gas_limit:
  30_000_000, .. }), inspector)`; `call_tx(target)`, `create_tx_with_value(deployer, value)`,
  `call_value_1_to(dst)`, `account(balance, code)`, `caller()`.

- [ ] **Step 1: add the seam (counter + reset) and wire the counter**

Increment `CREATE_HOOK_HITS` as the **first** statement of `Inspector::create` (`rcs_capture.rs:469`), before
`self.push_checkpoint()` and before the `if !self.active` early return, all under `#[cfg(test)]`. Add
`reset_create_gate_seams()` next to the thread-locals. Update the seam's doc comment to state the three
distinguishable states: hits `== 0` ⇒ hook never ran; hits `> 0` with `capture_body_ran == false` ⇒ hit while
inactive; hits `> 0` with `capture_body_ran == true` and `derivation_ran` either value ⇒ armed, derivation ran or
not.

- [ ] **Step 2 (RED): write the non-CREATE zero-hit control**

```rust
/// r11-DERIVE-CTRL. A real transaction with NO `CREATE` at any depth (pure `CALL` only) must leave
/// the create-hook hit counter at zero, which is what makes `derivation_ran == false` in the
/// inactive-CREATE test (`derivation_ran_false_when_create_hook_not_hit`) a statement about a hook
/// that DID run rather than one that never fired.
#[test]
fn non_create_control_leaves_create_hook_hits_zero() {
    // reset_create_gate_seams(); armed inspector; a funded contract whose code is
    // call_value_1_to(dst) → one internal native CALL transfer, no CREATE anywhere.
    // assert_eq!(CREATE_HOOK_HITS.with(|c| c.get()), 0);
    // assert!(matches!(outcome, CaptureOutcome::Complete(s) if s.len() == 1));  // the tx really ran
}
```

Run: `cargo nextest run -p xlayer-builder non_create_control_leaves_create_hook_hits_zero`
Expected: FAIL — `CREATE_HOOK_HITS` / `reset_create_gate_seams` unresolved before Step 1, or the assertion fails
if the counter is wired after the active gate.

- [ ] **Step 3 (Review Focus — seam bleed): inactive CREATE hit > 0, and every sentinel test resets first**

```rust
/// r11-DERIVE-CTRL, the other side: an UNARMED inspector that really does execute a CREATE records
/// a hook hit while reporting neither a capture body nor a derivation.
#[test]
fn inactive_create_hook_hit_is_nonzero_with_derivation_false() {
    // reset_create_gate_seams(); do NOT call start_capture(); execute create_tx_with_value(..).
    // assert!(CREATE_HOOK_HITS.with(|c| c.get()) > 0);
    // let (_, _, capture_body_ran, derivation_ran) = CREATE_GATE_SNAPSHOT.with(|s| s.get());
    // assert!(!capture_body_ran); assert!(!derivation_ran);
}
```

Also add `reset_create_gate_seams();` as the first statement of the four existing sentinel tests —
`inactive_create_no_mutation_and_no_derivation_at_create_return`,
`derivation_ran_true_when_journal_read_for_address_derivation`,
`derivation_ran_false_when_create_hook_not_hit`,
`derivation_ran_false_but_capture_body_ran_for_create2` — so none of them can read a tuple left by an earlier
test on the same thread (the design's "per-test-case reset required"). Do not change their existing assertions.

Run: `cargo nextest run -p xlayer-builder inactive_create_hook_hit_is_nonzero_with_derivation_false`
Expected: first FAIL, then PASS.

- [ ] **Step 4 (Review Focus — CREATE2): assert the hit counter in the CREATE2 sentinel too**

Extend `derivation_ran_false_but_capture_body_ran_for_create2` with
`assert!(CREATE_HOOK_HITS.with(|c| c.get()) > 0)` alongside its existing `capture_body_ran == true` /
`derivation_ran == false` assertions, so the legitimate CREATE2 `false` can never be read as "the hook never
ran".

Run: `cargo nextest run -p xlayer-builder derivation_ran`
Expected: all four sentinel tests PASS.

- [ ] **Step 5: prove the counter is test-only**

```bash
cargo build -p xlayer-builder                       # release-path compile, cfg(test) off
grep -c 'cfg(test)' crates/builder/src/flashblocks/rcs_capture.rs   # the new seam is inside cfg(test)
cargo clippy --all-targets --workspace -- -D warnings
```

Expected: build succeeds with no `unused` diagnostics for the seam; clippy 0/0.

- [ ] **Step 6: full package run + fmt**

```bash
cargo +nightly fmt -p xlayer-builder
cargo nextest run -p xlayer-builder -p rcs-filter
```

Expected: fmt clean; nextest exit 0.

- [ ] **Step 7: commit**

```bash
git add crates/builder/src/flashblocks/rcs_capture.rs
git commit -m "test(rcs): create-hook hit counter and per-test seam reset distinguish not-hit from no-derivation (r11-DERIVE-CTRL)"
```

---

### Task 3 — r11-NT2: reproduce the original NT2 on the pre-fix baseline, or record an honest non-reproduction

The deliverable is an **experimental result**, not an argument. `714e1293` is the genuine pre-fix revision (its
`create` hook still carries `let depth = context.journal().depth(); if depth > 0 && !value.is_zero()`), so the
experiment is feasible and must actually be attempted.

**Files:**
- Create: `/tmp/nt2-red/` — a throwaway worktree at `714e1293`. **Never** committed, never pushed.
- Modify: none in the branch under review.

**Interfaces:**
- Consumes: `internal_create_endowment_captured_through_composite` (exists at both revisions) as the fixture
  shape — an internal `CREATE` with a non-zero endowment driven through the real composite `OpEvm`.
- Produces: the recorded observation that Task 4 §NT2 publishes.

- [ ] **Step 1: create the pre-fix worktree**

```bash
git worktree add /tmp/nt2-red 714e12934aca97eda1ff344d7d1fdc344baff03f
cd /tmp/nt2-red && grep -n 'depth > 0' crates/builder/src/flashblocks/rcs_capture.rs
```

Expected: the `depth > 0` gates are present (pre-fix confirmed) at the `create` hook and in both
`record_call_observation` / `record_create_observation`.

- [ ] **Step 2: port the fixture unchanged and run it**

Copy the baseline's `original_nt2_internal_create_denied_through_real_screen_tx` fixture (and only the helpers it
needs) into the pre-fix tree's test module without altering the transaction shape, the endowment value, or the
rule. Run it.

Run: `cargo nextest run -p xlayer-builder original_nt2_internal_create_denied_through_real_screen_tx`
Expected: **record the actual result**, whichever it is.

- [ ] **Step 3: record the observation, and the depth the hook actually read**

Whatever Step 2 returns, capture the mechanism directly: print `context.journal().depth()` and
`inputs.value()` at the pre-fix `create` hook for that fixture, and the resulting stream length. Write down:
the baseline SHA, the fixture, the command, the exit code, the observed hook depth, the observed stream, and the
post-fix output of the same fixture at the new head.

- [ ] **Step 4: classify the result — exactly one of these two, no third option**

- **(a) GENUINE REPRODUCTION** — the fixture fails to deny at `714e1293` and denies at the new head. Record the
  pre-fix revision, the fixture input, the actual hook/depth observation, and the difference from the post-fix
  output.
- **(b) HONEST NON-REPRODUCTION** — the fixture already denies at `714e1293` (or cannot be driven there).
  State explicitly that the scenario could not be reproduced, name the exact baseline and fixture attempted,
  and give the actual result observed.

Do **not** write "RED pre-D1, GREEN at `<sha>`" as a fact unless (a) actually happened. Do **not** substitute a
top-level CREATE for the internal CREATE (r4 §13 AC3.6). The Task 4 acceptance table's NT2 row must match the
classification chosen here, word for word.

- [ ] **Step 5: tear the worktree down**

```bash
git worktree remove /tmp/nt2-red --force
git worktree list      # expect only the main worktree
```

- [ ] **Step 6: no commit in this task**

The result is text; it is committed as part of Task 4's evidence document. Verify
`git status --short` shows nothing new from this task.

---

### Task 4 — r11-EVIDENCE-CONTRACT: the per-SHA evidence record, raw and complete

**Files:**
- Create: `docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md`
- Preserve: `docs/superpowers/specs/2026-10-13-rcs-native-transfer-mr77-r10-evidence.md` (history; do not edit).

**Interfaces:**
- Consumes: Task 0 Step 5's baseline log; Tasks 1–2's new tests; Task 3's classified NT2 result.
- Produces: the evidence document Task 5's MR description links, and the ID→artifact→evidence table Stage 5.0
  checks row by row.

- [ ] **Step 1: raw per-test log for BOTH packages at the submission SHA**

```bash
git rev-parse HEAD > /tmp/submission-sha.txt
cargo nextest run -p xlayer-builder -p rcs-filter 2>&1 | tee /tmp/nextest-r11.log
echo "exit=$?"
```

Paste `/tmp/nextest-r11.log` into §3 of the evidence document **verbatim**: every per-test line **with its
duration**, the exact command, and the process exit code. The r10 document's `... (per-test lines)` placeholder
and its "durations elided" note are the precise defect round-4 rejected — no placeholder, no elision, no bare
count. If the log is long, that is acceptable; truncation is not.

- [ ] **Step 2: default production feature graph excludes `test-utils`**

```bash
cargo tree -p xlayer-reth-node -e normal,build,features > /tmp/cargo-tree-r11.log
echo "exit=$?"
grep -n 'rcs-filter' /tmp/cargo-tree-r11.log
grep -c 'test-utils' /tmp/cargo-tree-r11.log     # expect 0
```

The package is `xlayer-reth-node` (`bin/node/Cargo.toml:2`) — the reviewer-issued correction carried with the
r11 PASS. Default feature set only: no `--all-features`, dev edges excluded. Paste the command, the exit code,
the `rcs-filter` lines, and the zero `test-utils` count into §4.1.

- [ ] **Step 3: cross-crate test availability with the feature on**

```bash
cargo test -p xlayer-builder 2>&1 | tail -40
echo "exit=$?"
cargo build -p rcs-filter
echo "exit=$?"
```

Record both. `cargo test -p xlayer-builder` (not `cargo check --all-targets`) is what r11 requires as proof the
cross-crate helpers are reachable from the builder's test targets; `cargo build -p rcs-filter` is the production
side of the dual verification.

- [ ] **Step 4: formatting and lint at the same SHA**

```bash
cargo +nightly fmt --all --check ; echo "exit=$?"
cargo clippy --all-targets --workspace -- -D warnings ; echo "exit=$?"
```

Expected and recorded: both exit 0; zero `warning:` and zero `error:` lines in the clippy output. If the
environment blocks clippy (the r8-era disk-full case), say so explicitly with the actual error — never report a
pass that did not happen.

- [ ] **Step 5: test-35 retained vs re-tested**

Compute the covering-blob hash for `loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt` at
the submission SHA and compare it to the r10 record `6685e407887f1d0d0cce1819f7c82de872a9737f6d40b08c5fa395779ecf621b`
(8 953 bytes). Mark M1 / M2 / M3 **retained** (blob byte-identical) or **re-tested** (hash changed, re-run
result recorded) — and note that Task 1 adds a sibling loop-level test without touching this one.

- [ ] **Step 6: write §NT2 from Task 3, and the ID→artifact→evidence table**

§NT2 carries Task 3's classification verbatim — (a) genuine reproduction with baseline / fixture / observed
output, or (b) honest non-reproduction with the baseline and fixture attempted and the actual result. The table
has one row per acceptance ID across all 22 (17 r10 + 5 r11), each with: ID, class (TEST / PROD / SPEC / KEEP),
artifact (exact test function name or file path), the asserted property, and the evidence section. Every r10 row
is marked *carried forward, verified at `a0e5d7b7` (Task 0 Step 4)*; the five r11 rows point at Tasks 1–5.

- [ ] **Step 7: commit**

```bash
git add docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
git commit -m "docs(superpowers): r11 per-SHA evidence — raw nextest, default feature tree, NT2 result (r11-EVIDENCE-CONTRACT, r11-NT2)"
```

---

### Task 5 — r11-MR-TABLE: rewrite the MR77 description against the submission SHA

**Files:**
- Modify: the MR77 description on GitLab (no repository file).

**Interfaces:**
- Consumes: Task 4's evidence document; the submission SHA; the verified stale-name list below.
- Produces: the review-request body Stage 5.0 reads.

- [ ] **Step 1: push the branch**

```bash
git log --oneline a0e5d7b7..HEAD          # the round-5 delta commits only
git push origin xl/xlop-1217/0928-1135
git rev-parse HEAD                        # the submission SHA
```

Never `--no-verify`, never `--force`, never push to `main`. If the pre-push security gate blocks, fix the
reported finding and push again.

- [ ] **Step 2: drop every test name that does not exist at the submission SHA**

These 14 names appear in the current MR body and are **absent** from the baseline source (verified by name
search over `rcs_capture.rs`, `context.rs`, `handle.rs`, `integration_tests.rs` at `a0e5d7b7`). Remove them;
re-verify the list at the submission SHA before publishing, because Tasks 1–2 add names:

`nt2_op_evm_to_screen_tx_captures_expected_transfer`,
`top_level_value_tx_to_eoa_captured_as_synthetic_log`,
`zero_value_call_produces_empty_stream`,
`budget_exceeded_returns_limit_exceeded_outcome`,
`real_log_mismatch_returns_invariant_violation`,
`resimulation_on_budget_exceeded_returns_empty_stream`,
`resimulation_on_invariant_violation_returns_empty_stream`,
`multi_internal_calls_capped_at_budget`,
`nonce_overflow_create_revokes_endowment`,
`nonce_overflow_create2_inside_call_revokes_endowment`,
`create_address_mismatch_detected_on_successful_create`,
`create_address_mismatch_revokes_endowment_in_release_build`,
`internal_create2_nonce_overflow_endowment_revoked`,
`top_level_create_zero_value_no_transfer`.

Mechanical check before publishing:

```bash
# For every test name the new MR body lists:
grep -Rc "fn <name>" crates/builder/src crates/rcs-filter/src | awk -F: '{s+=$2} END {print s}'   # must be >= 1
```

- [ ] **Step 3: correct the design label and the links**

Replace "Design revision (MR77 fixes): r4 (2026-10-12) — sha256 `7a788723…`" with:

- Design revision **r11** (2026-10-13)
- Full Lark URL `https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac`
- Canonical body sha256 `49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836`
- In-repo mirror `docs/superpowers/specs/2026-10-12-rcs-native-transfer-mr77-fixes-design.md`
- Evidence `docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md`
- Plan `docs/superpowers/plans/2026-10-14-rcs-native-transfer-mr77-fixes-plan-r11.md`

- [ ] **Step 4: record the pipeline jobs that actually ran**

Read the pipeline for the submission SHA and list the real job names and their results. Do not describe a
product-test job that does not exist — state plainly that the attached raw `nextest` log in the evidence
document is the product-test evidence of record, as §8.E requires.

- [ ] **Step 5: publish the r11 acceptance table**

One row per acceptance ID: ID, class, test function name or artifact path, result, evidence section. The r10
rows are marked carried-forward-and-verified; the five r11 rows point at this round's tasks. Every name in the
table must have passed Step 2's mechanical check.

- [ ] **Step 6: send the review request on the Bus**

Per the flow's Terran Bus contract: `sender = viking-nativetransfer-starport-rcs`,
`recipient = starport-rcs`, `kind = request`, `correlation_id = player-fix-mr77-native-transfer-2items-20261012`,
a frozen `message_id`, the submission SHA, the full MR URL, and the per-item disposition for round-4 items 1–5.
Read the created row back. This is Stage 3.0/5.0's action, not this plan's — listed so the handoff is explicit.
Never use a native question tool for it.

---

## Self-review

**1. Spec coverage.** The five r11 acceptance IDs map to Tasks 1 (r11-NATIVE-LIMIT-DENY), 2 (r11-DERIVE-CTRL),
3 (r11-NT2), 4 (r11-EVIDENCE-CONTRACT) and 5 (r11-MR-TABLE). The 17 r10 IDs are not re-planned because they
were read as present at `a0e5d7b7`; Task 0 Step 4 is the mechanical gate that turns that reading into a
checked precondition, so a regression surfaces before any later task runs. D1–D9 / D7a / D-ISO / D7b need no
task — they are implemented and this plan changes no production behavior.

**2. Step scan.** Every step names a file, a function signature, an assertion, or a command with its expected
output. Test bodies are given as names plus assertions rather than full code, because the fixtures they reuse
(`build_ctx`, `run_capturing_tx`, `call_value_1_to`, `create_tx_with_value`, `golden::RULE_SCENARIO_A`) already
exist and determine the rest.

**3. Type consistency.** `CREATE_HOOK_HITS` and `reset_create_gate_seams()` are defined in Task 2 Step 1 and
used under those exact names in Steps 2–4. `two_native_transfers_then_erc20_transfer_emitter_code()` is defined
in Task 1 Step 1 and used in Steps 2–5. The `CREATE_GATE_SNAPSHOT` tuple keeps its existing 4-field layout —
the hit count is a separate cell, not a fifth field, so no existing sentinel test's destructuring breaks.

**4. Review Focus.** All five entries have owning steps: Task 1 Steps 4, 5 and 6; Task 2 Steps 1/3 and 4.

**5. Proportion.** The plan is a fraction of the r11 design's length and contains no function bodies — the two
production files are not edited, and the new tests are specified by name, fixture and assertion.

---

## Acceptance ID → artifact → evidence (22 IDs)

| ID | Class | Artifact | Status in this plan |
|---|---|---|---|
| r10-ADVANCE-CFG | PROD | `rcs-filter/Cargo.toml` `[features] test-utils`; `handle.rs` cfg gate; `builder/Cargo.toml` `[dev-dependencies]` | carried forward — verified Task 0 Step 4; dual verification re-run in Task 4 Steps 2–3 |
| r10-ADVANCE-TEST | TEST | `real_stream_screen_tx_and_screen_existing_consistent` | carried forward — Task 0 Step 4 |
| r10-T3-LEGACYVAL | TEST | `legacy_value_rule_still_fires_on_real_capture_superset_stream` | carried forward |
| r10-T3-DUALALIAS | TEST | `dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream` | carried forward |
| r10-T3-AUDITMERGE | TEST | `audit_type_action_merge_action_content_no_empty_audit_on_real_stream` | carried forward |
| r10-T3-NT2-EVID | SPEC | evidence §NT2 | superseded in substance by r11-NT2 — Task 3 |
| r10-DMM-LOOP | TEST | `loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt` | carried forward; Task 1 adds the sibling native-budget loop test |
| r10-DMM-ABORT | TEST | `address_mismatch_flag_cleared_by_both_finish_and_abort` | carried forward |
| r10-T1-DERIVE | PROD+TEST | `create` hook computed `derivation_ran`; three sentinel tests | carried forward; Task 2 adds the hit counter that disambiguates its `false` |
| r10-OVF-ASSERT | TEST | `create_nonce_overflow_is_ok_true_address_none_revoked`, `create2_...` | carried forward |
| r10-T2-ORIGIN | TEST | `eip7702_origin_itself_delegated_runs_inner_call_and_create` | carried forward |
| r10-REGRESS-KEEP | KEEP | the six named regressions | carried forward — Task 0 Step 4 |
| r10-EV-LOG | SPEC | evidence §3 | Task 4 Step 1 |
| r10-EV-TABLE | SPEC | MR test table | Task 5 Steps 2 and 5 |
| r10-EV-CI | SPEC | evidence §4 | Task 4 Step 4; real job list in Task 5 Step 4 |
| r10-EV-T35 | SPEC/KEEP | evidence §5 | Task 4 Step 5 |
| r10-EV-SUPERPOWERS | KEEP | `docs/superpowers/`, `.superpowers/`, tarballs, Terran ledgers | Task 0 Steps 1/3/6 |
| **r11-NATIVE-LIMIT-DENY** | TEST | `loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt` (+ at-limit control, + `loop_level_native_limit_deny_does_not_poison_the_next_candidate`) | **Task 1** |
| **r11-DERIVE-CTRL** | TEST | `non_create_control_leaves_create_hook_hits_zero`, `inactive_create_hook_hit_is_nonzero_with_derivation_false`, `CREATE_HOOK_HITS` seam + per-test reset | **Task 2** |
| **r11-EVIDENCE-CONTRACT** | SPEC | raw `nextest` both packages; `cargo tree -p xlayer-reth-node -e normal,build,features`; `cargo test -p xlayer-builder` | **Task 4** |
| **r11-NT2** | SPEC | evidence §NT2, from the `714e1293` experiment | **Task 3** |
| **r11-MR-TABLE** | SPEC | MR77 description: exact names, r11 label, full URL + sha256, real pipeline jobs | **Task 5** |

## Commit boundaries

| # | Commit | Contents |
|---|---|---|
| 1 | `docs(superpowers): carry the r11 design mirror and the round-5 delta plan onto the MR77 head` | Task 0 Step 6 |
| 2 | `test(rcs): native budget LimitExceeded denies through the real candidate loop (r11-NATIVE-LIMIT-DENY)` | Task 1 Step 8 |
| 3 | `test(rcs): create-hook hit counter and per-test seam reset distinguish not-hit from no-derivation (r11-DERIVE-CTRL)` | Task 2 Step 7 |
| 4 | `docs(superpowers): r11 per-SHA evidence — raw nextest, default feature tree, NT2 result (r11-EVIDENCE-CONTRACT, r11-NT2)` | Task 4 Step 7 |

Four commits, in task order, each independently reviewable. Task 3 produces no commit (its output is text that
commit 4 carries); Task 5 edits MR metadata, not the repository. Push once, after commit 4 (Task 5 Step 1).
