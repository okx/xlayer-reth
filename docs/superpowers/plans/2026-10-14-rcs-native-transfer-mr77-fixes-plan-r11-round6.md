# MR77 RCS Filter — r11 Implementation Plan (round-6 delta) — Evidence Binding, Honest NT2 Labelling, MR Body

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **CURRENT EXECUTABLE PLAN (artifact A1-02).** Supersedes
> `2026-10-14-rcs-native-transfer-mr77-fixes-plan-r11.md` (round-5 delta, baseline `a0e5d7b7` — its
> Tasks 1–5 are now **implemented at `ae490d7f`**, see *Carried forward*), and through it
> `2026-10-13-…-plan-r11.md`, `2026-10-13-…-plan-r10.md`, `2026-10-12-…-plan.md`. **All four prior plan
> files are retained — do not delete them** (r10-EV-SUPERPOWERS).
>
> **Why this revision exists:** round-5 shipped the code and tests, and the building's round-6 dispatch
> found that what remains is **not** a code gap. Five of the six open items are *evidence and metadata*
> defects, and the sixth is an explicit instruction **not** to touch two code groups. A plan that
> re-planned the round-5 tests would therefore re-do verified-good work and violate item 5. This plan is
> the narrow delta that closes `r11-EVIDENCE-CONTRACT`, `r11-NT2` and `r11-MR-TABLE`.

**Goal:** Re-submit MR77 at a new head whose evidence document is SHA-bound, raw-complete and honestly
scoped, and whose MR description is revision-independent — closing all six round-6 items without
changing production or test behavior.

**Architecture:** The approved mechanism (D1–D9 / D7a / D-ISO / D7b) and every acceptance test are
**already implemented and passing** at the baseline and must not be reopened. This round edits exactly
one in-repo document (`specs/2026-10-14-…-r11-evidence.md`), adds one raw-output artifact file beside
it, re-runs the per-SHA verification at the final head, and rewrites the MR description body. **No
production code, no test code, no `Cargo.toml` change.**

**Tech Stack:** Rust; revm pinned (revm-handler 20.0.3 / revm-inspector 21.0.3 / revm-interpreter
37.0.3); op-revm (`OpSpecId::JOVIAN` ⇒ `PRAGUE` ⇒ EIP-7702 active); `alloy_op_evm::OpEvm` composite
inspector; `cargo` / `cargo nextest` / `just`. Repo `github/xlayer-reth`, branch
`xl/xlop-1217/0928-1135`, reused MR <https://gitlab.okg.com/github/xlayer-reth/-/merge_requests/77>.
Jira XLOP-1228 (Task) under Epic XLOP-1217. Workspace packages: `xlayer-builder`, `rcs-filter`,
`xlayer-reth-node`.

**Spec:** `docs/superpowers/specs/2026-10-12-rcs-native-transfer-mr77-fixes-design.md` (in-repo mirror;
canonical publication <https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac>, revision
**r11**, canonical body sha256
`49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836`). Design PASS
`starport-rcs-design-review-r11-pass-20261013` (Bus `recvxVksp5gum4`), `approval_mode=delegated_agent`,
reviewer = building `starport-rcs`. The PASS is **design only**: it authorizes no merge, deploy,
release, MR-approve or scope expansion, and the MR implementation itself is not yet approved
(`detailed_merge_status: not_approved`, verified live 2026-10-14).

**Round-6 requirement source:** the building's dispatch
`starport-rcs-viking-focus-reczz28NDDzzYzst-r1` (Bus record `reczz28NDLogM6qj`, correlation
`reczz28NCcA0SK1N`) and status report `reczz28NDBIBLhVy`, as recorded in
`docs/superpowers/lark-review-doc.md` → *Scoped downstream requirements*. These close existing r11
acceptance IDs; they are **not** new design.

---

## Global Constraints

Every task's requirements implicitly include this section.

- **Plan baseline — `ae490d7ffebce921a8e957618b56672d192739f9`** (current MR77 head). Verified twice
  this round: GitLab `get_merge_request` → `diff_refs.head_sha`, and
  `git ls-remote origin refs/heads/xl/xlop-1217/0928-1135`. **NOT** `ae04e4b3`, **NOT** `a0e5d7b7`,
  **NOT** `21a311e9`, **NOT** `9867fd48`, **NOT** `714e1293`.
- **Baseline ≠ this workspace.** The checked-out worktree is at `714e1293` (the *pre-fix* R12 baseline:
  `depth > 0` gates still live at `rcs_capture.rs:215` and `:232`). **Task 0 is mandatory** —
  implementing on top of `714e1293` would discard the entire r3–r11 implementation series.
- **Node package is `xlayer-reth-node`** (`bin/node/Cargo.toml`), per the reviewer-issued execution
  correction carried with the r11 PASS. The feature-graph command is
  `cargo tree -p xlayer-reth-node -e normal,build,features` — default feature set, **no**
  `--all-features`, dev edges excluded. `xlayer-node` does not exist; do not use it.
- **No production change and no test change this round.** The only tracked files this plan may modify or
  create live under `docs/superpowers/`. If a task appears to need a code edit, it is out of scope —
  stop and report instead (see Task 5).
