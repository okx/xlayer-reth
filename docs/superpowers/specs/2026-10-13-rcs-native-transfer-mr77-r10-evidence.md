# RCS Filter native-transfer fixes — revision r10 per-SHA evidence

> Traceability document. Design ids, requirement ids and review records belong in `docs/superpowers/`,
> in the stage report and in MR metadata; they are deliberately absent from the production and test
> sources.

Design: Lark revision **r10**, token `UXohdEdeaokIGpxWnJPlaqGugac`, canonical sha256
`87cb906d8da9422db3c5d75ecf4464cf84b5e20b62e5725419596c0e5d203c68`.
Plan: `docs/superpowers/plans/2026-10-13-rcs-native-transfer-mr77-fixes-plan-r10.md`.
Branch: `xl/xlop-1217/0928-1135`. The exact head SHA after this evidence commit series is recorded
in the merge-request description and in the stage implementation report; the evidence below was
produced from the tree that series commits.

## 1. Evidence contract

| Item | Requirement | Section |
|---|---|---|
| AC5.1 | Readable `cargo nextest` log over **both** `xlayer-builder` and `rcs-filter`, with the exact command, the process exit code and named per-test results | §3 |
| AC5.2 | `cargo +nightly fmt --all --check` exit 0 **and** `cargo clippy --all-targets --workspace -- -D warnings` 0 warnings / 0 errors at the same head | §4 |
| AC5.3 | The loop-level fail-closed gate marked retained (covering blob proven byte-identical) versus re-tested (re-run result recorded) | §5 |
| AC5.4 | The review request carries the head SHA and a per-item disposition | §6 |

A bare pass count is not evidence; the raw log referenced in §3 is the evidence of record.

## 2. Acceptance id → artifact → evidence

| Id | Class | Artifact | Assertion / evidence | Result |
|---|---|---|---|---|
| r10-ADVANCE-CFG | PROD | `crates/rcs-filter/Cargo.toml` (`[features] test-utils`), `crates/rcs-filter/src/handle.rs` (`cfg(any(test, feature = "test-utils"))`), `crates/builder/Cargo.toml` (`[dev-dependencies]` only) | `cargo build -p rcs-filter` compiles with the feature off (helpers not exposed); the builder test build compiles with it on and passes | PASS (§4.1) |
| r10-ADVANCE-TEST | TEST | `crates/builder/src/flashblocks/rcs_capture.rs` re-simulation test | compiles and calls `advance_to_approved_for_test` | PASS (`real_stream_screen_tx_and_screen_existing_consistent`) |
| r10-T3-LEGACYVAL | TEST | `legacy_value_rule_still_fires_on_real_capture_superset_stream` | rule bound to the transaction-level `value` fires on the real superset stream; negative control `value = 0` → `Allow` | PASS |
| r10-T3-DUALALIAS | TEST | `dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream` | two real streams: 1 transfer within budget → `Allow`; 2 transfers over budget → `Deny`; at the exact count → `Allow` | PASS |
| r10-T3-AUDITMERGE | TEST | `audit_type_action_merge_action_content_no_empty_audit_on_real_stream` | same `(audit_type, log_index)` → one action with decoded `transfer` inputs; distinct `audit_type` retained; all-null binding → no action, with a positive control | PASS |
| r10-T3-NT2-EVID | SPEC | this document §6.1 | pre-fix mechanism recorded from the branch history, not asserted as reproduced | recorded |
| r10-DMM-LOOP | TEST | `loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt` (with `create_address_mismatch_through_real_create_end_fails_closed` retained as the invariant anchor) | `LAST_SCREEN_DECISION == Some(Screen::Deny)`, `screen_tx` not called, no buffer entry, no commit, no receipt; no-fault control buffers | PASS |
| r10-DMM-ABORT | TEST | `address_mismatch_flag_cleared_by_both_finish_and_abort` | flag set → `finish_capture` reports and clears; flag set → `abort_capture` clears | PASS |
| r10-T1-DERIVE | PROD+TEST | `create` hook derivation; `derivation_ran_true_when_journal_read_for_address_derivation`, `derivation_ran_false_when_create_hook_not_hit`, `derivation_ran_false_but_capture_body_ran_for_create2` | the flag is computed by the derivation; "ran" / "not hit" / "ran without a journal read" are three distinguishable states | PASS |
| r10-OVF-ASSERT | TEST | `create_nonce_overflow_is_ok_true_address_none_revoked`, `create2_nonce_overflow_is_ok_true_address_none_revoked` | real completion `is_ok() == true` **and** `address.is_none()`, plus a sticky `observed_count` that revocation did not decrement | PASS |
| r10-T2-ORIGIN | TEST | `eip7702_origin_itself_delegated_runs_inner_call_and_create` | CALL `to == call_recipient` and `value == 1 wei`; CREATE `to ==` the exact `CreateOutcome.address` and `value == 1 wei`; zero / caller / call-recipient explicitly excluded; execution success asserted | PASS |
| r10-REGRESS-KEEP | KEEP | `top_level_call_with_value_emits_transfer`, `root_success_child_revert_keeps_root_drops_child`, `whole_tx_root_failure_drops_all`, `create_early_failure_no_address_or_event_leak`, `next_capture_starts_clean`, `create_is_ok_early_return_no_endowment_leak` | all six present by exact name and passing | PASS |
| r10-EV-LOG | SPEC | §3 | exact command, exit code, named per-test results | recorded |
| r10-EV-TABLE | SPEC | the merge-request test table | every listed name exists in the source at this head | recorded |
| r10-EV-CI | SPEC | §4 | `fmt --check` exit 0 and workspace clippy 0/0, each with command and exit code | recorded |
| r10-EV-T35 | SPEC/KEEP | §5 | retained (blob proven identical) versus re-tested | recorded |
| r10-EV-SUPERPOWERS | KEEP | `docs/superpowers/`, `.superpowers/`, checkpoint tarballs | preserved, not deleted or truncated | recorded |

