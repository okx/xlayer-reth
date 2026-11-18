//! `known_allowed` classification: approved originals, synthetic-ban replay prefixes, the
//! content-binding inference for unlabelled entries, and the label map's precedence.

use std::collections::BTreeMap;

use alloy_primitives::{hex, keccak256, Address, Keccak256, B256, U256};
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use rcs_filter::audit::synthetic_ban::{
    verify_synthetic_ban, BATCH_ADD_TO_BLACKLIST_SIGNATURE, SYNTHETIC_BAN_SOURCE_HASH_DOMAIN,
};
use rcs_filter::test_support::synthetic_ban_golden::{
    golden_bans, golden_l1_user_deposit_source_hash,
};
use xlayer_rpc::audit::deposit::{build_deposit_tx, DepositTxRequest};
use xlayer_rpc::audit::rpc::test_seams::inferred_known_allowed_kind;
use xlayer_rpc::audit::rpc::KnownAllowedKind;
use xlayer_rpc::audit::verdict::{CallstackIdentification, Verdict};

use crate::harness::*;

/// The golden one-element synthetic ban, sent to `to`. The fixture proxy delegatecalls whatever
/// its calldata says, so a ban sent to it executes the implementation the rule inspects.
fn golden_ban_to(to: Address) -> DepositTxRequest {
    let ban = golden_bans().into_iter().find(|ban| ban.n == 1).expect("an n = 1 vector");
    DepositTxRequest { data: ban.data, ..deposit_request(&ban.source_hash, to) }
}

/// A caller-authored ban over `elements`, built with the public derivation.
fn self_built_ban(elements: &[B256], to: Address) -> DepositTxRequest {
    let mut data = keccak256(BATCH_ADD_TO_BLACKLIST_SIGNATURE.as_bytes())[..4].to_vec();
    data.extend_from_slice(&U256::from(32u64).to_be_bytes::<32>());
    data.extend_from_slice(&U256::from(elements.len()).to_be_bytes::<32>());
    let mut hasher = Keccak256::new();
    hasher.update(SYNTHETIC_BAN_SOURCE_HASH_DOMAIN);
    for element in elements {
        data.extend_from_slice(element.as_slice());
        hasher.update(element.as_slice());
    }
    DepositTxRequest {
        data: format!("0x{}", hex::encode(data)),
        ..deposit_request(&format!("{:#x}", hasher.finalize()), to)
    }
}

fn inferred(req: &DepositTxRequest) -> KnownAllowedKind {
    inferred_known_allowed_kind(&build_deposit_tx(req).expect("the request builds"))
}

// ------------------------------------------------------------- request-path hex policy ----

#[test]
fn every_spelling_of_a_golden_ban_classifies_as_a_ban_through_the_request_path() {
    for ban in golden_bans() {
        let bare_data = ban.data.trim_start_matches("0x").to_string();
        let bare_hash = ban.source_hash.trim_start_matches("0x").to_string();
        let data_spellings = [
            ban.data.clone(),
            bare_data.clone(),
            format!("0x{}", bare_data.to_uppercase()),
            format!("0x{}", mixed_case(&bare_data)),
        ];
        let hash_spellings =
            [ban.source_hash.clone(), bare_hash.clone(), format!("0x{}", bare_hash.to_uppercase())];
        for data in &data_spellings {
            for source_hash in &hash_spellings {
                let req =
                    DepositTxRequest { data: data.clone(), ..deposit_request(source_hash, PROXY) };
                assert_eq!(
                    inferred(&req),
                    KnownAllowedKind::SyntheticBan,
                    "n = {}, data {}…, source_hash {source_hash}",
                    ban.n,
                    &data[..12]
                );
            }
        }
    }
}

fn mixed_case(hex: &str) -> String {
    hex.chars()
        .enumerate()
        .map(|(i, c)| if i % 2 == 0 { c.to_ascii_uppercase() } else { c })
        .collect()
}