- **Item-5 protected groups (binding):** do **not** modify the native `LimitExceeded` group or the
  CREATE-inactive-hit group. The building spot-checked `context.rs` (`fail_closed_screen`,
  `execute_best_transactions`) and `rcs_capture.rs` at `ae490d7f` and confirmed **no new code defect**
  there. A change is permitted only if the task names a concrete trigger scenario **and** a test that
  fails before / passes after.
- **Scope:** reuse MR77 and branch `xl/xlop-1217/0928-1135`. No merge, deploy, release, force-push, push
  to `main`, GitLab-approve, second MR, or new Jira issue. Preserve others' edits and the
  `deps/optimism` submodule pointer `0dca4b893f3d85dfa02209af9ff031822c4e3043` — never touch it.
- **No cleanup:** `docs/superpowers/**`, `.superpowers/`, `superpowers-context.tar.gz` and
  `superpowers-context-r4era-20261013.tar.gz` are preserved in full (r10-EV-SUPERPOWERS). Deleting or
  truncating any of them is a FAIL.
- **FAIL rule (binding, design §8.r6):** acceptance = every TEST present **and** passing ∧ every PROD in
  the diff ∧ every SPEC stated ∧ every KEEP retained. Missing, renamed-away, `#[ignore]`d or weakened
  **any one** item of **any** class = FAIL for the whole re-submission. A subset is a FAIL.
- **Budgets:** `max_native_transfers_per_tx` and `max_event_bindings_per_tx` both default `10_000`, both
  `>= 1` (`0` is a startup error even when the Filter is disabled). `observed_count` is sticky.
- **Honest-evidence rule (binding, r11-NT2 / r11-EVIDENCE-CONTRACT):** every number, log and tree in the
  evidence document is either the real captured output or an explicit statement that it was not
  obtained, naming the command, the exit code and what was observed. Narration about an output never
  substitutes for the output. A tool or environment failure is recorded as a failure — never softened,
  never silently carried over from a prior round.
- **Formatting / lint:** `cargo +nightly fmt --all --check` exit 0 and
  `cargo clippy --all-targets --workspace -- -D warnings` 0 warnings / 0 errors, each recorded with its
  exact command and exit code. Never format the submodule.

## Carried forward — verified present at `ae490d7f`, do NOT re-implement

Read out of the fetched baseline objects this round (`git grep`/`git show` at `ae490d7f`). Task 0
re-asserts each one mechanically.

| Item | Where, at `ae490d7f` |
|---|---|
| **r11-NATIVE-LIMIT-DENY** | `context.rs::loop_level_fail_closed_tests::loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt` (:1964) + `loop_level_native_limit_deny_does_not_poison_the_next_candidate` (:2086) |
| **r11-DERIVE-CTRL** | `rcs_capture.rs::non_create_control_leaves_create_hook_hits_zero` (:3502), `inactive_create_hook_hit_is_nonzero_with_derivation_false` (:3528), per-test seam reset helper (:206) covering `CREATE_GATE_SNAPSHOT` (:163) |
| r10-ADVANCE-CFG | `rcs-filter/Cargo.toml` `[features] default = []` + `test-utils`; `handle.rs` `#[cfg(any(test, feature = "test-utils"))]`; `builder/Cargo.toml` `[dev-dependencies]` `features = ["test-utils"]` |
| r10-ADVANCE-TEST | `real_stream_screen_tx_and_screen_existing_consistent` |
| r10-T3-LEGACYVAL / DUALALIAS / AUDITMERGE | `legacy_value_rule_still_fires_on_real_capture_superset_stream`, `dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream`, `audit_type_action_merge_action_content_no_empty_audit_on_real_stream` |
| r10-DMM-LOOP / DMM-ABORT | `context.rs::loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt` (:1729); `rcs_capture.rs::address_mismatch_flag_cleared_by_both_finish_and_abort` |
| r10-T1-DERIVE | `create` hook computes `derivation_ran`; three sentinel tests |
| r10-OVF-ASSERT | `create_nonce_overflow_is_ok_true_address_none_revoked`, `create2_nonce_overflow_is_ok_true_address_none_revoked` |
| r10-T2-ORIGIN | `eip7702_origin_itself_delegated_runs_inner_call_and_create` |
| r10-REGRESS-KEEP | all six by exact name: `top_level_call_with_value_emits_transfer`, `root_success_child_revert_keeps_root_drops_child`, `whole_tx_root_failure_drops_all`, `create_early_failure_no_address_or_event_leak`, `next_capture_starts_clean`, `create_is_ok_early_return_no_endowment_leak` |
| r5-REN-COLLISION | `internal_create2_eip684_collision_endowment_revoked` |
| r5-T3-KEEP | `rcs-filter/src/integration_tests.rs` `t3a`–`t3f` |
| test-35 | `context.rs::loop_level_fail_closed_tests::loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt` (:1518), M1/M2/M3 mutation-resistant |

## The six round-6 items → owner and task

| # | Item (from `lark-review-doc.md`) | Owner | Task here |
|---|---|---|---|
| 1 | Rewrite the **MR77 description body** in revision-independent wording; reconcile notes `11222724` / `11223169` | **4.0** | Task 7 (prepared body + contingency) |
| 2 | Resolve the **evidence-doc SHA binding** (`ae04e4b3` header vs `ae490d7f` claim) | **3.0** | Task 1 |
| 3 | Attach the **real `cargo tree`** output to §4.1 | **3.0** | Task 2 |
| 4 | Relabel **NT2 §7** as a partial, mixed-worktree non-reproduction | **3.0** | Task 3 |
| 5 | **Do NOT redo** the native `LimitExceeded` / CREATE-inactive-hit groups without a named trigger | **3.0** | Task 5 (gate) |
| 6 | **§4.2 OOM** — reconfirm independently, or state plainly it was not reconfirmed | **3.0** | Task 4 |

