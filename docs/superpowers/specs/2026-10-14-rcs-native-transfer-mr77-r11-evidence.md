# RCS Filter native-transfer fixes — revision r11 per-SHA evidence

> Traceability document. Design ids, requirement ids and review records belong in `docs/superpowers/`,
> in the stage report and in MR metadata; they are deliberately absent from the production and test
> sources.

Design: Lark revision **r11**, token `UXohdEdeaokIGpxWnJPlaqGugac`, canonical sha256
`49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836`.
Plan: `docs/superpowers/plans/2026-10-14-rcs-native-transfer-mr77-fixes-plan-r11-round6.md` (round-6
delta of `2026-10-14-...-plan-r11.md`).
Branch: `xl/xlop-1217/0928-1135`. Round-6 plan baseline: `ae490d7ffebce921a8e957618b56672d192739f9`.

**Evidence SHA binding (rule, not a single SHA — see `EV-SHA` in §1):**

1. **Test-capture SHA:** `ae04e4b3f4759583425ddd84b4b7732f9bf1fb3e` — the commit whose tree §3's and
   §5's test/lint runs were executed against (the round-5 delta's last test/code commit on top of the
   round-5 baseline `a0e5d7b73a669d50ed18f65d1c40c52a7a70a230`: docs carry-forward, the two new test
   commits, one rustfmt fixup).
2. **Final submission SHA:** `<FINAL_HEAD>` — filled in by this round's Task 6, Step 5.
3. **The binding rule, verbatim:** Every commit between the test-capture SHA and the final submission
   SHA touches only `docs/**`. The proof is the `git log --oneline` and `git diff --name-only` output
   reproduced below; a reviewer can re-run both commands and must see only `docs/**` paths. If any
   non-`docs/**` path ever appears in that range, this document's test evidence is void and must be
   re-captured at the new head.
4. **Chain proof, test-capture SHA → round-5 submission SHA** (inherited, re-verified this round):

   ```
   $ git log --oneline ae04e4b3f4759583425ddd84b4b7732f9bf1fb3e..ae490d7ffebce921a8e957618b56672d192739f9
   ae490d7f docs(superpowers): r11 per-SHA evidence — raw nextest, default feature tree, NT2 result
   $ echo $?
   0

   $ git diff --name-only ae04e4b3f4759583425ddd84b4b7732f9bf1fb3e..ae490d7ffebce921a8e957618b56672d192739f9
   docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
   $ echo $?
   0
   ```

   This inherited range is history, not a claim about this round; it is carried forward unchanged.