#[test]
fn undecodable_data_or_source_hash_is_malformed_and_never_executed_or_classified() {
    let ban = golden_ban_to(PROXY);
    let odd = DepositTxRequest { data: format!("{}0", ban.data), ..ban.clone() };
    let non_hex = DepositTxRequest { data: format!("{}zz", ban.data), ..ban.clone() };
    let bad_hash = DepositTxRequest { source_hash: "0x1234".to_string(), ..ban.clone() };
    for (case, req) in [("odd length", odd), ("non-hex", non_hex), ("short source_hash", bad_hash)]
    {
        assert!(build_deposit_tx(&req).is_err(), "{case}: the build fails");
        // The batch records a failed build as `None` and never executes it, so the result is
        // the non-verdict whatever the entry's class would have been.
        let run = run_batch(
            std::slice::from_ref(&req),
            vec![proxy_impl_allowlist_rule()],
            1,
            &[],
            None,
            seeded_db(APPROVED_IMPL, 1),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Malformed, "{case}");
        assert_eq!(result.source_hash, req.source_hash, "{case}");
    }
}

#[test]
fn a_transaction_that_is_not_a_deposit_is_never_a_ban() {
    use alloy_consensus::transaction::Recovered;
    use alloy_consensus::{SignableTransaction, TxLegacy};
    let ban = golden_bans().into_iter().find(|ban| ban.n == 1).expect("vector");
    let legacy =
        TxLegacy { input: hex::decode(&ban.data).expect("hex").into(), ..Default::default() }
            .into_signed(alloy_primitives::Signature::new(U256::ZERO, U256::ZERO, false));
    let tx = Recovered::new_unchecked(
        reth_optimism_primitives::OpTransactionSigned::from(
            op_alloy_consensus::OpTxEnvelope::Legacy(legacy),
        ),
        DEPOSITOR,
    );
    assert_eq!(inferred_known_allowed_kind(&tx), KnownAllowedKind::Original);
}

// ---------------------------------------------------------------- class behaviour ---------

/// a `known_allowed` Original executing unapproved implementation code returns
/// verdict `Deny`, and its state is NOT committed.
#[test]
fn known_allowed_original_running_unapproved_code_is_denied() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Deny);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Objected));
    assert_eq!(run.proxy_slot0, U256::ZERO, "state must not be committed");
}

/// the same with approved code returns `Allow` + `clean`, state committed.
#[test]
fn known_allowed_original_running_approved_code_is_allowed_and_committed() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        seeded_db(APPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Clean));
    assert_eq!(run.proxy_slot0, U256::from(1u64));
}

/// a `known_allowed` Original whose capture fails returns the non-verdict, state not
/// committed, reported `not_identified`.
#[test]
fn known_allowed_original_with_failed_capture_is_a_non_verdict() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Malformed);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO);
}

/// the non-verdict survives a rule that is filtered out BEFORE its selectors are ever
/// resolved. The request's only rule is narrowed to an origin the transaction does not have,
/// so the matcher skips it with zero decoding and therefore never looks at the (unavailable)
/// observation set. Priority is a property of the REQUEST, not of whichever rule happened to
/// survive filtering, so an unidentifiable execution must still be the non-verdict with no
/// state committed.
#[test]
fn capture_failure_is_the_non_verdict_even_when_every_rule_is_skipped_by_origin() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(
        result.verdict,
        Verdict::Malformed,
        "an unidentifiable execution is never reported Allow, whichever rules were skipped"
    );
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict must not commit state");

    // Counter-check: the SAME request and the SAME skipped rule under a sufficient budget is
    // adjudicated normally and committed, so the assertion above is about the unavailable
    // observation set and not about the origin narrowing by itself.
    let identified = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        seeded_db(APPROVED_IMPL, 1),
    );
    assert_eq!(only(&identified).verdict, Verdict::Allow);
    assert_eq!(only(&identified).callstack_identification, Some(CallstackIdentification::Clean));
    assert_eq!(identified.proxy_slot0, U256::from(1u64));
}