## Verified gaps at `ae490d7f` — this plan's whole scope

Each read in full out of the baseline document this round.

| Item | Verified gap |
|---|---|
| 2 | Evidence header: *“This evidence was captured at commit `ae04e4b3f4759583425ddd84b4b7732f9bf1fb3e`”*, while the MR note claims `ae490d7f`. **Resolved cheaply:** `git diff --name-only ae04e4b3..ae490d7f` is exactly one path, `docs/superpowers/specs/2026-10-14-…-r11-evidence.md` (one commit, `ae490d7f`, `docs(superpowers): r11 per-SHA evidence …`) — docs-only. The doc does not yet *carry* that proof, and committing this round's own edits moves the head again, so the fix must be a **rule**, not a single SHA. |
| 3 | §4.1 contains the command, `echo $?` → `0`, and prose (*“`rcs-filter` appears exactly twice … lines 12007-12008 and 12495 of the raw output”*) — but **no graph output**. The referenced raw output is not in the repository. |
| 4 | §7 *does* describe the two-file swap (`rcs_capture.rs` + `context.rs` substituted from `git show 714e1293:<path>`) and classifies the result as honest non-reproduction — but it never states that this is a **mixed worktree**, i.e. that the remaining ~3.7k lines were post-fix, so the run is **not** full old-checkout coverage. |
| 6 | §4.2 records the two `cargo test -p xlayer-builder` OOM kills (cgroup `memory.max` 32 GiB, `oom-kill:constraint=CONSTRAINT_MEMCG`) and asserts they are *“an environment/resource constraint of this sandbox, not a defect in the code under test”* — a self-report, with no independent reconfirmation and no statement that reconfirmation was skipped. |
| 1 | MR77 description (read live this round) still says *“Design revision (MR77 fixes): r4 … `7a788723`”*, *“AC6 — 404/404 total tests pass”*, head `21a311e9`, and carries the per-test name tables whose **14** names prior review rounds confirmed absent from the source. |

## File Structure

| File | Responsibility, this plan |
|---|---|
| `docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md` | **Modified** — the only document edited. Header gains the SHA-binding rule (Task 1); §4.1 gains the real tree evidence (Task 2); §7 gains the mixed-worktree boundary (Task 3); §4.2 gains the OOM disposition (Task 4); §3/§5 are re-captured at the final head (Task 6). |
| `docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt` | **New** — the verbatim `cargo tree -p xlayer-reth-node -e normal,build,features` stdout, committed so §4.1's claim is independently checkable (Task 2). |
| `docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-nextest.txt` | **New** — the verbatim `cargo nextest run -p xlayer-builder -p rcs-filter` stdout at the final head (Task 6). |
| `docs/superpowers/plans/2026-10-14-…-plan-r11-round6.md` (this file) | **New** — committed alongside; all four prior plans retained untouched. |
| MR77 description (GitLab, not a file) | Rewritten in Task 7 — **Stage 4.0's** effect. |
| everything under `crates/**`, `bin/**`, `Cargo.toml`, `Cargo.lock` | **Unchanged.** Task 5 proves it mechanically. |

## Review Focus

Five conditions this round's requirements imply that nothing at the baseline pins, most likely to bite
first. Each is pinned to the task that owns it.

- **The evidence SHA is self-referential, so a one-off fix cannot hold.** Any SHA the document names is
  invalidated by the commit that writes that SHA. Item 2 recurs every round unless the doc states a
  *rule* (capture SHA + a docs-only chain to the final head) and the final head is filled in by the
  push that creates it. → pinned in **Task 1, Steps 2 and 4**, and re-asserted in **Task 6, Step 5**.
- **A grepped excerpt of `cargo tree` reads as narration and gets rejected again.** The previous round
  already lost this item by describing the output instead of attaching it; an excerpt chosen by the
  author is the same failure in a thinner disguise. The full stdout must be committed, with the
  in-document claim reduced to commands a reviewer can re-run against the committed file. → pinned in
  **Task 2, Steps 3 and 4**.
- **`cargo tree` or `nextest` may fail in this sandbox** (this environment has already produced a
  disk-full clippy block and two OOM kills). A task that only has a success path will either fabricate
  an output or silently skip the item. Each capture step therefore needs an explicit failure branch that
  records the command, exit code and observed error, and marks the acceptance item **not closed**. →
  pinned in **Task 2, Step 5**, **Task 4, Step 3**, and **Task 6, Step 4**.
- **"Not reconfirmed" must be a written outcome, not an omission.** Item 6 accepts either reconfirmation
  or a plain statement of non-reconfirmation; silence passes neither, and silence is the default
  behavior of a round that runs out of budget. → pinned in **Task 4, Step 4**.
- **The no-code-change claim needs mechanical proof, not an assurance.** Item 5 forbids touching two
  code groups; an author-written "I did not change code" is exactly the class of claim the building has
  twice rejected. → pinned in **Task 5, Steps 1 and 2**.

---

### Task 0 — Re-base onto `ae490d7f` and re-assert the carried-forward state