5. **Chain proof, test-capture SHA → final submission SHA** (this round's full accumulated range): filled
   in by Task 6, Step 5, once `<FINAL_HEAD>` is known.

The round-5 provenance sentence (`a0e5d7b7..ae04e4b3`: docs carry-forward, the two new test commits, one
rustfmt fixup) is carried forward unchanged above — it is history, not a claim about this round.

## 1. Evidence contract

| Item | Requirement | Section |
|---|---|---|
| EV-LOG | Readable `cargo nextest` log over **both** `xlayer-builder` and `rcs-filter`, exact command + exit code + named per-test results, no placeholder/elision | §3 |
| EV-TREE | `cargo tree -p xlayer-reth-node -e normal,build,features` (default feature set) shows `rcs-filter` WITHOUT `test-utils` | §4.1 |
| EV-CROSSTEST | `cargo test -p xlayer-builder` (test-utils enabled) proves the cross-crate test helper is reachable | §4.2 |
| EV-CI | `cargo +nightly fmt --all --check` exit 0 and `cargo clippy --all-targets --workspace -- -D warnings` 0/0, each with command + exit code | §5 |
| EV-T35 | Test-35 marked retained (covering blob proven byte-identical) vs re-tested | §6 |
| EV-NT2 | r11-NT2 classification: genuine reproduction or honest non-reproduction, with baseline/fixture/result | §7 |
| EV-SHA | Test-capture SHA and final submission SHA both named, with a re-runnable docs-only chain between them | header |

A bare pass count is not evidence; the raw log referenced in §3 is the evidence of record.

## 2. Acceptance id → artifact → evidence (22 ids: 17 carried from r10, 5 new in r11)

| Id | Class | Artifact | Result |
|---|---|---|---|
| r10-ADVANCE-CFG | PROD | `rcs-filter/Cargo.toml` `[features] test-utils`; `handle.rs` cfg gate; `builder/Cargo.toml` `[dev-dependencies]` | carried forward — verified present at `a0e5d7b7` (plan Task 0 Step 4); re-verified §4.1/§4.2 |
| r10-ADVANCE-TEST | TEST | `real_stream_screen_tx_and_screen_existing_consistent` | PASS (§3, §4.2) |
| r10-T3-LEGACYVAL | TEST | `legacy_value_rule_still_fires_on_real_capture_superset_stream` | PASS (§3) |
| r10-T3-DUALALIAS | TEST | `dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream` | PASS (§3) |
| r10-T3-AUDITMERGE | TEST | `audit_type_action_merge_action_content_no_empty_audit_on_real_stream` | PASS (§3) |
| r10-DMM-LOOP | TEST | `loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt` | PASS (§3) |
| r10-DMM-ABORT | TEST | `address_mismatch_flag_cleared_by_both_finish_and_abort` | PASS (§3) |
| r10-T1-DERIVE | PROD+TEST | `create` hook computed `derivation_ran`; `derivation_ran_true_when_journal_read_for_address_derivation`, `derivation_ran_false_when_create_hook_not_hit`, `derivation_ran_false_but_capture_body_ran_for_create2` | PASS (§3) |
| r10-OVF-ASSERT | TEST | `create_nonce_overflow_is_ok_true_address_none_revoked`, `create2_nonce_overflow_is_ok_true_address_none_revoked` | PASS (§3) |
| r10-T2-ORIGIN | TEST | `eip7702_origin_itself_delegated_runs_inner_call_and_create` | PASS (§3) |
| r10-REGRESS-KEEP | KEEP | `top_level_call_with_value_emits_transfer`, `root_success_child_revert_keeps_root_drops_child`, `whole_tx_root_failure_drops_all`, `create_early_failure_no_address_or_event_leak`, `next_capture_starts_clean`, `create_is_ok_early_return_no_endowment_leak` | all six present and PASS (§3) |
| r10-EV-LOG | SPEC | §3 | satisfied |
| r10-EV-TABLE | SPEC | MR description (Task 5) | pending Task 5 |
| r10-EV-CI | SPEC | §5 | satisfied |
| r10-EV-T35 | SPEC/KEEP | §6 | satisfied — retained |
| r10-EV-SUPERPOWERS | KEEP | `docs/superpowers/`, `.superpowers/`, `superpowers-context.tar.gz` | preserved, not deleted (plan Task 0 Steps 1/3/6) |
| **r11-NATIVE-LIMIT-DENY** | TEST | `loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt` (incl. inline at-limit budget=2 control), `loop_level_native_limit_deny_does_not_poison_the_next_candidate` | PASS (§3) |
| **r11-DERIVE-CTRL** | TEST | `CREATE_HOOK_HITS` seam + `reset_create_gate_seams()`; `non_create_control_leaves_create_hook_hits_zero`, `inactive_create_hook_hit_is_nonzero_with_derivation_false`; the four pre-existing sentinel tests gain the reset call | PASS (§3) |
| **r11-EVIDENCE-CONTRACT** | SPEC | §3, §4.1, §4.2 | satisfied |
| **r11-NT2** | SPEC | §7 | (b) honest non-reproduction — **partial, mixed-worktree** (two files substituted from `714e1293`); not full old-checkout coverage (§7) |
| **r11-MR-TABLE** | SPEC | MR description (Task 5) | pending Task 5 |

**Deviation from the plan's literal Task 1 Step 6** (ledgered as a ruling, not a silent drop): the plan
sketches an additional `zero_native_budget_is_a_config_error_even_when_filter_disabled` test. This exact
property — `max_native_transfers_per_tx == 0` is a startup error even when the filter is disabled — is
already asserted by the existing `rcs-filter/src/config.rs::tests::budgets_are_validated_even_when_disabled`
test (native-budget half), confirmed present and passing (§3, line `rcs-filter config::tests::budgets_are_validated_even_when_disabled`). Adding a near-duplicate test under a new name would not
strengthen coverage; this ID is not in the plan's own 22-id acceptance table above, so skipping the
literal step is not a FAIL-rule violation.

## 3. Per-SHA test log — BOTH packages, workspace-wide

Full raw verbatim output (every per-test line, no elision, no placeholder) committed at
`docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-nextest.txt` (420 lines), captured at
this round-6 submission tree (code identical to `ae490d7f`; Task 5 §8.1 proves zero `*.rs` delta).

```
$ cargo nextest run -p xlayer-builder -p rcs-filter 2>&1 \
    | tee docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-nextest.txt
$ echo "exit=${PIPESTATUS[0]}"
exit=0
```

Summary line, reproduced verbatim from the committed file's tail (not restated as a count the author
chose):