/// the synthetic-prefix exception is explicit and survives the request-level check: with the
/// same skipped rule and the same failed capture, a `SyntheticBan` still returns `Allow` and
/// its state IS committed, because later originals depend on that base state.
#[test]
fn synthetic_ban_prefix_keeps_allow_and_commit_when_every_rule_is_skipped_by_origin() {
    let prefix = format!("0x{}", "55".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY)],
        vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
        1,
        &[&prefix],
        Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let s = only(&run);
    assert_eq!(s.verdict, Verdict::Allow, "the prefix contract is unconditional");
    assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::from(1u64), "its commit is never withheld");
}

/// a replay prefix whose observations are AVAILABLE but stamped by a foreign snapshot is
/// reported `NotIdentified`, never `Clean` — even when the request's only rule is narrowed
/// away by `origin` so the matcher would skip it with zero decoding and return `Allow`.
///
/// The unavailable dimension cannot cover this: capture completed, so the set is `Available`
/// and only the stamp says the frames were produced under different rules. Driven through the
/// complete production batch with a real inspector, because the gap was in that arm's own
/// observation test and a direct call to the classifier would reproduce it.
#[test]
fn a_prefix_whose_observations_carry_a_foreign_stamp_is_not_identified() {
    in_isolated_process(
        module_path!(),
        "a_prefix_whose_observations_carry_a_foreign_stamp_is_not_identified",
        || {
            let prefix = format!("0x{}", "55".repeat(32));
            let raw_rules = vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)];
            let kinds: BTreeMap<String, KnownAllowedKind> =
                [(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect();

            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            let _guard = metrics::set_default_local_recorder(&recorder);
            let (run, out) = capture_tracing(|| {
                run_batch_stamped(
                    &[deposit_request(&prefix, PROXY)],
                    raw_rules.clone(),
                    1,
                    &[&prefix],
                    Some(kinds),
                    seeded_db(APPROVED_IMPL, 1),
                    Some(foreign_stamp(&raw_rules, 2)),
                )
            });

            let s = only(&run);
            assert_eq!(
                s.callstack_identification,
                Some(CallstackIdentification::NotIdentified),
                "observations adjudicable only under a different snapshot identify nothing"
            );
            // The prefix contract is unchanged by this fix: still `Allow`, still committed.
            assert_eq!(s.verdict, Verdict::Allow, "the prefix contract stays unconditional");
            assert_eq!(run.proxy_slot0, U256::from(1u64), "its commit is never withheld");

            assert!(
                out.contains("outcome=\"not_identified\""),
                "the single bounded anomaly log must report not_identified: {out}"
            );
            assert_eq!(
                out.lines().filter(|line| line.contains("synthetic_ban_identification")).count(),
                1,
                "exactly one bounded anomaly log for the prefix: {out}"
            );
            let mut counters: Vec<(String, u64)> = snapshotter
                .snapshot()
                .into_vec()
                .into_iter()
                .filter_map(|(key, _, _, value)| match value {
                    DebugValue::Counter(count)
                        if key
                            .key()
                            .name()
                            .starts_with("rcs_filter.synthetic_ban_identification") =>
                    {
                        Some((key.key().name().to_string(), count))
                    }
                    _ => None,
                })
                .collect();
            counters.sort();
            assert_eq!(
                counters,
                vec![
                    ("rcs_filter.synthetic_ban_identification.clean_total".to_string(), 0),
                    ("rcs_filter.synthetic_ban_identification.not_applicable_total".to_string(), 0),
                    ("rcs_filter.synthetic_ban_identification.not_identified_total".to_string(), 1),
                    ("rcs_filter.synthetic_ban_identification.objected_total".to_string(), 0),
                ],
                "the dedicated synthetic_ban series records not_identified and NOTHING else — in \
             particular `clean_total` stays at zero"
            );
        },
    );
}