No code change. Produces the worktree every later task edits, plus mechanical proof that the r10/r11
acceptance items already hold — so no later task re-implements verified-good work (item 5).

**Files:**
- Modify: none. Checkout only.
- Preserve: untracked `docs/superpowers/**`, `.superpowers/`, both `superpowers-context*.tar.gz`.

**Interfaces:**
- Consumes: nothing.
- Produces: a worktree at `ae490d7f` on branch `xl/xlop-1217/0928-1135` whose `docs/superpowers/`
  contains the baseline's tracked documents **plus** this workspace's design mirror, canonical body,
  `lark-review-doc.md`, `specs/history/**` and all five plans.

- [ ] **Step 1: copy the workspace's superpowers docs aside**

```bash
cd xlayer-reth
mkdir -p /tmp/sp-carry && cp -a docs/superpowers /tmp/sp-carry/
git status --short   # expect only: ?? .superpowers/  ?? docs/superpowers/  ?? superpowers-context*.tar.gz
```

- [ ] **Step 2: fetch and check out the baseline**

```bash
git fetch --no-tags origin 'refs/heads/xl/xlop-1217/0928-1135:refs/remotes/origin/xl/xlop-1217/0928-1135'
git checkout -B xl/xlop-1217/0928-1135 ae490d7ffebce921a8e957618b56672d192739f9
git rev-parse HEAD                    # expect ae490d7ffebce921a8e957618b56672d192739f9
git submodule status deps/optimism    # expect 0dca4b893f3d85dfa02209af9ff031822c4e3043, unchanged
```

- [ ] **Step 3: restore the carried docs over the baseline's own**

Copy `/tmp/sp-carry/superpowers/*` back into `docs/superpowers/` **without deleting** any file the
baseline already tracks there. Afterwards `docs/superpowers/` holds, at minimum: the r11 design mirror,
the r11 canonical body, `specs/history/**`, the r10 and r11 evidence specs, `lark-review-doc.md`, and
all five plan files (`2026-10-12`, `2026-10-13`-r10, `2026-10-13`-r11, `2026-10-14`-r11, and this
round-6 plan).

- [ ] **Step 4: re-assert the carried-forward state — fail loudly on any gap**

```bash
grep -c 'cfg(any(test, feature = "test-utils"))' crates/rcs-filter/src/handle.rs   # expect >= 2
grep -c 'features = \["test-utils"\]' crates/builder/Cargo.toml                    # expect 1
for n in loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt \
         loop_level_native_limit_deny_does_not_poison_the_next_candidate \
         non_create_control_leaves_create_hook_hits_zero \
         inactive_create_hook_hit_is_nonzero_with_derivation_false \
         loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt \
         loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt \
         address_mismatch_flag_cleared_by_both_finish_and_abort \
         real_stream_screen_tx_and_screen_existing_consistent \
         legacy_value_rule_still_fires_on_real_capture_superset_stream \
         dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream \
         audit_type_action_merge_action_content_no_empty_audit_on_real_stream \
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
  c=$(grep -Rc "fn $n" crates/builder/src/flashblocks crates/rcs-filter/src | awk -F: '{s+=$2} END {print s+0}')
  [ "$c" -ge 1 ] || echo "MISSING: $n"
done
```

Expected: counts as annotated and **no** `MISSING:` line. A `MISSING:` line means the checkout is wrong
— stop, do not "restore" the test by writing it.

- [ ] **Step 5: baseline green run — the "before" record**

Run: `cargo nextest run -p xlayer-builder -p rcs-filter 2>&1 | tee /tmp/nextest-ae490d7f.log`
Expected: exit 0, all tests pass. This log is Task 5's comparison point and the proof that this round
adds no test delta. If the run does not reach exit 0, record the exact failure and stop — a red baseline
is a finding to report, not something to work around.

- [ ] **Step 6: commit the carried docs**

```bash
git add docs/superpowers
git commit -m "docs(superpowers): carry the r11 design mirror and the round-6 delta plan onto the MR77 head"
```

---

### Task 1 — Item 2: bind the evidence document to its SHAs by a rule

**Files:**
- Modify: `docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md` — the header block
  (currently the paragraph beginning *“This evidence was captured at commit `ae04e4b3…`”*) and §1's
  contract table.

**Interfaces:**
- Consumes: the Task 0 worktree at `ae490d7f`.
- Produces: a header stating three things later tasks fill in — `CAPTURE_SHA`, `FINAL_HEAD`, and the
  docs-only chain between them; and a new §1 row `EV-SHA` that Task 6 Step 5 closes.

- [ ] **Step 1: capture the chain proof for the inherited range**

```bash
git log --oneline ae04e4b3f4759583425ddd84b4b7732f9bf1fb3e..ae490d7ffebce921a8e957618b56672d192739f9
git diff --name-only ae04e4b3f4759583425ddd84b4b7732f9bf1fb3e..ae490d7ffebce921a8e957618b56672d192739f9
```

Expected (verified while writing this plan — if it differs, the baseline moved; re-derive, do not
transcribe):

```
ae490d7f docs(superpowers): r11 per-SHA evidence — raw nextest, default feature tree, NT2 result
docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
```

- [ ] **Step 2: rewrite the header as a rule, not a single SHA**

Replace the “captured at commit `ae04e4b3…`” paragraph with a block that states, in this order:

1. **Test-capture SHA** — the commit whose tree the §3/§5 runs were executed against.
2. **Final submission SHA** — left as the literal placeholder `<FINAL_HEAD>` for Task 6 Step 5.
3. **The binding rule, verbatim:** *“Every commit between the test-capture SHA and the final submission
   SHA touches only `docs/**`. The proof is the `git log --oneline` and `git diff --name-only` output
   reproduced below; a reviewer can re-run both commands and must see only `docs/**` paths. If any
   non-`docs/**` path ever appears in that range, this document's test evidence is void and must be
   re-captured at the new head.”*
4. The two command outputs from Step 1, pasted verbatim under a fenced block, each preceded by its
   command line and followed by `$ echo $?` and the real exit code.

Carry the inherited `a0e5d7b7..ae04e4b3` provenance sentence forward unchanged — it is history, not a
claim about this round.

- [ ] **Step 3: add the `EV-SHA` contract row to §1**

Append to the §1 table: `| EV-SHA | Test-capture SHA and final submission SHA both named, with a
re-runnable docs-only chain between them | header |`.

- [ ] **Step 4: verify the document no longer asserts a bare capture SHA**

```bash
grep -n 'ae04e4b3\|ae490d7f\|<FINAL_HEAD>' docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
```

Expected: every `ae04e4b3` / `ae490d7f` occurrence sits inside the header's rule block, the chain
output, or the inherited provenance sentence — and exactly one `<FINAL_HEAD>` placeholder remains for
Task 6.

- [ ] **Step 5: commit**

```bash
git add docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
git commit -m "docs(rcs): bind the r11 evidence document to its capture and submission SHAs"
```

---

### Task 2 — Item 3: attach the real default-feature graph

**Files:**
- Create: `docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt`
- Modify: the same evidence document, §4.1.

**Interfaces:**
- Consumes: the Task 0 worktree.
- Produces: a committed verbatim tree output plus a §4.1 claim expressed as re-runnable commands over
  that file.

- [ ] **Step 1: capture the graph verbatim**

```bash
cargo tree -p xlayer-reth-node -e normal,build,features \
  > docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt 2>&1
echo "exit=$?"
wc -l docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt
```

Default feature set — do **not** add `--all-features`, and do **not** add `-e dev`.

- [ ] **Step 2: derive the absence claim mechanically**

```bash
F=docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt
grep -n 'rcs-filter' "$F"
grep -c 'rcs-filter feature "test-utils"' "$F"    # expect 0
```

Expected: every `rcs-filter` line is the crate itself or `rcs-filter feature "default"`, and the
`test-utils` count is `0`. If the count is non-zero, the feature is leaking into the default production
graph — that is a real defect: stop and report it, do not edit the document around it.

- [ ] **Step 3: rewrite §4.1 to point at the committed file**

§4.1 must contain, in this order: the exact command; its real exit code; the committed file's path and
line count; the two Step 2 commands with their real outputs; and the one-sentence conclusion that
`rcs-filter`'s `test-utils` feature is absent from the default production dependency graph. Keep the
existing clarification that unrelated crates (`reth-node-builder`, `reth-primitives-traits`, `reth-db`,
…) have their own `test-utils` features. **Delete** the current prose that cites line numbers of an
output not present in the repository.

- [ ] **Step 4: verify a reviewer can re-derive the claim without the author**

```bash
grep -n 'feature-tree.txt' docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
grep -n 'lines 12007' docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md  # expect no match
```

Expected: §4.1 references the committed file; the stale line-number narration is gone.

- [ ] **Step 5: failure branch — if `cargo tree` cannot run**

If Step 1's exit code is non-zero (resolver failure, disk exhaustion, network block), write into §4.1:
the exact command, the real exit code, the verbatim first and last 20 lines of the error, and the
sentence *“EV-TREE is NOT closed this round: the default-feature graph could not be captured in this
environment.”* Commit the partial output file as captured. Do **not** reuse a prior round's narration
and do **not** mark the item closed.

- [ ] **Step 6: commit**

```bash
git add docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt \
        docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
git commit -m "docs(rcs): attach the real default-feature dependency graph for xlayer-reth-node"
```

---

### Task 3 — Item 4: label NT2 §7 as a partial, mixed-worktree non-reproduction

**Files:**
- Modify: the same evidence document, §7.

**Interfaces:**
- Consumes: §7's existing baseline / fixture / method / command / result record (keep all of it).
- Produces: §7 with an explicit scope-boundary paragraph; the `r11-NT2` row in §2 reworded to match.

- [ ] **Step 1: state the boundary in the document's own words**

Add to §7, immediately after the existing **Method** paragraph, a paragraph that says all four of:

1. Only two files (`crates/builder/src/flashblocks/rcs_capture.rs` and `context.rs`) were substituted
   from `714e1293`; every other file in the worktree remained at the post-fix revision.
2. Therefore the run executed a **mixed worktree**, not a clean `714e1293` checkout.
3. Therefore the result is a **partial non-reproduction**: it shows this fixture does not reproduce the
   defect under that specific two-file substitution, and it is explicitly **not** full old-checkout
   coverage of the pre-fix revision.
4. The reason a clean old checkout was not used, stated as fact: current `context.rs` matches on
   `CaptureInvariantError::CreateAddressMismatch`, a variant absent at `714e1293`, so the two revisions
   do not compile together — a full old checkout would have been a different experiment, which this
   round did not run.