The design calls this set "19 ids" but enumerates 17; the table follows the enumerated 17.

## 3. Per-SHA test log

```
$ cargo nextest run -p xlayer-builder -p rcs-filter
   ... (per-test lines)
     Summary [ 245.621s] 409 tests run: 409 passed, 0 skipped
$ echo $?
0
```

Each id-bearing test, verbatim from the run above (durations elided):

```
PASS xlayer-builder flashblocks::context::loop_level_fail_closed_tests::loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt
PASS xlayer-builder flashblocks::context::loop_level_fail_closed_tests::loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::address_mismatch_flag_cleared_by_both_finish_and_abort
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::audit_type_action_merge_action_content_no_empty_audit_on_real_stream
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create2_nonce_overflow_is_ok_true_address_none_revoked
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_address_mismatch_through_real_create_end_fails_closed
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_early_failure_no_address_or_event_leak
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_is_ok_early_return_no_endowment_leak
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_nonce_overflow_is_ok_true_address_none_revoked
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::derivation_ran_false_but_capture_body_ran_for_create2
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::derivation_ran_false_when_create_hook_not_hit
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::derivation_ran_true_when_journal_read_for_address_derivation
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::eip7702_origin_itself_delegated_runs_inner_call_and_create
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::legacy_value_rule_still_fires_on_real_capture_superset_stream
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::next_capture_starts_clean
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::one_transfer_rule_denies_both_toplevel_and_internal_real_stream
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::original_nt2_internal_create_denied_through_real_screen_tx
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::real_stream_native_budget_limit_plus_one_denies
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::real_stream_screen_tx_and_screen_existing_consistent
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::root_success_child_revert_keeps_root_drops_child
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::top_level_call_with_value_emits_transfer
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::top_level_native_denied_through_real_screen_tx
PASS xlayer-builder flashblocks::rcs_capture::gating_integration_tests::whole_tx_root_failure_drops_all
PASS rcs-filter integration_tests::t3a_native_endowment_transfer_log_denied_through_screen_tx
PASS rcs-filter integration_tests::t3b_single_rule_denies_erc20_and_native_transfer_in_same_tx
PASS rcs-filter integration_tests::t3c_event_binding_budget_overflow_denies_tx
PASS rcs-filter integration_tests::t3d_two_audit_rules_same_audit_type_produce_single_pool_entry
PASS rcs-filter integration_tests::t3e_post_approval_re_screen_produces_audit_approved_for_native_transfer
PASS rcs-filter integration_tests::t3f_native_and_erc20_transfer_produce_identical_decision_no_matching_change_needed
```