/// the positive contrast for the test above, identical in every respect EXCEPT the snapshot
/// identity: with capture stamped by the evaluating snapshot the same prefix, the same
/// origin-narrowed rule and the same execution are still identified `Clean`, so the
/// assertion above is about the foreign stamp and not about the origin narrowing or about
/// every prefix now being reported unidentifiable.
#[test]
fn a_prefix_whose_observations_carry_the_evaluating_stamp_is_still_identified() {
    let prefix = format!("0x{}", "55".repeat(32));
    let raw_rules = vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)];
    let kinds: BTreeMap<String, KnownAllowedKind> =
        [(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect();
    let (run, out) = capture_tracing(|| {
        run_batch_stamped(
            &[deposit_request(&prefix, PROXY)],
            raw_rules,
            1,
            &[&prefix],
            Some(kinds),
            seeded_db(APPROVED_IMPL, 1),
            None,
        )
    });

    let s = only(&run);
    assert_eq!(
        s.callstack_identification,
        Some(CallstackIdentification::Clean),
        "a matching-stamp prefix is still identified normally"
    );
    assert_eq!(s.verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::from(1u64));
    assert!(
        !out.contains("synthetic_ban_identification"),
        "a clean prefix is counted without an anomaly log: {out}"
    );
}

/// a synthetic replay prefix keeps the pre-existing contract unconditionally:
/// even when a rule objects to its executed code, `S` returns `Allow`, its state IS
/// committed, and the objection is reported only. A later original is therefore simulated on
/// the CORRECT base state including the prefix.
#[test]
fn synthetic_ban_prefix_keeps_allow_and_commits_even_when_objected() {
    let prefix = format!("0x{}", "55".repeat(32));
    let original = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY), deposit_request(&original, APPROVED_IMPL)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&prefix],
        Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let s = &run.results[0];
    assert_eq!(s.verdict, Verdict::Allow, "a synthetic prefix's verdict never changes");
    assert_eq!(
        s.callstack_identification,
        Some(CallstackIdentification::Objected),
        "its identification is reported, not acted on"
    );
    assert_eq!(
        run.proxy_slot0,
        U256::from(2u64),
        "the prefix's state IS committed, so later originals see the correct base state"
    );
    assert_eq!(run.results[1].verdict, Verdict::Allow);
}