- [ ] **Step 2: remove any wording that implies full old-checkout validation**

```bash
grep -n 'genuine pre-D1 revision\|pre-fix revision\|714e1293' docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
```

Every surviving occurrence must be qualified by the mixed-worktree boundary. Keep the classification
**(b) HONEST NON-REPRODUCTION**; keep the fixture name
`internal_create_endowment_captured_through_composite`, the command, and the `exit 0 / PASS` result
unchanged. Do **not** upgrade the classification, and do **not** delete the attempt.

- [ ] **Step 3: align the §2 acceptance row**

Reword the `r11-NT2` row's Result cell to: *“(b) honest non-reproduction — **partial, mixed-worktree**
(two files substituted from `714e1293`); not full old-checkout coverage (§7)”*.

- [ ] **Step 4: verify the boundary is stated, not implied**

```bash
grep -n -i 'mixed worktree\|mixed-worktree' docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
```

Expected: at least one hit in §7 and one in the §2 `r11-NT2` row.

- [ ] **Step 5: commit**

```bash
git add docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
git commit -m "docs(rcs): scope the NT2 result as a partial mixed-worktree non-reproduction"
```

---

### Task 4 — Item 6: dispose of the §4.2 OOM self-report explicitly

**Files:**
- Modify: the same evidence document, §4.2.

**Interfaces:**
- Consumes: §4.2's existing environment note.
- Produces: §4.2 carrying exactly one of two explicit dispositions. Silence is a FAIL.

- [ ] **Step 1: decide the branch by what this environment allows**

Branch **A (reconfirm)** if a bounded re-run is affordable; branch **B (state plainly)** otherwise.
Choose before running anything, and record the choice in the document.

- [ ] **Step 2: branch A — attempt the independent reconfirmation**

```bash
cargo test -p xlayer-builder 2>&1 | tail -40 ; echo "exit=${PIPESTATUS[0]}"
dmesg 2>/dev/null | grep -i 'oom-kill\|Killed process' | tail -5
```

Record verbatim: the command, the real exit code, the tail of the output, and any kernel OOM line
(including its absence). Then state whether the two prior OOMs are reconfirmed as an infrastructure
artifact, or whether the behavior differed this round.

- [ ] **Step 3: branch A failure handling**

If the run is killed again, that **is** the reconfirmation — record the kernel evidence and say so. If
it cannot be started at all (no disk, no memory headroom), do not leave the item open: fall through to
branch B and say that branch A was attempted and why it could not run.

- [ ] **Step 4: branch B — state the non-reconfirmation plainly**

Write into §4.2, verbatim: *“The two full-`cargo test -p xlayer-builder` OOM kills recorded above were
**not** independently reconfirmed in this round. The statement that they are an infrastructure artifact
rather than a defect in the code under test is carried forward from the previous round's observation and
is explicitly not re-verified here.”* No softening, no implication that it was checked.

- [ ] **Step 5: verify exactly one disposition is present**

```bash
grep -n -i 'reconfirm' docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
```

Expected: §4.2 contains one of the two statements and not both.

- [ ] **Step 6: commit**

```bash
git add docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
git commit -m "docs(rcs): record the explicit disposition of the cargo test OOM self-report"
```

---

### Task 5 — Item 5: prove mechanically that no protected code changed

**Files:**
- Modify: none. Produces a record appended to the evidence document, §8.

**Interfaces:**
- Consumes: `/tmp/nextest-ae490d7f.log` from Task 0 Step 5 and the commits from Tasks 1–4.
- Produces: an §8 subsection `8.1 Round-6 change surface` carrying the real diff output.

- [ ] **Step 1: capture the round's whole change surface**

```bash
git diff --name-only ae490d7ffebce921a8e957618b56672d192739f9..HEAD
git diff --stat ae490d7ffebce921a8e957618b56672d192739f9..HEAD -- crates bin Cargo.toml Cargo.lock
```

Expected: the first command lists **only** `docs/superpowers/**` paths; the second produces **empty**
output. Non-empty second output means a code change slipped in — revert it or name its trigger
scenario and its before/after test, per the Global Constraints.

- [ ] **Step 2: paste both outputs into §8.1 with the item-5 conclusion**

§8.1 states: the two commands, their verbatim output, and the sentence *“No file under `crates/**`,
`bin/**`, `Cargo.toml` or `Cargo.lock` was modified in round 6. The native `LimitExceeded` group and the
CREATE-inactive-hit group are untouched; no trigger scenario was named, and none was needed.”*

- [ ] **Step 3: confirm the test set did not move**

```bash
git diff --stat ae490d7ffebce921a8e957618b56672d192739f9..HEAD -- '*.rs'   # expect empty
```

Expected: empty. Combined with Task 0 Step 5's green log, this is the proof that this round adds no test
delta and weakens no existing test (test-35 and all KEEP items retained by construction).

- [ ] **Step 4: commit**

```bash
git add docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
git commit -m "docs(rcs): record the round-6 change surface as docs-only"
```

---

### Task 6 — Close `r11-EVIDENCE-CONTRACT` at the final head

**Files:**
- Create: `docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-nextest.txt`
- Modify: the same evidence document, §3, §5, §6 and the header's `<FINAL_HEAD>` placeholder.