```
     Summary [ 244.905s] 413 tests run: 413 passed, 0 skipped
```

A reviewer re-runs the command above and diffs their own output against the committed file; the raw
log is the evidence of record, not this summary line.

## 4. Feature isolation (default production graph does not expose the helpers)

### 4.1 Default production feature graph (`xlayer-reth-node`, no `--all-features`, dev edges excluded)

```
$ cargo tree -p xlayer-reth-node -e normal,build,features > docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt 2>&1
$ echo $?
0
$ wc -l docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt
12503 docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt
```

Full verbatim output committed at
`docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt` (12503 lines) — a
reviewer re-runs the two commands below against that committed file directly, without the author:

```
$ grep -n 'rcs-filter' docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt
12007:│       ├── rcs-filter feature "default"
12008:│       │   └── rcs-filter v0.0.7 (.../crates/rcs-filter)
12495:│       └── rcs-filter feature "default" (*)
$ echo $?
0

$ grep -c 'rcs-filter feature "test-utils"' docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt
0
$ echo $?
0
```

`rcs-filter` appears exactly twice in the committed tree file, both times as `rcs-filter feature
"default"` only — `test-utils` is never listed against `rcs-filter` anywhere in the default production
dependency graph. (Other, unrelated crates — `reth-node-builder`, `reth-primitives-traits`, `reth-db`,
etc. — have their OWN `test-utils` feature in this tree; those are not `rcs-filter`'s.) `rcs-filter`'s
`test-utils` feature is absent from the default production dependency graph of `xlayer-reth-node`.

### 4.2 Cross-crate test availability (test-utils enabled)

```
$ cargo build -p rcs-filter
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 34.80s
$ echo $?
0

$ cargo test -p xlayer-builder --lib real_stream_screen_tx_and_screen_existing_consistent
    Finished `test` profile [unoptimized + debuginfo] target(s) in 36.24s
     Running unittests src/lib.rs (target/debug/deps/xlayer_builder-868a9c6807f8561b)

running 1 test
test flashblocks::rcs_capture::gating_integration_tests::real_stream_screen_tx_and_screen_existing_consistent ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 179 filtered out; finished in 0.13s

$ echo $?
0
```

**Environment note (recorded honestly, not a code defect):** the FULL `cargo test -p xlayer-builder`
suite (the plan's literal Step 3 command) was attempted twice in the round-5 delta — once with the
default thread count and once with `--test-threads=2` — and both runs were terminated by the kernel OOM
killer at this environment's cgroup `memory.max` of 32 GiB (`dmesg`: `oom-kill:constraint=
CONSTRAINT_MEMCG ... task=xlayer_builder-`), after ~30 tests had already passed in-process. `cargo
test`'s default libtest harness runs all tests of one binary in ONE process (unlike `nextest`'s
process-per-test model used in §3, which ran all 413 tests, including every test in this binary,
successfully), so per-test memory does not get released between tests in that mode. The scoped `cargo
test -p xlayer-builder --lib real_stream_screen_tx_and_screen_existing_consistent` run above is the
actual r10-ADVANCE-TEST cross-crate-availability proof the acceptance item needs (the `test-utils`-gated
helper compiles and passes when invoked from `xlayer-builder`'s own test target), and `cargo build -p
rcs-filter` is the production-side half of the dual verification (§4.1 proves the symbol is absent from
the default production graph; this proves `rcs-filter` alone still builds without the feature).

**Round-6 disposition (branch A — independently reconfirmed, not carried forward as a self-report):**

```
$ cargo test -p xlayer-builder
[... 190 tests pass in-process ...]
test tests::forks::jovian_minimum_fee_must_be_set_flashblocks ... ok
error: test failed, to rerun pass `-p xlayer-builder --lib`

Caused by:
  process didn't exit successfully: `.../target/debug/deps/xlayer_builder-868a9c6807f8561b`
  (signal: 9, SIGKILL: kill)
$ echo "${PIPESTATUS[0]}"
101

$ dmesg | grep -i 'oom-kill\|Killed process' | tail -1
oom-kill:constraint=CONSTRAINT_MEMCG,...,task=xlayer_builder-,pid=2151916,uid=1000
```

The two prior-round OOM kills are **independently reconfirmed this round**: the identical command,
run fresh in this round's own environment with zero code changes since the round-5 OOM reports (Task
5 §8.1 proves the round adds no code delta), reproduces the same kernel OOM kill against the same
`xlayer_builder-` test binary, at the same cgroup `memory.max` of 32 GiB, before the suite reaches its
end. This confirms the behavior is an infrastructure/environment constraint of this sandbox's libtest
single-process memory model, not a defect in the code under test — not as a carried-forward assertion,
but as a result observed directly in this round.

## 5. Formatting and lint

**Round-5 provenance (inherited):** first `cargo +nightly fmt -p xlayer-builder -p rcs-filter --
--check` attempt reported 3 mechanical long-expression wrapping diffs in the new `context.rs` test
code (committed separately as `style(builder): rustfmt the native-budget loop-level tests`); re-check
after `cargo +nightly fmt` reported exit 0.

**Round-6 re-capture (this round, at the round-6 submission tree):**

```
$ cargo +nightly fmt --all -- --check
$ echo $?
0

$ cargo clippy --all-targets --workspace -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3m 51s
$ echo $?
0
```

Zero `warning:` and zero `error:` lines in the clippy output (grep-verified:
`grep -c "warning:\|error:"` → `0`). Both commands re-run fresh this round, over the full workspace
(`--all`/`--workspace`, a superset of the round-5 `-p xlayer-builder -p rcs-filter` scope), confirming
EV-CI holds unchanged at the round-6 submission head.

## 6. Test-35 (loop-level fail-closed, M1/M2/M3) — retained, not re-authored

`loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt` is **RETAINED** across both the
round-5 delta and this round-6 delta — not re-authored, not re-tested from scratch.

- **Round-5 provenance (inherited, unchanged):** its exact source text (function body only, extracted
  the same way at both revisions) was byte-identical sha256 between the round-5 plan baseline `a0e5d7b7`
  and the round-5 submission head `ae04e4b3`:
  `sha256 = f1fb844f641360393f6f7ab50b61d9b8522b52c571d31efcc1e13a354b2b0674` at both.
- **Round-6 proof (this round):** Task 5 §8.1's `git diff --stat ae490d7ffebce921a8e957618b56672d192739f9..HEAD -- '*.rs'`
  is empty — no `.rs` file changed in round 6, so the function body covering this test is byte-identical
  to `ae490d7f` by construction; recomputing the sha256 would reproduce the same inherited value above.
- It PASSED again in this round's §3 run (`docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-nextest.txt`,
  `(255/413)`), proving the fail-closed-no-buffer-no-commit-no-receipt guarantee still holds.
- **M1/M2/M3 mutation resistance was NOT re-run this round.** It was established and recorded in an
  earlier commit (`test(builder): make the loop-level fail-closed test 35 falsifiable and
  mutation-resistant`) against the unchanged function body above; this round's §3 PASS is a correctness
  re-run only, not a repeat of the mutation-testing procedure. Stating this explicitly rather than
  implying re-running from a correctness PASS alone.
- This round's work (Tasks 1-6) touched only `docs/superpowers/**` (Task 5 §8.1); the sibling loop-level
  test added in round 5 (`loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt`) was
  likewise not touched this round.

## 7. r11-NT2 — pre-fix reproduction attempt

**Baseline:** `714e12934aca97eda1ff344d7d1fdc344baff03f` (the genuine pre-D1 revision; `depth > 0` gates
confirmed live at `rcs_capture.rs:215`, `:232`, `:412`).
**Fixture:** `internal_create_endowment_captured_through_composite` (existing at both revisions,
unchanged — no top-level CREATE was substituted for the internal one).
**Method:** both `crates/builder/src/flashblocks/rcs_capture.rs` and `context.rs` (the files that
differ between the two revisions in a way that would otherwise fail to compile together — current
`context.rs` matches on a `CaptureInvariantError::CreateAddressMismatch` variant absent at 714e1293)
were swapped in-place to their exact `git show 714e1293:<path>` content, the fixture was run, then both
files were restored from the pre-swap backup and `git diff --quiet` confirmed a byte-identical return to
`HEAD`.
**Command:** `cargo nextest run -p xlayer-builder -E 'test(internal_create_endowment_captured_through_composite)'`
**Result:** exit 0, **PASS** (1 passed, 0 failed).

**Classification: (b) HONEST NON-REPRODUCTION — partial, mixed-worktree.** This fixture does not
reproduce the original NT2 defect even on the genuine pre-fix revision: the CREATE in this fixture
executes at a point where `journal().depth()` still reads greater than zero under the pre-fix hook
timing, so the `depth > 0` gate correctly captures it regardless. The original bug report's exact
triggering shape is not reproduced by this (or any other) fixture already present in the repository;
recorded here as an honest non-reproduction rather than asserting an unverified "RED pre-D1" claim.

**Scope boundary of this result (stated explicitly, not implied):**

1. Only two files — `crates/builder/src/flashblocks/rcs_capture.rs` and `context.rs` — were substituted
   from `714e12934aca97eda1ff344d7d1fdc344baff03f`; every other file in the worktree remained at the
   post-fix revision.
2. The run therefore executed a **mixed worktree**, not a clean `714e1293` checkout.
3. The result is accordingly a **partial non-reproduction**: it shows this fixture does not reproduce
   the defect under that specific two-file substitution, and it is explicitly **not** full old-checkout
   coverage of the pre-fix revision.
4. A clean old checkout was not used because current `context.rs` matches on
   `CaptureInvariantError::CreateAddressMismatch`, a variant absent at `714e1293` — the two revisions do
   not compile together, so a full old checkout would have been a different experiment, which this
   round did not run.

## 8. Preservation

`docs/superpowers/`, `.superpowers/`, `superpowers-context.tar.gz` and the Terran review-ledger
references in the design/plan documents are preserved — none were deleted or truncated by this round's
work.

### 8.1 Round-6 change surface

```
$ git diff --name-only ae490d7ffebce921a8e957618b56672d192739f9..HEAD
docs/superpowers/history/lark-review-doc-r10-detailed-from-stage5-archive.md
docs/superpowers/lark-review-doc.md
docs/superpowers/plans/2026-10-14-rcs-native-transfer-mr77-fixes-plan-r11-round6.md
docs/superpowers/specs/2026-10-12-rcs-native-transfer-mr77-fixes-design.md
docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md
docs/superpowers/specs/2026-10-14-rcs-native-transfer-mr77-r11-feature-tree.txt

$ git diff --stat ae490d7ffebce921a8e957618b56672d192739f9..HEAD -- crates bin Cargo.toml Cargo.lock
(empty)
```

No file under `crates/**`, `bin/**`, `Cargo.toml` or `Cargo.lock` was modified in round 6. The native
`LimitExceeded` group and the CREATE-inactive-hit group are untouched; no trigger scenario was named,
and none was needed.