/// the same with the prefix's capture failing: `S` is still `Allow` and still
/// committed, reported `not_identified`.
#[test]
fn synthetic_ban_prefix_with_failed_capture_still_allows_and_commits() {
    let prefix = format!("0x{}", "55".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&prefix],
        Some([(prefix.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
        seeded_db(UNAPPROVED_IMPL, 1_000),
    );
    let s = only(&run);
    assert_eq!(s.verdict, Verdict::Allow);
    assert_eq!(s.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::from(2u64), "its commit is never withheld");
}

/// an `Original` in the SAME request still gets `Deny` priority, proving the two
/// classes are handled differently within one batch rather than by a global switch.
#[test]
fn both_classes_are_handled_differently_in_one_request() {
    let prefix = format!("0x{}", "55".repeat(32));
    let original = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY), deposit_request(&original, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&prefix, &original],
        Some(
            [
                (prefix.clone(), KnownAllowedKind::SyntheticBan),
                (original.clone(), KnownAllowedKind::Original),
            ]
            .into_iter()
            .collect(),
        ),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(run.results[0].verdict, Verdict::Allow, "synthetic prefix");
    assert_eq!(run.results[1].verdict, Verdict::Deny, "original");
}

// ---------------------------------------------------------------- batch behaviour ---------

#[test]
fn an_unlabelled_ban_prefix_is_committed_and_allowed_even_when_objected_or_unadjudicable() {
    in_isolated_process(
        module_path!(),
        "an_unlabelled_ban_prefix_is_committed_and_allowed_even_when_objected_or_unadjudicable",
        || {
            let prefix = golden_ban_to(PROXY);
            let (run, counters) = counters_during(|| {
                run_batch(
                    std::slice::from_ref(&prefix),
                    vec![proxy_impl_allowlist_rule()],
                    1,
                    &[&prefix.source_hash],
                    None,
                    seeded_db(UNAPPROVED_IMPL, 1),
                )
            });
            let result = only(&run);
            assert_eq!(result.verdict, Verdict::Allow);
            assert_eq!(result.callstack_identification, Some(CallstackIdentification::Objected));
            assert_eq!(run.proxy_slot0, U256::from(2u64), "the prefix's state is committed");
            assert_eq!(
                counter(&counters, "rcs_filter.synthetic_ban_identification.objected_total"),
                1,
                "{counters:?}"
            );
            assert_eq!(
                counter(
                    &counters,
                    "rcs_filter.known_allowed_resolution.inferred_synthetic_ban_total"
                ),
                1,
                "{counters:?}"
            );

            // Unadjudicable: the request-level gate does not catch an inferred ban either.
            let run = run_batch(
                std::slice::from_ref(&prefix),
                vec![proxy_impl_allowlist_rule()],
                1,
                &[&prefix.source_hash],
                None,
                seeded_db(UNAPPROVED_IMPL, 1_000),
            );
            let result = only(&run);
            assert_eq!(result.verdict, Verdict::Allow);
            assert_eq!(
                result.callstack_identification,
                Some(CallstackIdentification::NotIdentified)
            );
            assert_eq!(run.proxy_slot0, U256::from(2u64), "its commit is never withheld");
        },
    );
}

#[test]
fn a_clean_unlabelled_original_is_allowed_and_committed_and_never_audited() {
    let source = format!("0x{}", "33".repeat(32));
    let approved = keccak256(logging_impl_code());
    let rules =
        vec![proxy_impl_allowlist_rule_approving(&[approved]), audit_on_proxy_transfer_rule()];
    let run =
        run_batch(&[deposit_request(&source, PROXY)], rules, 1, &[&source], None, logging_db());
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow, "the quota rule would match, and is not re-run");
    assert!(result.actions.is_none());
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Clean));
    assert_eq!(run.depositor_nonce, 1, "its state is committed");
}