## 4. Formatting, lint and feature isolation

```
$ cargo +nightly fmt --all --check
$ echo $?
0

$ cargo clippy --all-targets --workspace -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 39.99s
$ echo $?
0
```

Zero `warning:` and zero `error:` lines in the clippy output.

### 4.1 Feature isolation (production graph does not expose the helpers)

```
$ cargo build -p rcs-filter
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 22.20s
$ echo $?
0

$ cargo check -p xlayer-builder --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4m 36s
$ echo $?
0
```

## 5. Loop-level gate: retained vs re-tested

| Item | Status | Evidence |
|---|---|---|
| `context.rs::loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt` | **RETAINED** — covering blob byte-identical to the branch base | extracted-function sha256 `6685e407887f1d0d0cce1819f7c82de872a9737f6d40b08c5fa395779ecf621b`, 8953 bytes, equal at both revisions |
| `context.rs::fail_closed_screen` (the fail-closed funnel the gate drives) | **RETAINED** — byte-identical | sha256 prefix `54cb3ab14634da7d` equal at both revisions |
| **M1** — the candidate must not reach `screen_tx` on the fail-closed path (buffer entry absent, control proves it would otherwise buffer) | retained assertion | re-tested at this head, PASS |
| **M2** — the decision must be `Deny`, never `Drop` (asserted through the decision seam, not inferred from eviction) | retained assertion | re-tested at this head, PASS |
| **M3** — no state commit before the `continue` (sender nonce and balance unchanged across the loop) | retained assertion | re-tested at this head, PASS |
| `context.rs::loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt` | **NEW** — added by this revision | re-tested at this head, PASS |

## 6. Per-item disposition

### 6.1 Original nested-CREATE reproducer: RED versus non-reproduction

The pre-fix mechanism is recorded here from the branch history rather than asserted as reproduced
at the current base, because at the current base the fix is already in place and re-establishing a
red run would require reverting it. The pre-fix `create` hook gated on the journal depth read
inside the hook:

```text
let depth = context.journal().depth();
let value = inputs.value();
if depth > 0 && !value.is_zero() { /* derive + record */ }
```

with `record_create_observation` applying the same `depth > 0` condition. That condition did not
reliably distinguish a nested `CREATE` frame from a top-level one, so a nested `CREATE` endowment
could be dropped from the observation stream. The fix removes the depth-based decision entirely and
observes every frame's real native value, which is correct regardless of the exact hook ordering —
so no ledger entry here claims "RED on the pre-fix baseline was reproduced"; it records the exact
mechanism and the revision that removed it.

### 6.2 Per-id disposition

| Id | Disposition |
|---|---|
| r10-ADVANCE-CFG | implemented (non-default feature + `[dev-dependencies]` opt-in); dual verification recorded in §4.1 |
| r10-ADVANCE-TEST | satisfied by the retained re-simulation test, which compiles only because the feature is on |
| r10-T3-LEGACYVAL | rewritten: transaction-level `value` rule, negative control added |
| r10-T3-DUALALIAS | rewritten: two-stream boundary comparison with a third "exactly at the count" control |
| r10-T3-AUDITMERGE | rewritten: action content, same-type dedup, distinct-type retention, all-null negative control with a positive counterpart |
| r10-T3-NT2-EVID | recorded (see §6.1) |
| r10-DMM-LOOP | implemented as a real candidate-loop test with a no-fault control |
| r10-DMM-ABORT | implemented |
| r10-T1-DERIVE | implemented (computed value; three distinguishable states) |
| r10-OVF-ASSERT | strengthened with real-completion and sticky-count assertions |
| r10-T2-ORIGIN | strengthened to exact per-transfer recipients and values |
| r10-REGRESS-KEEP | all six retained and passing |
| r10-EV-LOG | this section |
| r10-EV-TABLE | the merge-request test table |
| r10-EV-CI | §4 |
| r10-EV-T35 | §5 |
| r10-EV-SUPERPOWERS | preserved |