**Interfaces:**
- Consumes: Tasks 1–5 committed.
- Produces: the submission-SHA-bound evidence record; the final head SHA that Task 7 cites.

- [ ] **Step 1: re-capture the test log at the current tree, raw and complete**

```bash
cargo nextest run -p xlayer-builder -p rcs-filter 2>&1 \
  | tee docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-nextest.txt
echo "exit=${PIPESTATUS[0]}"
```

Expected: exit 0. The committed file must contain every per-test line with its name and duration — no
elision, no `... (per-test lines)` placeholder, no "durations elided" note. §3 then cites this file and
reproduces its summary line verbatim rather than restating a count.

- [ ] **Step 2: re-capture fmt and lint**

```bash
cargo +nightly fmt --all -- --check ; echo "fmt exit=$?"
cargo clippy --all-targets --workspace -- -D warnings 2>&1 | tail -20 ; echo "clippy exit=${PIPESTATUS[0]}"
```

Record both commands with their real exit codes in §5.

- [ ] **Step 3: re-assert §6 test-35 retention**

§6 must state that test-35 is **retained, not re-authored**, and prove it from this round's change
surface: Task 5 Step 3's empty `*.rs` diff means the covering blob is byte-identical to `ae490d7f`.
State whether M1/M2/M3 were re-run this round or only retained — do not imply re-running if §3's log is
the only evidence.

- [ ] **Step 4: failure branch**

If Step 1 is non-zero, record the failing test names verbatim and stop — a red suite is a finding to
report to the building, not something to document around. If Step 2's clippy cannot run (this
environment has previously hit disk exhaustion), record the exact command, exit code and error, and
state in §5 that `EV-CI`'s clippy half is **not** closed this round. Never record an unrun command as
passing.

- [ ] **Step 5: bind the final head and push**

```bash
git add docs/superpowers/specs
git commit -m "docs(rcs): per-SHA r11 evidence re-captured at the round-6 submission head"
git push origin xl/xlop-1217/0928-1135      # no --force, no -n/--no-verify, never to main
git rev-parse HEAD                           # this is FINAL_HEAD
```

Then replace the header's `<FINAL_HEAD>` placeholder with that SHA, re-run Task 1 Step 1's two commands
over `CAPTURE_SHA..FINAL_HEAD`, paste the real output into the header's chain block (it must show only
`docs/**`), and amend-or-follow-up commit and push once more. Record the resulting head as the
submission SHA.

- [ ] **Step 6: verify the document is self-consistent**

```bash
grep -n '<FINAL_HEAD>\|per-test lines\|durations elided' docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
```

Expected: no match for any of the three.

---

### Task 7 — Item 1: rewrite the MR77 description body (owner: Stage 4.0)

**Files:**
- Modify: MR77's description on GitLab — not a repository file.

**Interfaces:**
- Consumes: `FINAL_HEAD` from Task 6 Step 5; the committed evidence document path; the r11 Lark URL and
  canonical sha256 from this plan's **Spec** block.
- Produces: a revision-independent MR body, plus one note that supersedes both `11222724` and
  `11223169`.

- [ ] **Step 1: compose the new body against these rules**

- No `rN` design-revision label anywhere in the body. Reference the design by **URL + canonical
  sha256** only.
- No fixed total-test count (`404/404`, `413/413`, …). Reference suites by **crate + pass criterion**:
  *“`cargo nextest run -p xlayer-builder -p rcs-filter` — exit 0; the raw per-test log is committed at
  `docs/superpowers/specs/2026-10-14-…-r11-nextest.txt`.”*
- **Delete the per-test name tables entirely.** They are what carried the 14 non-existent names; a
  corrected table would re-create the same maintenance defect.
- List **wording items** and **evidence items** in two separate sections, so a reviewer can tell a
  documentation change from a verification result.
- Do **not** report *“not yet independently re-reviewed”* as a code defect anywhere.
- State the head SHA as `FINAL_HEAD`, and name the actual CI jobs that ran — not an assumed set.

- [ ] **Step 2: attempt the description edit**

Use the GitLab MR-update capability if this session has one. **Known constraint:** the GitLab MCP tool
set available to this flow is note-level only (`create_merge_request`, `create_merge_request_note`,
`update_merge_request_note`, … — there is no `update_merge_request`), which is exactly why note
`11223169` exists. Verify your own tool table before concluding either way.

- [ ] **Step 3: if the description cannot be edited, do not claim the item closed**

Post **one** note that (a) carries the full corrected body from Step 1 verbatim, (b) states explicitly
that it **supersedes both `11222724` and `11223169`**, and (c) records the blocker in one sentence: no
description-editing tool is available in this session's GitLab MCP tool set, so the body itself is
unchanged and item 1 remains **partially closed** — reconciling note delivered, description rewrite
blocked. Report that status to the building on the Bus with the correct `correlation_id` / `reply_to`;
never report it as done.

- [ ] **Step 4: verify against the item's own pass criterion**

```text
get_merge_request(project_id="github/xlayer-reth", merge_request_iid="77")
  → description contains no `rN` design label, no fixed total-test count, none of the removed test names
get_merge_request_notes(...)
  → a note exists that supersedes both 11222724 and 11223169
```

Record which half passed and which did not. A partially met criterion is reported as partial.

---

## Self-review