#[test]
fn an_unadjudicable_inferred_original_does_not_stop_the_prefix_from_committing() {
    let prefix = golden_ban_to(PROXY);
    let original = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[prefix.clone(), deposit_request(&original, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&prefix.source_hash, &original],
        None,
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let ban = &run.results[0];
    let original = &run.results[1];
    assert_eq!(ban.verdict, Verdict::Allow);
    assert_eq!(original.verdict, Verdict::Malformed);
    assert_eq!(original.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.depositor_nonce, 1, "exactly one commit: the prefix's");
}

#[test]
fn a_label_map_wins_over_inference_in_both_directions() {
    // A ban-shaped entry labelled `original` is adjudicated as an original.
    let prefix = golden_ban_to(PROXY);
    let run = run_batch(
        std::slice::from_ref(&prefix),
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&prefix.source_hash],
        Some([(prefix.source_hash.clone(), KnownAllowedKind::Original)].into_iter().collect()),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(only(&run).verdict, Verdict::Deny);
    assert_eq!(run.proxy_slot0, U256::ZERO);

    // A non-ban entry labelled `synthetic_ban` keeps the unconditional allow and commit: the
    // explicit opt-out for a caller that wants the old behaviour for an entry.
    let source = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(only(&run).verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::from(2u64));
}

// ------------------------------------------------------------ the trust boundary ---------

/// A request author can build a matching entry for calldata and a target of its choice. This is
/// a content binding, not source authentication, so such an entry IS classified as a ban — and
/// what it gets is exactly what every unlabelled entry got before: `allow` and a commit, with the
/// objection reported. No caller gains a capability it did not already have.
#[test]
fn a_self_built_matching_entry_gets_exactly_the_previous_unlabelled_result() {
    in_isolated_process(
        module_path!(),
        "a_self_built_matching_entry_gets_exactly_the_previous_unlabelled_result",
        || {
            let entry = self_built_ban(&[B256::repeat_byte(0x99)], PROXY);
            assert!(verify_synthetic_ban(
                entry.source_hash.parse().expect("hash"),
                &hex::decode(&entry.data).expect("hex")
            ));
            let (run, counters) = counters_during(|| {
                run_batch(
                    std::slice::from_ref(&entry),
                    vec![proxy_impl_allowlist_rule()],
                    1,
                    &[&entry.source_hash],
                    None,
                    seeded_db(UNAPPROVED_IMPL, 1),
                )
            });
            let result = only(&run);
            // The previous behaviour for an unlabelled entry executing objected code: `allow`, no
            // actions, the objection reported, and the unapproved implementation's marker committed.
            assert_eq!(result.verdict, Verdict::Allow);
            assert!(result.actions.is_none());
            assert_eq!(result.callstack_identification, Some(CallstackIdentification::Objected));
            assert_eq!(run.proxy_slot0, U256::from(2u64));
            assert_eq!(
                counter(&counters, "rcs_filter.synthetic_ban_identification.objected_total"),
                1,
                "{counters:?}"
            );
        },
    );
}

/// The same canonical ban calldata carried by a deposit whose `source_hash` is derived from an
/// L1 log, the way RCS derives every user deposit's: not a ban. A depositor's L1-derived hash
/// cannot opt into the ban class; only the derivation decides.
#[test]
fn an_l1_derived_source_hash_with_ban_calldata_is_adjudicated_as_an_original() {
    let control = golden_ban_to(PROXY);
    let l1_derived =
        DepositTxRequest { source_hash: golden_l1_user_deposit_source_hash(), ..control.clone() };
    assert_eq!(inferred(&l1_derived), KnownAllowedKind::Original);

    let run = run_batch(
        std::slice::from_ref(&l1_derived),
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&l1_derived.source_hash],
        None,
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(only(&run).verdict, Verdict::Deny);
    assert_eq!(run.proxy_slot0, U256::ZERO, "nothing committed");

    let run = run_batch(
        std::slice::from_ref(&control),
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&control.source_hash],
        None,
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(only(&run).verdict, Verdict::Allow, "control: the ban-derived hash is a ban");
    assert_eq!(run.proxy_slot0, U256::from(2u64));
}

/// The behaviour change for any unlabelled caller other than RCS: a non-ban entry objected to on
/// the call-frame dimension is now `deny` with nothing committed. The previous behaviour for the
/// same entry was `allow`, committed, with the objection reported only.
#[test]
fn a_non_ban_unlabelled_entry_is_now_denied_and_labelling_it_restores_the_old_result() {
    let source = format!("0x{}", "77".repeat(32));
    let entry = deposit_request(&source, PROXY);
    let run = run_batch(
        std::slice::from_ref(&entry),
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        None,
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(only(&run).verdict, Verdict::Deny);
    assert_eq!(run.proxy_slot0, U256::ZERO);

    let run = run_batch(
        &[entry],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[&source],
        Some([(source.clone(), KnownAllowedKind::SyntheticBan)].into_iter().collect()),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::Objected));
    assert_eq!(run.proxy_slot0, U256::from(2u64));
}

#[test]
fn an_entry_outside_known_allowed_is_ordinary_even_when_it_verifies_as_a_ban() {
    let prefix = golden_ban_to(PROXY);
    let run = run_batch(
        &[prefix],
        vec![proxy_impl_allowlist_rule()],
        1,
        &[],
        None,
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(only(&run).verdict, Verdict::Deny, "only known_allowed entries are classified");
    assert_eq!(run.proxy_slot0, U256::ZERO);
}
