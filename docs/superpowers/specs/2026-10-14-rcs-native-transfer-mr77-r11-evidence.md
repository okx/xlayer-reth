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
| **r11-NT2** | SPEC | §7 | satisfied — honest non-reproduction, recorded |
| **r11-MR-TABLE** | SPEC | MR description (Task 5) | pending Task 5 |

**Deviation from the plan's literal Task 1 Step 6** (ledgered as a ruling, not a silent drop): the plan
sketches an additional `zero_native_budget_is_a_config_error_even_when_filter_disabled` test. This exact
property — `max_native_transfers_per_tx == 0` is a startup error even when the filter is disabled — is
already asserted by the existing `rcs-filter/src/config.rs::tests::budgets_are_validated_even_when_disabled`
test (native-budget half), confirmed present and passing (§3, line `rcs-filter config::tests::budgets_are_validated_even_when_disabled`). Adding a near-duplicate test under a new name would not
strengthen coverage; this ID is not in the plan's own 22-id acceptance table above, so skipping the
literal step is not a FAIL-rule violation.

## 3. Per-SHA test log — BOTH packages, workspace-wide

```
$ cargo nextest run -p xlayer-builder -p rcs-filter
   Compiling xlayer-builder v0.0.7 (.../crates/builder)
        PASS [   0.008s] (  1/413) rcs-filter client::wire_tests::required_response_fields_fail_decoding_when_missing
        PASS [   0.043s] (  2/413) rcs-filter client::wire_tests::oversized_response_is_rejected_before_body_allocation
        PASS [   0.042s] (  3/413) rcs-filter client::wire_tests::status_query_encoding_never_uses_tx_hashes
        PASS [   0.006s] (  4/413) rcs-filter config::tests::budgets_are_validated_even_when_disabled
        PASS [   0.005s] (  5/413) rcs-filter config::tests::default_config_budgets_are_10_000
        PASS [   0.005s] (  6/413) rcs-filter config::tests::defaults_match_expected_values
        PASS [   0.005s] (  7/413) rcs-filter config::tests::disabled_filter_skips_submit_concurrency_validation
        PASS [   0.005s] (  8/413) rcs-filter config::tests::enabled_requires_base_url
        PASS [   0.077s] (  9/413) rcs-filter client::wire_tests::rules_endpoints_reject_non_200
        PASS [   0.080s] ( 10/413) rcs-filter client::wire_tests::get_and_query_decode_only_valid_200_json
        PASS [   0.006s] ( 11/413) rcs-filter config::tests::protocol_version_is_exact_match
        PASS [   0.007s] ( 12/413) rcs-filter config::tests::network_timeout_and_backoff_bounds_are_validated
        PASS [   0.043s] ( 13/413) rcs-filter client::wire_tests::tx_hashes_query_encodes_only_tx_hashes
        PASS [   0.007s] ( 14/413) rcs-filter config::tests::retention_must_exceed_total_retry_timeout
        PASS [   0.006s] ( 15/413) rcs-filter config::tests::scheduling_and_state_durations_must_be_non_zero
        PASS [   0.007s] ( 16/413) rcs-filter config::tests::submit_concurrency_boundaries_are_validated_when_enabled
        PASS [   0.007s] ( 17/413) rcs-filter config::tests::validate_rejects_zero_event_bindings_budget
        PASS [   0.008s] ( 18/413) rcs-filter config::tests::validate_rejects_zero_native_budget
        PASS [   0.008s] ( 19/413) rcs-filter handle::tests::approved_can_become_outdated_before_canonical_cleanup
        PASS [   0.008s] ( 20/413) rcs-filter handle::tests::approved_consistency_mismatch_hard_drops
        PASS [   0.008s] ( 21/413) rcs-filter handle::tests::approved_consistency_pass_is_attempt_scoped
        PASS [   0.008s] ( 22/413) rcs-filter handle::tests::approved_mismatch_after_total_timeout_still_drops
        PASS [   0.007s] ( 23/413) rcs-filter handle::tests::audit_tx_enters_pool_as_pending
        PASS [   0.007s] ( 24/413) rcs-filter handle::tests::cancelled_attempt_rechecks_changed_actions
        PASS [   0.007s] ( 25/413) rcs-filter handle::tests::deny_tx_returns_deny_without_buffering
        PASS [   0.007s] ( 26/413) rcs-filter handle::tests::first_screen_matcher_overflow_emits_error_bounded_fields
        PASS [   0.007s] ( 27/413) rcs-filter handle::tests::excessive_complete_bindings_deny_without_buffering
        PASS [   0.007s] ( 28/413) rcs-filter handle::tests::max_native_transfers_accessor_returns_config_value
        PASS [   0.007s] ( 29/413) rcs-filter handle::tests::pending_precheck_marks_descendants_invalid_for_iterator
        PASS [   0.007s] ( 30/413) rcs-filter handle::tests::pre_screen_executes_approved_and_timeout_allow
        PASS [   0.008s] ( 31/413) rcs-filter handle::tests::pre_screen_defers_all_inflight_states
        PASS [   0.006s] ( 32/413) rcs-filter handle::tests::record_local_deny_creates_no_buffer_entry
        PASS [   0.008s] ( 33/413) rcs-filter handle::tests::rules_state_updates_on_install_and_reload
        PASS [   0.007s] ( 34/413) rcs-filter handle::tests::scenario_c_denied_drops_and_never_resubmits
        PASS [   0.008s] ( 35/413) rcs-filter handle::tests::screen_tx_uses_configured_event_binding_limit
        PASS [   0.008s] ( 36/413) rcs-filter handle::tests::scenario_d_outdated_drops_without_consistency_check
        PASS [   0.007s] ( 37/413) rcs-filter handle::tests::timed_out_allow_is_terminal_allow
        PASS [   0.007s] ( 38/413) rcs-filter handle::tests::unmatched_tx_is_allowed
        PASS [   0.008s] ( 39/413) rcs-filter integration_tests::approved_outage_past_total_timeout_still_checks_consistency
        PASS [   0.008s] ( 40/413) rcs-filter integration_tests::all_failed_groups_are_attempted_and_lowest_height_error_is_returned
        PASS [   0.008s] ( 41/413) rcs-filter integration_tests::channel_lag_reconciles_all_dropped_hashes
        PASS [   0.006s] ( 42/413) rcs-filter integration_tests::concurrent_partial_failure_keeps_independent_groups_isolated
        PASS [   0.007s] ( 43/413) rcs-filter integration_tests::denied_emits_one_discard_event
        PASS [   0.007s] ( 44/413) rcs-filter integration_tests::conflicting_duplicate_query_rows_prefer_terminal_rejection
        PASS [   0.145s] ( 45/413) rcs-filter client::wire_tests::response_body_stall_hits_total_request_timeout
        PASS [   0.009s] ( 46/413) rcs-filter integration_tests::duplicate_terminal_updates_are_idempotent
        PASS [   0.009s] ( 47/413) rcs-filter integration_tests::e2e_scenario_a_happy_path
        PASS [   0.007s] ( 48/413) rcs-filter integration_tests::fail_close_emits_discard_event
        PASS [   0.008s] ( 49/413) rcs-filter integration_tests::expired_later_height_is_never_submitted
        PASS [   0.008s] ( 50/413) rcs-filter integration_tests::failed_conflict_predecessor_releases_successor
        PASS [   0.010s] ( 51/413) rcs-filter integration_tests::four_way_concurrency_reduces_eight_group_latency_by_four_times
        PASS [   0.009s] ( 52/413) rcs-filter integration_tests::hot_reload_applies_to_new_tx_but_not_buffered
        PASS [   0.009s] ( 53/413) rcs-filter integration_tests::it_anonymous_audit_payload_matches_contract
        PASS [   0.008s] ( 54/413) rcs-filter integration_tests::it_approved_survives_rcs_outage_past_total_timeout
        PASS [   0.007s] ( 55/413) rcs-filter integration_tests::it_canonical_notification_cleans_buffer
        PASS [   0.008s] ( 56/413) rcs-filter integration_tests::it_concurrent_payloads_do_not_share_permit
        PASS [   0.008s] ( 57/413) rcs-filter integration_tests::it_discard_parks_nonce_descendant
        PASS [   0.008s] ( 58/413) rcs-filter integration_tests::it_late_submit_fail_close_emits_exactly_one_event
        PASS [   0.008s] ( 59/413) rcs-filter integration_tests::it_malformed_reload_keeps_old_rules
        PASS [   0.008s] ( 60/413) rcs-filter integration_tests::it_missing_rules_never_marks_version_installed
        PASS [   0.007s] ( 61/413) rcs-filter integration_tests::it_partially_invalid_reload_rejects_whole_batch_and_keeps_old_rules
        PASS [   0.007s] ( 62/413) rcs-filter integration_tests::it_parent_change_rechecks_approval
        PASS [   0.009s] ( 63/413) rcs-filter integration_tests::it_official_snapshot_all_event_shapes_submit_exact_wire
        PASS [   0.007s] ( 64/413) rcs-filter integration_tests::it_payload_cancel_does_not_cache_approval
        PASS [   0.007s] ( 65/413) rcs-filter integration_tests::it_pending_transactions_do_not_execute_each_flashblock
        PASS [   0.007s] ( 66/413) rcs-filter integration_tests::it_query_timeout_race_is_deterministic
        PASS [   0.008s] ( 67/413) rcs-filter integration_tests::it_rebroadcast_dropped_hash_is_reconciled
        PASS [   0.008s] ( 68/413) rcs-filter integration_tests::it_rcs_denied_removes_txpool_root
        PASS [   0.008s] ( 69/413) rcs-filter integration_tests::it_reorg_rescreens_reinserted_tx
        PASS [   0.008s] ( 70/413) rcs-filter integration_tests::it_submit_response_after_deadline_is_ignored
        PASS [   0.007s] ( 71/413) rcs-filter integration_tests::later_physical_log_changes_approval_consistency_hash
        PASS [   0.009s] ( 72/413) rcs-filter integration_tests::it_timeout_first_fail_close_emits_once_and_late_query_is_ignored
        PASS [   0.008s] ( 73/413) rcs-filter integration_tests::load_and_install_errors_when_unavailable
        PASS [   0.009s] ( 74/413) rcs-filter integration_tests::it_transfer_batch_payload_preserves_abi_arrays_through_submit
        PASS [   0.009s] ( 75/413) rcs-filter integration_tests::load_and_install_installs_and_marks_ready
        PASS [   0.008s] ( 76/413) rcs-filter integration_tests::load_and_install_rejects_unsupported_protocol
        PASS [   0.008s] ( 77/413) rcs-filter integration_tests::not_submitted_loop_recovers_after_rcs_returns
        PASS [   0.008s] ( 78/413) rcs-filter integration_tests::official_snapshot_compiles_and_matches_all_supported_transfer_shapes
        PASS [   0.008s] ( 79/413) rcs-filter integration_tests::out_of_order_independent_responses_update_only_their_own_generations
        PASS [   0.009s] ( 80/413) rcs-filter integration_tests::outdated_emits_discard_event
        PASS [   0.203s] ( 81/413) rcs-filter client::wire_tests::rules_version_and_required_fields_are_enforced_over_http
        PASS [   0.007s] ( 82/413) rcs-filter integration_tests::pool_lock_recovers_from_poisoning
        PASS [   0.008s] ( 83/413) rcs-filter integration_tests::partial_query_failure_still_applies_successful_statuses
        PASS [   0.007s] ( 84/413) rcs-filter integration_tests::query_once_maps_submitted_through_pending_to_approved
        PASS [   0.009s] ( 85/413) rcs-filter integration_tests::query_once_absent_status_leaves_entry_untouched
        PASS [   0.008s] ( 86/413) rcs-filter integration_tests::rules_lock_recovers_from_poisoning
        PASS [   0.010s] ( 87/413) rcs-filter integration_tests::rcs_failover_recovers_within_grace_period
        PASS [   0.008s] ( 88/413) rcs-filter integration_tests::serial_fallback_preserves_height_order_and_lowest_error
        PASS [   0.008s] ( 89/413) rcs-filter integration_tests::scenario_b_deny_makes_zero_rcs_calls
        PASS [   0.009s] ( 90/413) rcs-filter integration_tests::stale_query_response_does_not_approve_reinserted_lifecycle
        PASS [   0.008s] ( 91/413) rcs-filter integration_tests::stale_submit_response_does_not_advance_reinserted_lifecycle
        PASS [   0.008s] ( 92/413) rcs-filter integration_tests::submit_once_builds_contract_scenario_a_payload_verbatim
        PASS [   0.009s] ( 93/413) rcs-filter integration_tests::submit_group_isolation_continues_after_one_height_fails
        PASS [   0.011s] ( 94/413) rcs-filter integration_tests::startup_retries_until_rcs_available_then_recovers
        PASS [   0.011s] ( 95/413) rcs-filter integration_tests::submit_batch_metrics_cover_empty_completed_and_error_batches
        PASS [   0.223s] ( 96/413) rcs-filter client::wire_tests::client_recovers_after_stalled_response_without_recreation
        PASS [   0.010s] ( 97/413) rcs-filter integration_tests::submit_once_groups_by_block_height
        PASS [   0.010s] ( 98/413) rcs-filter integration_tests::submit_once_keeps_rejected_malformed_not_submitted
        PASS [   0.008s] ( 99/413) rcs-filter integration_tests::t3a_native_endowment_transfer_log_denied_through_screen_tx
        PASS [   0.011s] (100/413) rcs-filter integration_tests::submit_once_preserves_same_nonce_order_while_refilling_independent_work
        PASS [   0.011s] (101/413) rcs-filter integration_tests::submit_once_refills_slots_beyond_limit_while_first_group_is_blocked
        PASS [   0.010s] (102/413) rcs-filter integration_tests::t3b_single_rule_denies_erc20_and_native_transfer_in_same_tx
        PASS [   0.012s] (103/413) rcs-filter integration_tests::submit_once_preserves_same_quota_order_while_refilling_independent_work
        PASS [   0.010s] (104/413) rcs-filter integration_tests::t3c_event_binding_budget_overflow_denies_tx
        PASS [   0.010s] (105/413) rcs-filter integration_tests::t3d_two_audit_rules_same_audit_type_produce_single_pool_entry
        PASS [   0.008s] (106/413) rcs-filter integration_tests::t3f_native_and_erc20_transfer_produce_identical_decision_no_matching_change_needed
        PASS [   0.009s] (107/413) rcs-filter integration_tests::t3e_post_approval_re_screen_produces_audit_approved_for_native_transfer
        PASS [   0.010s] (108/413) rcs-filter integration_tests::terminal_tombstone_is_pruned_after_retention
        PASS [   0.010s] (109/413) rcs-filter integration_tests::transaction_inserted_during_slow_batch_is_processed_by_next_batch
        PASS [   0.010s] (110/413) rcs-filter integration_tests::transfer_batch_approval_consistency_is_array_exact
        PASS [   0.010s] (111/413) rcs-filter integration_tests::unsupported_protocol_at_runtime_keeps_old_rules
        PASS [   0.011s] (112/413) rcs-filter integration_tests::unknown_audit_type_is_a_global_submit_barrier
        PASS [   0.009s] (113/413) rcs-filter matching::tests::all_events_missing_audit_emits_no_action_and_allows
        PASS [   0.009s] (114/413) rcs-filter integration_tests::unsupported_protocol_skips_pull_and_recovers_without_content_bump
        PASS [   0.007s] (115/413) rcs-filter matching::tests::anonymous_indexed_event_matches_by_successful_decode
        PASS [   0.008s] (116/413) rcs-filter matching::tests::anonymous_event_rejects_wrong_topic_count_and_malformed_data
        PASS [   0.008s] (117/413) rcs-filter matching::tests::anonymous_non_indexed_event_preserves_all_successful_logs
        PASS [   0.010s] (118/413) rcs-filter matching::tests::blacklist_target_is_not_denied
        PASS [   0.011s] (119/413) rcs-filter matching::tests::binding_budget_boundary_100_121
        PASS [   0.011s] (120/413) rcs-filter matching::tests::complete_binding_count_rejects_usize_multiplication_overflow
        PASS [   0.011s] (121/413) rcs-filter matching::tests::contract_creation_is_denied
        PASS [   0.009s] (122/413) rcs-filter matching::tests::deny_beats_audit_under_all_null_bindings
        PASS [   0.010s] (123/413) rcs-filter matching::tests::deny_stops_remaining_candidate_scan
        PASS [   0.009s] (124/413) rcs-filter matching::tests::distinct_logs_in_the_same_audit_type_are_preserved
        PASS [   0.009s] (125/413) rcs-filter matching::tests::distinct_aliases_on_same_log_are_deduplicated_by_audit_type_and_log_index
        PASS [   0.019s] (126/413) rcs-filter matching::tests::complete_binding_budget_is_accumulated_across_candidate_rules
        PASS [   0.008s] (127/413) rcs-filter matching::tests::filter_skip_contract_mismatch_decodes_zero_logs
        PASS [   0.007s] (128/413) rcs-filter matching::tests::identical_cross_rule_items_are_deduplicated
        PASS [   0.008s] (129/413) rcs-filter matching::tests::filter_skip_origin_mismatch_decodes_zero_logs
        PASS [   0.009s] (130/413) rcs-filter matching::tests::excessive_complete_bindings_are_rejected_and_fail_closed
        PASS [   0.007s] (131/413) rcs-filter matching::tests::identical_physical_logs_at_distinct_indexes_are_all_preserved
        PASS [   0.008s] (132/413) rcs-filter matching::tests::later_matching_log_denies_and_discards_collected_audit_actions
        PASS [   0.010s] (133/413) rcs-filter matching::tests::literal_true_rule_without_any_physical_log_denies_but_never_audits
        PASS [   0.010s] (134/413) rcs-filter matching::tests::low_binding_budget_2_3_boundary
        PASS [   0.010s] (135/413) rcs-filter matching::tests::later_matching_log_is_included_in_audit_actions
        PASS [   0.011s] (136/413) rcs-filter matching::tests::malformed_log_is_missing_event_denies_for_deny_allows_for_audit
        PASS [   0.010s] (137/413) rcs-filter matching::tests::matching_signature_with_malformed_data_does_not_match
        PASS [   0.009s] (138/413) rcs-filter matching::tests::no_log_tx_hits_contract_address_deny
        PASS [   0.011s] (139/413) rcs-filter matching::tests::missing_named_event_binds_null
        PASS [   0.010s] (140/413) rcs-filter matching::tests::no_log_tx_negative_condition_on_missing_event_field_denies
        PASS [   0.010s] (141/413) rcs-filter matching::tests::same_log_uses_strictest_action_and_audit_union
        PASS [   0.011s] (142/413) rcs-filter matching::tests::no_matching_log_allows
        PASS [   0.010s] (143/413) rcs-filter matching::tests::same_physical_log_is_kept_independently_for_distinct_audit_types
        PASS [   0.010s] (144/413) rcs-filter matching::tests::scenario_a_matches_audit_with_quota_item
        PASS [   0.010s] (145/413) rcs-filter matching::tests::scenario_b_deny_short_circuits
        PASS [   0.010s] (146/413) rcs-filter matching::tests::transfer_batch_decodes_uint_arrays_as_json_string_arrays
        PASS [   0.011s] (147/413) rcs-filter matching::tests::unrelated_log_is_treated_as_missing_event_and_still_denies
        PASS [   0.010s] (148/413) rcs-filter pool::tests::approved_ignores_total_retry_timeout
        PASS [   0.010s] (149/413) rcs-filter matching::tests::within_budget_non_first_complete_multi_event_binding_is_evaluated
        PASS [   0.010s] (150/413) rcs-filter pool::tests::canonical_cleanup_removes_only_committed_hashes
        PASS [   0.011s] (151/413) rcs-filter pool::tests::canonical_lag_recovery_keeps_only_fail_closed_tombstones
        PASS [   0.010s] (152/413) rcs-filter pool::tests::concurrent_outdated_beats_consistency_pass
        PASS [   0.010s] (153/413) rcs-filter pool::tests::concurrent_pending_beats_consistency_pass
        PASS [   0.010s] (154/413) rcs-filter pool::tests::inconsistent_approved_entry_is_dropped
        PASS [   0.010s] (155/413) rcs-filter pool::tests::late_denied_cannot_override_expired_fail_open
        PASS [   0.010s] (156/413) rcs-filter pool::tests::late_approved_cannot_override_expired_fail_close
        PASS [   0.008s] (157/413) rcs-filter pool::tests::non_terminal_reconciliation_keeps_terminal_tombstones
        PASS [   0.009s] (158/413) rcs-filter pool::tests::non_terminal_reconciliation_is_generation_fenced
        PASS [   0.008s] (159/413) rcs-filter pool::tests::outer_timeout_boundary_is_strict_and_tombstones_release
        PASS [   0.008s] (160/413) rcs-filter pool::tests::outer_timeout_fail_close_tombstones_dropped
        PASS [   0.007s] (161/413) rcs-filter pool::tests::pending_timeout_boundary_is_strict
        PASS [   0.008s] (162/413) rcs-filter pool::tests::query_before_deadline_applies_normally
        PASS [   0.008s] (163/413) rcs-filter pool::tests::query_denied_and_outdated_tombstone_as_dropped
        PASS [   0.008s] (164/413) rcs-filter pool::tests::prune_terminal_evicts_only_expired_tombstones
        PASS [   0.008s] (165/413) rcs-filter pool::tests::retry_cycles_preserve_first_not_submitted_at
        PASS [   0.006s] (166/413) rcs-filter pool::tests::stale_consistency_result_cannot_mutate_reinserted_hash
        PASS [   0.006s] (167/413) rcs-filter pool::tests::stale_submit_and_query_responses_cannot_advance_reinserted_hash
        PASS [   0.007s] (168/413) rcs-filter pool::tests::submit_response_advances_only_accepted
        PASS [   0.006s] (169/413) rcs-filter quota_hash::tests::different_value_hashes_differ
        PASS [   0.007s] (170/413) rcs-filter pool::tests::unrecognized_status_is_not_terminal
        PASS [   0.007s] (171/413) rcs-filter pool::tests::total_timeout_resolves_submitted_and_pending
        PASS [   0.007s] (172/413) rcs-filter pool::tests::submitted_stall_reverts_to_not_submitted
        PASS [   0.007s] (173/413) rcs-filter quota_hash::tests::identical_content_hashes_equal
        PASS [   0.009s] (174/413) rcs-filter quota_hash::tests::param_insertion_order_hashes_equal
        PASS [   0.008s] (175/413) rcs-filter quota_hash::tests::transfer_batch_arrays_are_order_sensitive_and_deterministic
        PASS [   0.009s] (176/413) rcs-filter rules::jsonlogic::tests::empty_and_or_are_valid_and_falsy
        PASS [   0.009s] (177/413) rcs-filter rules::jsonlogic::tests::empty_and_or_and_non_accelerable_combinators_stay_raw_and_false
        PASS [   0.009s] (178/413) rcs-filter rules::jsonlogic::tests::combinators_with_accelerable_child_are_specialized_and_exact
        PASS [   0.009s] (179/413) rcs-filter rules::jsonlogic::tests::compiled_matches_truthy_for_all_shapes
        PASS [   0.008s] (180/413) rcs-filter rules::jsonlogic::tests::eq_and_and_match_scenario_a
        PASS [   0.007s] (181/413) rcs-filter rules::jsonlogic::tests::fast_path_hit_miss_case_and_duplicates_match_linear
        PASS [   0.009s] (182/413) rcs-filter rules::jsonlogic::tests::identical_normalized_lists_share_one_arc
        PASS [   0.009s] (183/413) rcs-filter rules::jsonlogic::tests::in_under_non_boolean_operator_stays_raw_and_matches_linear
        PASS [   0.009s] (184/413) rcs-filter rules::jsonlogic::tests::literal_true_is_truthy
        PASS [   0.010s] (185/413) rcs-filter rules::jsonlogic::tests::in_list_blacklist
        PASS [   0.010s] (186/413) rcs-filter rules::jsonlogic::tests::missing_var_is_null_and_comparisons_are_false
        PASS [   0.011s] (187/413) rcs-filter rules::jsonlogic::tests::nested_and_or_return_operands
        PASS [   0.011s] (188/413) rcs-filter rules::jsonlogic::tests::negation_and_neq
        PASS [   0.011s] (189/413) rcs-filter rules::jsonlogic::tests::non_address_needle_falls_back_to_linear
        PASS [   0.007s] (190/413) rcs-filter rules::jsonlogic::tests::ordinary_strings_are_case_sensitive_but_addresses_are_not
        PASS [   0.007s] (191/413) rcs-filter rules::jsonlogic::tests::numeric_gt_18_digit_precision
        PASS [   0.008s] (192/413) rcs-filter rules::jsonlogic::tests::u256_boundaries_compare_exactly
        PASS [   0.008s] (193/413) rcs-filter rules::jsonlogic::tests::recognizes_only_static_all_address_arrays
        PASS [   0.009s] (194/413) rcs-filter rules::jsonlogic::tests::validates_all_supported_operators_and_rejects_bad_shapes
        PASS [   0.009s] (195/413) rcs-filter rules::validation::tests::audit_rule_with_empty_audit_types_is_rejected
        PASS [   0.008s] (196/413) rcs-filter rules::validation::tests::audit_without_timeout_action_defaults_allow
        PASS [   0.009s] (197/413) rcs-filter rules::validation::tests::duplicate_input_name_rejected
        PASS [   0.009s] (198/413) rcs-filter rules::validation::tests::duplicate_rule_ids_are_all_rejected_without_dropping_unique_rules
        PASS [   0.010s] (199/413) rcs-filter rules::validation::tests::event_alias_depth_above_eight_is_rejected
        PASS [   0.010s] (200/413) rcs-filter rules::validation::tests::empty_event_abis_rejected
        PASS [   0.361s] (201/413) rcs-filter client::wire_tests::endpoint_specific_statuses_are_enforced
        PASS [   0.009s] (202/413) rcs-filter rules::validation::tests::invalid_jsonlogic_rejects_only_affected_rules
        PASS [   0.009s] (203/413) rcs-filter rules::validation::tests::event_topic_limits_are_enforced
        PASS [   0.009s] (204/413) rcs-filter rules::validation::tests::invalid_rules_do_not_drop_valid_ones
        PASS [   0.008s] (205/413) rcs-filter rules::validation::tests::load_rules_shares_arc_across_rules_and_standalone_compiles
        PASS [   0.010s] (206/413) rcs-filter rules::validation::tests::missing_input_name_rejected
        PASS [   0.010s] (207/413) rcs-filter rules::validation::tests::missing_required_rule_and_event_fields_fail_decoding
        PASS [   0.010s] (208/413) rcs-filter rules::validation::tests::non_audit_rules_do_not_require_quota_event_shapes
        PASS [   0.009s] (209/413) rcs-filter rules::validation::tests::only_exact_event_abi_type_is_accepted
        PASS [   0.009s] (210/413) rcs-filter rules::validation::tests::quota_wrong_shape_rejected
        PASS [   0.009s] (211/413) rcs-filter rules::validation::tests::rendered_quota_rule_accepts_transfer_batch_shape
        PASS [   0.010s] (212/413) rcs-filter rules::validation::tests::scenario_a_rule_compiles_and_indexes
        PASS [   0.010s] (213/413) rcs-filter submit::tests::address_keys_are_compared_after_typed_normalization
        PASS [   0.011s] (214/413) rcs-filter submit::tests::batch_stats_track_actual_in_flight_and_release_slots
        PASS [   0.009s] (215/413) rcs-filter submit::tests::dependency_edges_always_point_from_lower_to_higher_height
        PASS [   0.009s] (216/413) rcs-filter submit::tests::different_erc1155_ids_with_same_recipient_and_token_still_conflict
        PASS [   0.009s] (217/413) rcs-filter submit::tests::empty_planner_has_no_segments
        PASS [   0.009s] (218/413) rcs-filter submit::tests::group_with_two_distinct_conflicts_waits_for_both_predecessors
        PASS [   0.009s] (219/413) rcs-filter submit::tests::empty_quota_items_and_invalid_token_are_exclusive
        PASS [   0.009s] (220/413) rcs-filter submit::tests::independent_groups_have_no_predecessors
        PASS [   0.009s] (221/413) rcs-filter submit::tests::malformed_known_quota_shapes_are_exclusive
        PASS [   0.010s] (222/413) rcs-filter submit::tests::malformed_or_empty_actions_are_exclusive
        PASS [   0.010s] (223/413) rcs-filter submit::tests::shared_nonce_and_quota_predecessor_is_counted_once
        PASS [   0.010s] (224/413) rcs-filter submit::tests::unknown_audit_type_is_an_exclusive_barrier
        PASS [   0.011s] (225/413) rcs-filter submit::tests::same_quota_groups_form_nearest_predecessor_chain
        PASS [   0.011s] (226/413) rcs-filter submit::tests::same_nonce_groups_form_nearest_predecessor_chain
        PASS [   0.010s] (227/413) rcs-filter submit::tests::valid_erc1155_batch_shape_remains_non_exclusive
        PASS [   0.009s] (228/413) rcs-filter test_support::mock::tests::get_rules_returns_registered_fixture
        PASS [   0.010s] (229/413) rcs-filter test_support::mock::tests::submit_echoes_and_query_returns_state
        PASS [   0.007s] (230/413) rcs-filter test_support::mock::tests::unavailable_errors
        PASS [   0.010s] (231/413) rcs-filter worker::tests::retry_schedule_is_exponential_capped_and_resets
        PASS [   0.010s] (232/413) rcs-filter worker::tests::retry_jitter_stays_between_half_base_and_cap
        PASS [   0.010s] (233/413) rcs-filter worker::tests::supervisor_restarts_worker_after_panic
        PASS [   0.019s] (234/413) xlayer-builder signer::test::test_deterministic_address_derivation
        PASS [   0.020s] (235/413) xlayer-builder flashblocks::best_txs::tests::test_simple_case
        PASS [   0.025s] (236/413) xlayer-builder args::op::tests::rcs_submit_concurrency_cli_metadata_has_default_and_env
        PASS [   0.017s] (237/413) xlayer-builder signer::test::test_public_key_format
        PASS [   0.020s] (238/413) xlayer-builder signer::test::test_sign_transaction
        PASS [   0.015s] (239/413) xlayer-builder flashblocks::context::bridge_intercept_tests::test_disabled_config_allows_all
        PASS [   0.041s] (240/413) xlayer-builder args::op::tests::rcs_submit_concurrency_cli_override_is_parsed
        PASS [   0.016s] (241/413) xlayer-builder flashblocks::context::bridge_intercept_tests::test_non_target_token_allowed
        PASS [   0.015s] (242/413) xlayer-builder flashblocks::context::bridge_intercept_tests::test_specific_token_blocks_bridge_tx
        PASS [   0.016s] (243/413) xlayer-builder flashblocks::context::bridge_intercept_tests::test_wildcard_blocks_bridge_tx
        PASS [   0.016s] (244/413) xlayer-builder flashblocks::context::capture_decision_tests::capture_guard_disarms_on_success_single_abort_on_err_no_double_abort
        PASS [   0.015s] (245/413) xlayer-builder flashblocks::context::capture_decision_tests::complete_is_not_fail_closed
        PASS [   0.017s] (246/413) xlayer-builder flashblocks::context::capture_decision_tests::enabled_filter_passthrough_fails_closed
        PASS [   0.016s] (247/413) xlayer-builder flashblocks::context::capture_decision_tests::invariant_violation_fails_closed
        PASS [   0.016s] (248/413) xlayer-builder flashblocks::context::capture_decision_tests::limit_exceeded_fails_closed
        PASS [   0.015s] (249/413) xlayer-builder flashblocks::context::capture_decision_tests::native_overflow_emits_error_with_bounded_fields_only
        PASS [   0.017s] (250/413) xlayer-builder flashblocks::context::capture_decision_tests::realogmismatch_emits_own_arm_four_fields_no_content
        PASS [   0.026s] (251/413) xlayer-builder flashblocks::context::loop_level_fail_closed_tests::loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt
        PASS [   0.228s] (252/413) xlayer-builder p2p::test::two_nodes_can_connect_and_message
        PASS [   0.028s] (253/413) xlayer-builder flashblocks::context::loop_level_fail_closed_tests::loop_level_native_limit_deny_does_not_poison_the_next_candidate
        PASS [   0.034s] (254/413) xlayer-builder flashblocks::context::loop_level_fail_closed_tests::loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt
        PASS [   0.027s] (255/413) xlayer-builder flashblocks::context::loop_level_fail_closed_tests::loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt
        PASS [   0.015s] (256/413) xlayer-builder flashblocks::generator::tests::test_block_cell_immediate_value
        PASS [   0.117s] (257/413) xlayer-builder flashblocks::generator::tests::test_block_cell_multiple_waiters
        PASS [   0.016s] (258/413) xlayer-builder flashblocks::generator::tests::test_block_cell_update_value
        PASS [   0.117s] (259/413) xlayer-builder flashblocks::generator::tests::test_block_cell_wait_for_value
        PASS [   0.016s] (260/413) xlayer-builder flashblocks::generator::tests::test_job_deadline
        PASS [   4.179s] (261/413) xlayer-builder flashblocks::generator::tests::test_payload_generator
        PASS [   0.016s] (262/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::abort_capture_mid_window_leaves_state_clean
        PASS [   0.014s] (263/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::address_mismatch_flag_cleared_by_both_finish_and_abort
        PASS [   0.021s] (264/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::audit_type_action_merge_action_content_no_empty_audit_on_real_stream
        PASS [   0.019s] (265/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::composite_post_exec_warming_preserved_with_rcs_inspector_inner
        PASS [   0.019s] (266/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create2_nonce_overflow_is_ok_true_address_none_revoked
        PASS [   0.017s] (267/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_address_mismatch_through_real_create_end_fails_closed
        PASS [   0.018s] (268/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_early_failure_no_address_or_event_leak
        PASS [   0.019s] (269/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_is_ok_early_return_no_endowment_leak
        PASS [   0.019s] (270/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_nonce_overflow_is_ok_true_address_none_revoked
        PASS [   0.019s] (271/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_recipients_balanced_before_finish_capture
        PASS [   0.018s] (272/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::create_recipients_push_pop_balanced
        PASS [   0.016s] (273/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::delegatecall_carries_no_transfer_event
        PASS [   0.020s] (274/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::delegatecall_chain_inner_call_and_create_captured
        PASS [   0.018s] (275/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::delegatecall_nested_call_and_create_attributed_to_delegator
        PASS [   0.016s] (276/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::derivation_ran_false_but_capture_body_ran_for_create2
        PASS [   0.016s] (277/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::derivation_ran_false_when_create_hook_not_hit
        PASS [   0.017s] (278/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::derivation_ran_true_when_journal_read_for_address_derivation
        PASS [   0.018s] (279/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream
        PASS [   0.021s] (280/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::eip7702_origin_itself_delegated_runs_inner_call_and_create
        PASS [   0.020s] (281/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::eip7702_type4_delegated_transfers_attributed_to_authority
        PASS [   0.019s] (282/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::inactive_create_hook_hit_is_nonzero_with_derivation_false
        PASS [   0.018s] (283/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::inactive_create_no_mutation_and_no_derivation_at_create_return
        PASS [   0.016s] (284/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::inactive_create_tx_mutates_no_capture_state
        PASS [   0.019s] (285/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::inactive_inspector_diso_create_tx_no_mutation
        PASS [   0.019s] (286/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::inspector_state_does_not_cross_transactions_through_real_evm
        PASS [   0.018s] (287/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::internal_call_transfer_captured_through_composite
        PASS [   0.016s] (288/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::internal_create2_eip684_collision_endowment_revoked
        PASS [   0.017s] (289/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::internal_create2_endowment_captured_through_composite
        PASS [   0.020s] (290/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::internal_create_endowment_captured_through_composite
        PASS [   0.018s] (291/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::internal_transfer_then_real_log_interleaved_captured_through_composite
        PASS [   0.019s] (292/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::legacy_value_rule_still_fires_on_real_capture_superset_stream
        PASS [   0.019s] (293/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::next_capture_starts_clean
        PASS [   0.018s] (294/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::non_create_control_leaves_create_hook_hits_zero
        PASS [   0.021s] (295/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::one_transfer_rule_denies_both_toplevel_and_internal_real_stream
        PASS [   0.020s] (296/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::original_nt2_internal_create_denied_through_real_screen_tx
        PASS [   0.019s] (297/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::parent_frame_revert_rolls_back_child_transfer_and_real_log_through_composite
        PASS [   0.018s] (298/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::real_stream_native_budget_limit_plus_one_denies
        PASS [   0.021s] (299/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::real_stream_screen_tx_and_screen_existing_consistent
        PASS [   0.018s] (300/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::reverted_child_frame_rolls_back_real_log_and_cross_check_passes
        PASS [   0.019s] (301/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::revoked_create_still_consumes_native_budget
        PASS [   0.017s] (302/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::revoked_create_with_prior_transfer_exceeds_budget_limit
        PASS [   0.019s] (303/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::root_success_child_revert_keeps_root_drops_child
        PASS [   0.017s] (304/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::runtime_forced_real_log_mismatch_fails_closed_through_composite
        PASS [   0.020s] (305/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::selfdestruct_transfer_captured_through_composite
        PASS [   0.019s] (306/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::top_level_call_with_value_emits_transfer
        PASS [   0.016s] (307/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::top_level_create_with_value_emits_transfer
        PASS [   0.021s] (308/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::top_level_native_denied_through_real_screen_tx
        PASS [   0.019s] (309/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::two_consecutive_txs_capture_independently
        PASS [   0.018s] (310/413) xlayer-builder flashblocks::rcs_capture::gating_integration_tests::whole_tx_root_failure_drops_all
        PASS [   0.017s] (311/413) xlayer-builder flashblocks::rcs_capture::tests::call_apparent_value_generates_no_event
        PASS [   0.017s] (312/413) xlayer-builder flashblocks::rcs_capture::tests::call_internal_nonzero_transfer_is_observed
        PASS [   0.017s] (313/413) xlayer-builder flashblocks::rcs_capture::tests::call_self_transfer_generates_no_event
        PASS [   0.015s] (314/413) xlayer-builder flashblocks::rcs_capture::tests::call_top_level_generates_no_event
        PASS [   0.018s] (315/413) xlayer-builder flashblocks::rcs_capture::tests::create_internal_endowment_is_observed_to_created_address
        PASS [   0.017s] (316/413) xlayer-builder flashblocks::rcs_capture::tests::create_top_level_generates_no_event
        PASS [   0.015s] (317/413) xlayer-builder flashblocks::rcs_capture::tests::create_zero_endowment_generates_no_event
        PASS [   0.017s] (318/413) xlayer-builder flashblocks::rcs_capture::tests::finish_capture_clears_state_on_invariant_exit
        PASS [   0.016s] (319/413) xlayer-builder flashblocks::rcs_capture::tests::finish_capture_clears_state_on_limit_exceeded_exit
        PASS [   0.017s] (320/413) xlayer-builder flashblocks::rcs_capture::tests::identical_from_to_value_not_deduplicated
        PASS [   0.014s] (321/413) xlayer-builder flashblocks::rcs_capture::tests::inactive_inspector_ignores_all_observations
        PASS [   0.017s] (322/413) xlayer-builder flashblocks::rcs_capture::tests::inactive_inspector_push_pop_checkpoint_and_create_recipients_stay_clean
        PASS [   0.016s] (323/413) xlayer-builder flashblocks::rcs_capture::tests::limit_th_allowed_limit_plus_one_overflows_sticky
        PASS [   0.015s] (324/413) xlayer-builder flashblocks::rcs_capture::tests::matching_real_logs_yield_complete
        PASS [   0.015s] (325/413) xlayer-builder flashblocks::rcs_capture::tests::native_asset_address_matches_identifier
        PASS [   0.035s] (326/413) xlayer-builder flashblocks::rcs_capture::tests::native_budget_boundary_10000_allowed_10001_overflows
        PASS [   0.016s] (327/413) xlayer-builder flashblocks::rcs_capture::tests::native_transfer_log_has_exact_shape
        PASS [   0.017s] (328/413) xlayer-builder flashblocks::rcs_capture::tests::noop_inspector_finish_is_passthrough
        PASS [   0.016s] (329/413) xlayer-builder flashblocks::rcs_capture::tests::observed_count_increments_and_does_not_roll_back
        PASS [   0.017s] (330/413) xlayer-builder flashblocks::rcs_capture::tests::real_log_count_mismatch_fails_closed
        PASS [   0.014s] (331/413) xlayer-builder flashblocks::rcs_capture::tests::real_log_order_mismatch_fails_closed
        PASS [   0.017s] (332/413) xlayer-builder flashblocks::rcs_capture::tests::reverse_transfers_are_two_ordered_events_not_netted
        PASS [   0.016s] (333/413) xlayer-builder flashblocks::rcs_capture::tests::reverse_transfers_parent_revert_removes_both_count_stays
        PASS [   0.016s] (334/413) xlayer-builder flashblocks::rcs_capture::tests::selfdestruct_shares_unified_budget_with_call
        PASS [   0.014s] (335/413) xlayer-builder flashblocks::rcs_capture::tests::selfdestruct_then_parent_revert_removes_event_but_count_stays
        PASS [   0.014s] (336/413) xlayer-builder flashblocks::rcs_capture::tests::selfdestruct_zero_or_self_target_consumes_no_budget
        PASS [   0.017s] (337/413) xlayer-builder flashblocks::rcs_capture::tests::start_capture_resets_state_between_txs
        PASS [   0.016s] (338/413) xlayer-builder flashblocks::rcs_capture::tests::zero_and_self_transfers_are_ignored
        PASS [   0.051s] (339/413) xlayer-builder flashblocks::rcs_filter_config_tests::budgets_default_to_10_000_and_map_into_config
        PASS [   0.038s] (340/413) xlayer-builder flashblocks::rcs_filter_config_tests::disabled_filter_bypasses_config_and_handle_construction
        PASS [   0.037s] (341/413) xlayer-builder flashblocks::rcs_filter_config_tests::disabled_filter_with_zero_event_bindings_is_startup_error
        PASS [   0.035s] (342/413) xlayer-builder flashblocks::rcs_filter_config_tests::disabled_filter_with_zero_native_budget_is_startup_error
        PASS [   0.055s] (343/413) xlayer-builder flashblocks::rcs_filter_config_tests::invalid_submit_concurrency_fails_before_workers_start
        PASS [   0.054s] (344/413) xlayer-builder flashblocks::rcs_filter_config_tests::submit_concurrency_is_mapped_into_filter_config
        PASS [   1.021s] (345/413) xlayer-builder flashblocks::service::rcs_txpool_tests::approved_reconciliation_removes_absent_real_txpool_entry
        PASS [   0.019s] (346/413) xlayer-builder flashblocks::service::rcs_txpool_tests::non_terminal_reconciliation_tracks_real_txpool_presence
        PASS [   0.016s] (347/413) xlayer-builder flashblocks::service::rcs_txpool_tests::real_txpool_discard_parks_nonce_descendant
        PASS [   1.122s] (348/413) xlayer-builder flashblocks::service::rcs_txpool_tests::terminal_consumer_removes_real_pool_tx_and_rebroadcast
        PASS [   0.017s] (349/413) xlayer-builder flashblocks::timing::tests::test_apply_offset
        PASS [   0.017s] (350/413) xlayer-builder flashblocks::timing::tests::test_calculate_first_flashblock_timing
        PASS [   0.016s] (351/413) xlayer-builder flashblocks::timing::tests::test_compute_remaining_time
        PASS [   0.017s] (352/413) xlayer-builder flashblocks::timing::tests::test_compute_scheduler_intervals
        PASS [   0.017s] (353/413) xlayer-builder flashblocks::timing::tests::test_new_different_block_times
        PASS [   0.015s] (354/413) xlayer-builder flashblocks::timing::tests::test_new_fcu_timing_scenarios
        PASS [   0.014s] (355/413) xlayer-builder flashblocks::timing::tests::test_new_with_offset_and_buffer
        PASS [   0.016s] (356/413) xlayer-builder flashblocks::timing::tests::test_scheduler_run_block_cancellation
        PASS [   0.018s] (357/413) xlayer-builder flashblocks::timing::tests::test_scheduler_run_cancellation_token_lifecycle
        PASS [   0.018s] (358/413) xlayer-builder flashblocks::timing::tests::test_scheduler_run_dropped_receiver
        PASS [   0.017s] (359/413) xlayer-builder flashblocks::timing::tests::test_scheduler_run_late_fcu_backwards_compat
        PASS [   0.017s] (360/413) xlayer-builder flashblocks::timing::tests::test_scheduler_run_sends_all_triggers
        PASS [   0.017s] (361/413) xlayer-builder flashblocks::utils::cache::tests::add_payload_new_id_replaces_sequence
        PASS [   0.015s] (362/413) xlayer-builder flashblocks::utils::cache::tests::add_payloads_same_id_appends
        PASS [   0.015s] (363/413) xlayer-builder flashblocks::utils::cache::tests::add_single_payload_creates_sequence
        PASS [   0.017s] (364/413) xlayer-builder flashblocks::utils::cache::tests::clone_shares_underlying_data
        PASS [   0.015s] (365/413) xlayer-builder flashblocks::utils::cache::tests::concurrent_add_and_read
        PASS [   0.018s] (366/413) xlayer-builder flashblocks::utils::cache::tests::default_cache_is_empty
        PASS [   0.016s] (367/413) xlayer-builder flashblocks::utils::cache::tests::get_txs_empty_cache_returns_none
        PASS [   0.017s] (368/413) xlayer-builder flashblocks::utils::cache::tests::get_txs_non_sequential_indexes_returns_none
        PASS [   0.017s] (369/413) xlayer-builder flashblocks::utils::cache::tests::get_txs_only_base_payload_returns_empty_vec
        PASS [   0.017s] (370/413) xlayer-builder flashblocks::utils::cache::tests::get_txs_with_none_parent_hash_returns_none
        PASS [   0.017s] (371/413) xlayer-builder flashblocks::utils::cache::tests::get_txs_wrong_parent_hash_returns_none
        PASS [   0.016s] (372/413) xlayer-builder flashblocks::utils::cache::tests::load_from_invalid_json_returns_none
        PASS [   0.014s] (373/413) xlayer-builder flashblocks::utils::cache::tests::load_from_nonexistent_file_returns_none
        PASS [   0.017s] (374/413) xlayer-builder flashblocks::utils::cache::tests::new_without_datadir_has_no_persist_path
        PASS [   0.015s] (375/413) xlayer-builder flashblocks::utils::cache::tests::parent_hash_extracted_from_first_payload_with_base
        PASS [   0.017s] (376/413) xlayer-builder flashblocks::utils::cache::tests::parent_hash_not_overwritten_once_set
        PASS [   0.018s] (377/413) xlayer-builder flashblocks::utils::cache::tests::persist_empty_cache_is_noop
        PASS [   0.020s] (378/413) xlayer-builder flashblocks::utils::cache::tests::persist_file_contains_valid_json
        PASS [   0.017s] (379/413) xlayer-builder flashblocks::utils::cache::tests::persist_no_path_is_noop
        PASS [   0.020s] (380/413) xlayer-builder flashblocks::utils::cache::tests::persist_overwrites_previous_file
        PASS [   0.019s] (381/413) xlayer-builder flashblocks::utils::cache::tests::persist_writes_file_and_load_restores
        PASS [   0.016s] (382/413) xlayer-builder flashblocks::utils::cache::tests::sequence_serde_roundtrip
        PASS [   0.015s] (383/413) xlayer-builder flashblocks::utils::cache::tests::sequence_serde_with_none_parent_hash
        PASS [   5.825s] (384/413) xlayer-builder tests::data_availability::block_fill_flashblocks
        PASS [   5.047s] (385/413) xlayer-builder tests::data_availability::block_size_limit_flashblocks
        PASS [   5.945s] (386/413) xlayer-builder tests::data_availability::da_footprint_fills_to_limit_flashblocks
        PASS [   5.040s] (387/413) xlayer-builder tests::data_availability::tx_size_limit_flashblocks
        PASS [   5.947s] (388/413) xlayer-builder tests::flashblocks::late_fcu_reduces_flashblocks_flashblocks
        PASS [  30.358s] (389/413) xlayer-builder tests::flashblocks::progressive_lag_reduces_flashblocks_flashblocks
        PASS [  17.990s] (390/413) xlayer-builder tests::flashblocks::smoke_basic_flashblocks
        PASS [  17.996s] (391/413) xlayer-builder tests::flashblocks::smoke_with_offset_flashblocks
        PASS [   5.581s] (392/413) xlayer-builder tests::flashblocks::test_flashblocks_no_state_root_calculation_flashblocks
        PASS [   8.428s] (393/413) xlayer-builder tests::flashblocks::test_flashblocks_number_contract_builder_tx_flashblocks
        PASS [   5.049s] (394/413) xlayer-builder tests::forks::jovian_block_parameters_set_flashblocks
        PASS [   6.050s] (395/413) xlayer-builder tests::forks::jovian_minimum_base_fee_flashblocks
        PASS [   3.241s] (396/413) xlayer-builder tests::forks::jovian_minimum_fee_must_be_set_flashblocks
        PASS [   6.649s] (397/413) xlayer-builder tests::forks::jovian_no_tx_pool_sync_flashblocks
        PASS [   5.087s] (398/413) xlayer-builder tests::miner_gas_limit::block_fill_flashblocks
        PASS [   5.034s] (399/413) xlayer-builder tests::miner_gas_limit::miner_gas_limit_flashblocks
        PASS [   7.069s] (400/413) xlayer-builder tests::miner_gas_limit::reset_gas_limit_flashblocks
        PASS [  12.807s] (401/413) xlayer-builder tests::smoke::block_includes_builder_transaction_flashblocks
        PASS [  21.043s] (402/413) xlayer-builder tests::smoke::produces_blocks_under_load_within_deadline_flashblocks
        PASS [   7.003s] (403/413) xlayer-builder tests::smoke::test_no_tx_pool_flashblocks
        PASS [   5.049s] (404/413) xlayer-builder tests::storage_cost::warm_cold_leak_across_dropped_tx_flashblocks
        PASS [   6.229s] (405/413) xlayer-builder tests::txpool::pending_pool_limit_flashblocks
        PASS [   6.131s] (406/413) xlayer-builder tests::xlayer_gasless::gasless_block_gas_limit_caps_and_resets_flashblocks
        PASS [   5.040s] (407/413) xlayer-builder tests::xlayer_gasless::gasless_same_block_revert_isolation_flashblocks
        PASS [   5.053s] (408/413) xlayer-builder tests::xlayer_gasless::gasless_same_block_success_isolation_flashblocks
        PASS [   5.042s] (409/413) xlayer-builder tests::xlayer_gasless::gasless_same_block_validation_failure_isolation_flashblocks
        PASS [   5.043s] (410/413) xlayer-builder tests::xlayer_gasless::gasless_tx_in_attributes_no_tx_pool_included_over_base_fee_flashblocks
        PASS [   3.247s] (411/413) xlayer-builder tests::xlayer_gasless::gasless_zero_price_tx_no_contract_rejected_flashblocks
        PASS [   3.248s] (412/413) xlayer-builder tests::xlayer_gasless::gasless_zero_price_tx_not_whitelisted_rejected_flashblocks
        PASS [   5.040s] (413/413) xlayer-builder tests::xlayer_gasless::gasless_zero_price_tx_whitelisted_included_flashblocks
     Summary [ 245.887s] 413 tests run: 413 passed, 0 skipped
$ echo $?
0
```

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
suite (the plan's literal Step 3 command) was attempted twice — once with the default thread count and
once with `--test-threads=2` — and both runs were terminated by the kernel OOM killer at this
environment's cgroup `memory.max` of 32 GiB (`dmesg`: `oom-kill:constraint=CONSTRAINT_MEMCG ...
task=xlayer_builder-`), after ~30 tests had already passed in-process. `cargo test`'s default libtest
harness runs all tests of one binary in ONE process (unlike `nextest`'s process-per-test model used in
§3, which ran all 413 tests, including every test in this binary, successfully), so per-test memory
does not get released between tests in that mode — an environment/resource constraint of this sandbox,
not a defect in the code under test. The scoped `cargo test -p xlayer-builder --lib
real_stream_screen_tx_and_screen_existing_consistent` run above is the actual r10-ADVANCE-TEST
cross-crate-availability proof the acceptance item needs (the `test-utils`-gated helper compiles and
passes when invoked from `xlayer-builder`'s own test target), and `cargo build -p rcs-filter` is the
production-side half of the dual verification (§4.1 proves the symbol is absent from the default
production graph; this proves `rcs-filter` alone still builds without the feature).

## 5. Formatting and lint

```
$ cargo +nightly fmt -p xlayer-builder -p rcs-filter -- --check
$ echo $?
0

$ cargo clippy --all-targets --workspace -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3m 06s
$ echo $?
0
```

Zero `warning:` and zero `error:` lines in the clippy output (grep-verified: `grep -c "warning:\|error:"`
→ `0`). First `cargo +nightly fmt -p xlayer-builder -p rcs-filter -- --check` attempt reported 3
mechanical long-expression wrapping diffs in the new `context.rs` test code (committed separately as
`style(builder): rustfmt the native-budget loop-level tests`); re-check after `cargo +nightly fmt`
reported exit 0.

## 6. Test-35 (loop-level fail-closed, M1/M2/M3) — retained

`loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt` is **RETAINED** (not re-tested
from scratch): its exact source text (function body only, extracted the same way at both revisions) is
byte-identical sha256 between the plan baseline and this revision —

```
sha256(baseline a0e5d7b7) = f1fb844f641360393f6f7ab50b61d9b8522b52c571d31efcc1e13a354b2b0674
sha256(this head  ae04e4b3) = f1fb844f641360393f6f7ab50b61d9b8522b52c571d31efcc1e13a354b2b0674
```

and it PASSED in §3's run (`(255/413)`), proving the M1/M2/M3 fail-closed-no-buffer-no-commit-no-receipt
guarantee still holds unchanged. This round's work (Tasks 1-2) added a SIBLING loop-level test
(`loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt`) without touching this
function.

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

**Classification: (b) HONEST NON-REPRODUCTION.** This fixture does not reproduce the original NT2 defect
even on the genuine pre-fix revision: the CREATE in this fixture executes at a point where
`journal().depth()` still reads greater than zero under the pre-fix hook timing, so the `depth > 0`
gate correctly captures it regardless. The original bug report's exact triggering shape is not
reproduced by this (or any other) fixture already present in the repository; recorded here as an
honest non-reproduction rather than asserting an unverified "RED pre-D1" claim.

## 8. Preservation

`docs/superpowers/`, `.superpowers/`, `superpowers-context.tar.gz` and the Terran review-ledger
references in the design/plan documents are preserved — none were deleted or truncated by this round's
work.