**1. Spec coverage.** The approved design's decisions D1–D9 / D7a / D-ISO / D7b and every r10/r11
acceptance ID are implemented at the baseline and listed under *Carried forward*; Task 0 Step 4 proves
each mechanically. All six round-6 items map to a task in *The six round-6 items → owner and task*, and
each task ends at that item's own pass criterion. No design clause is left without either an
implementing commit at `ae490d7f` or a task here. Gap found and closed during review: `EV-SHA` had no
contract row — added in Task 1 Step 3.

**2. Step scan.** Every step is one action with a checkable result. The document-editing steps state the
required propositions rather than the prose (the writer's wording is theirs; the content is not), and
every capture step pairs its command with its expected exit code. No step says "handle edge cases" or
"update as appropriate". Removed during review: a step that would have re-run the round-5 test suite for
its own sake — Task 0 Step 5's log already serves Task 5.

**3. Type consistency.** No new types or signatures are introduced this round (no code change), so there
is nothing to drift. The identifiers that must match across tasks are SHAs and file paths:
`CAPTURE_SHA` / `FINAL_HEAD` are introduced in Task 1, filled in Task 6 Step 5 and consumed in Task 7
Step 1; the three committed artifact paths (`-feature-tree.txt`, `-nextest.txt`, the evidence `.md`) are
spelled identically in File Structure, Tasks 2 and 6, and the MR body rules.

**4. Review Focus.** Five conditions listed; each is pinned to a specific step — the self-referential
SHA (Task 1 Steps 2/4 + Task 6 Step 5), author-chosen excerpts (Task 2 Steps 3/4), sandbox tool failure
(Task 2 Step 5, Task 4 Step 3, Task 6 Step 4), unwritten non-reconfirmation (Task 4 Step 4), and the
unproven no-change claim (Task 5 Steps 1/2).

**5. Proportion.** This plan is a fraction of the r11 design's length and shorter than the round-5 plan
it supersedes, which is right: the mechanism is already built, so what remains to decide is which
document says what, bound to which SHA, and who owns the MR body. There are no implementation code
blocks, only commands and their expected results.

---

## Acceptance ID → artifact → evidence (round-6 view)

The 22 r10/r11 IDs are unchanged and already satisfied at `ae490d7f` (see *Carried forward*; §2 of the
evidence document carries the full table). Round 6 closes the three SPEC-class IDs plus one new one:

| Id | Class | Artifact | Closed by |
|---|---|---|---|
| `EV-SHA` (new) | SPEC | evidence header: capture SHA + `FINAL_HEAD` + docs-only chain | Task 1, re-asserted Task 6 Step 5 |
| `r11-EVIDENCE-CONTRACT` | SPEC | `-nextest.txt`, `-feature-tree.txt`, §3 / §4.1 / §4.2 / §5 | Tasks 2, 4, 6 |
| `r11-NT2` | SPEC | §7 + §2 row | Task 3 |
| `r11-MR-TABLE` | SPEC | MR77 description + superseding note | Task 7 (Stage 4.0) |
| item-5 guard | KEEP | §8.1 change surface | Task 5 |

## Commit boundaries

One commit per task deliverable; all docs-only. Pushed once, in Task 6 Step 5, to
`xl/xlop-1217/0928-1135` — no force-push, no `--no-verify`, no push to `main`, no second MR.

| # | Commit message | Task |
|---|---|---|
| 1 | `docs(superpowers): carry the r11 design mirror and the round-6 delta plan onto the MR77 head` | 0 |
| 2 | `docs(rcs): bind the r11 evidence document to its capture and submission SHAs` | 1 |
| 3 | `docs(rcs): attach the real default-feature dependency graph for xlayer-reth-node` | 2 |
| 4 | `docs(rcs): scope the NT2 result as a partial mixed-worktree non-reproduction` | 3 |
| 5 | `docs(rcs): record the explicit disposition of the cargo test OOM self-report` | 4 |
| 6 | `docs(rcs): record the round-6 change surface as docs-only` | 5 |
| 7 | `docs(rcs): per-SHA r11 evidence re-captured at the round-6 submission head` | 6 |

## Stage-2.0 provenance and named limitation

- Design approval used: PASS `starport-rcs-design-review-r11-pass-20261013` (Bus `recvxVksp5gum4`),
  revision **r11**, canonical sha256 `49dce95e…0836`, `approval_mode=delegated_agent`, reviewer
  BUILDING_ID `starport-rcs`. Per the delegated-design policy this is sufficient; no human
  design-signature gate was reopened and no waiting event was required. The design was **not** reopened
  (no r12).
- Baseline verified live while writing this plan: GitLab `get_merge_request` →
  `diff_refs.head_sha = ae490d7f`, and `git ls-remote` agrees. `ae04e4b3..ae490d7f` verified docs-only.
- **Named limitation:** `ok-okengine-lark-mcp` exposed **zero tools** in this planning attempt's tool
  table, so neither the Lark Doc nor the Bus was read or written live. No HTTP/REST fallback was
  attempted, per the runtime's MCP tool-access rule. This did not block planning: the design is approved
  and the six round-6 items are mechanically specified, so no blocking clarification was due and no new
  Bus request was owed by this stage. The building's dispatch `reczz28NDLogM6qj` remains **open for
  Stage 5.0** to poll, consume (`delivery=delivered`, `status=processing`) and reply to.
- No source or test code was modified by the planning stage.
