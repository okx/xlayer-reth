//! Classification of unlabelled `known_allowed` entries by the synthetic-ban content binding.
//!
//! These tests prove the classification rule and that it depends on the entry's own
//! `source_hash` derivation. They do not, and cannot, prove that a caller derives its
//! originals' hashes from real L1 logs; that is a property of the caller.

use super::*;
use crate::audit::synthetic_ban::tests::{golden_bans, golden_l1_user_deposit_source_hash};
use crate::audit::synthetic_ban::{
    verify_synthetic_ban, BATCH_ADD_TO_BLACKLIST_SIGNATURE, SYNTHETIC_BAN_SOURCE_HASH_DOMAIN,
};
use alloy_primitives::{hex, Keccak256};

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

/// Reads back every `rcs_filter.*` counter the closure registered on a local recorder.
fn counters_during<T>(run: impl FnOnce() -> T) -> (T, Vec<(String, u64)>) {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let value = {
        let _guard = metrics::set_default_local_recorder(&recorder);
        run()
    };
    let mut counters: Vec<(String, u64)> = snapshotter
        .snapshot()
        .into_vec()
        .into_iter()
        .filter_map(|(key, _, _, value)| match value {
            DebugValue::Counter(count) => Some((key.key().name().to_string(), count)),
            _ => None,
        })
        .collect();
    counters.sort();
    (value, counters)
}

fn counter(counters: &[(String, u64)], name: &str) -> u64 {
    counters.iter().find(|(key, _)| key == name).map_or(0, |(_, count)| *count)
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
        // The handler records a failed build as `None` and never executes it, so the result is
        // the non-verdict whatever the entry's class would have been.
        let results = assemble_results(std::slice::from_ref(&req), &[None], &mut []);
        assert_eq!(results[0].verdict, Verdict::Malformed, "{case}");
        assert_eq!(results[0].source_hash, req.source_hash, "{case}");
    }
}

#[test]
fn a_transaction_that_is_not_a_deposit_is_never_a_ban() {
    use alloy_consensus::{SignableTransaction, TxLegacy};
    let ban = golden_bans().into_iter().find(|ban| ban.n == 1).expect("vector");
    let legacy =
        TxLegacy { input: hex::decode(&ban.data).expect("hex").into(), ..Default::default() }
            .into_signed(alloy_primitives::Signature::new(U256::ZERO, U256::ZERO, false));
    let tx = Recovered::new_unchecked(
        OpTransactionSigned::from(op_alloy_consensus::OpTxEnvelope::Legacy(legacy)),
        DEPOSITOR,
    );
    assert_eq!(inferred_known_allowed_kind(&tx), KnownAllowedKind::Original);
}

// ---------------------------------------------------------------- batch behaviour ---------

#[test]
fn an_unlabelled_ban_prefix_is_committed_and_allowed_even_when_objected_or_unadjudicable() {
    let prefix = golden_ban_to(PROXY);
    let (run, counters) = counters_during(|| {
        run_batch(
            std::slice::from_ref(&prefix),
            vec![proxy_impl_allowlist_rule()],
            1,
            &[&prefix.source_hash],
            None,
            generous(),
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
        counter(&counters, "rcs_filter.known_allowed_resolution.inferred_synthetic_ban_total"),
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
        AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
        seeded_db(UNAPPROVED_IMPL, 2),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(result.callstack_identification, Some(CallstackIdentification::NotIdentified));
    assert_eq!(run.proxy_slot0, U256::from(2u64), "its commit is never withheld");
}

#[test]
fn a_clean_unlabelled_original_is_allowed_and_committed_and_never_audited() {
    let source = format!("0x{}", "33".repeat(32));
    let approved = keccak256(logging_impl_code());
    let rules =
        vec![proxy_impl_allowlist_rule_approving(&[approved]), audit_on_proxy_transfer_rule()];
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        rules,
        1,
        &[&source],
        None,
        generous(),
        logging_db(),
    );
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
        AuditCaptureConfig { max_callstack_observations_per_tx: 1 },
        seeded_db(APPROVED_IMPL, 2),
    );
    let ban = run.results[0].as_ref().expect("executed");
    let original = run.results[1].as_ref().expect("executed");
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
        generous(),
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
        generous(),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(only(&run).verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::from(2u64));
}

/// One mixed batch: a ban prefix, two approved originals and an ordinary deposit.
fn mixed_batch() -> (Vec<DepositTxRequest>, Vec<String>) {
    let prefix = golden_ban_to(PROXY);
    let first = format!("0x{}", "66".repeat(32));
    let second = format!("0x{}", "67".repeat(32));
    let ordinary = format!("0x{}", "68".repeat(32));
    let known = vec![prefix.source_hash.clone(), first.clone(), second.clone()];
    let txs = vec![
        prefix,
        deposit_request(&first, PROXY),
        deposit_request(&second, PROXY),
        deposit_request(&ordinary, PROXY),
    ];
    (txs, known)
}

// The metric handles are registered once per process, so each recorder-reading test reads one
// batch only, and these assertions need a process per test (the repository's nextest runner).

#[test]
fn resolution_metrics_count_each_inferred_entry_once() {
    let (txs, known) = mixed_batch();
    let known: Vec<&str> = known.iter().map(String::as_str).collect();
    let (_, counters) = counters_during(|| {
        run_batch(
            &txs,
            vec![proxy_impl_allowlist_rule()],
            1,
            &known,
            None,
            generous(),
            seeded_db(APPROVED_IMPL, 1),
        )
    });
    let resolution =
        |name| counter(&counters, &format!("rcs_filter.known_allowed_resolution.{name}"));
    assert_eq!(resolution("labelled_total"), 0, "{counters:?}");
    assert_eq!(resolution("inferred_synthetic_ban_total"), 1, "{counters:?}");
    assert_eq!(resolution("inferred_original_total"), 2, "{counters:?}");
}

#[test]
fn resolution_metrics_count_each_labelled_entry_once() {
    let (txs, known_owned) = mixed_batch();
    let known: Vec<&str> = known_owned.iter().map(String::as_str).collect();
    let kinds: BTreeMap<String, KnownAllowedKind> = [
        (known_owned[0].clone(), KnownAllowedKind::SyntheticBan),
        (known_owned[1].clone(), KnownAllowedKind::Original),
        (known_owned[2].clone(), KnownAllowedKind::Original),
    ]
    .into_iter()
    .collect();
    let (_, counters) = counters_during(|| {
        run_batch(
            &txs,
            vec![proxy_impl_allowlist_rule()],
            1,
            &known,
            Some(kinds),
            generous(),
            seeded_db(APPROVED_IMPL, 1),
        )
    });
    let resolution =
        |name| counter(&counters, &format!("rcs_filter.known_allowed_resolution.{name}"));
    assert_eq!(resolution("labelled_total"), 3, "{counters:?}");
    assert_eq!(resolution("inferred_synthetic_ban_total"), 0, "{counters:?}");
    assert_eq!(resolution("inferred_original_total"), 0, "{counters:?}");
}

// ------------------------------------------------------------ the trust boundary ---------

/// A request author can build a matching entry for calldata and a target of its choice. This is
/// a content binding, not source authentication, so such an entry IS classified as a ban — and
/// what it gets is exactly what every unlabelled entry got before: `allow` and a commit, with the
/// objection reported. No caller gains a capability it did not already have.
#[test]
fn a_self_built_matching_entry_gets_exactly_the_previous_unlabelled_result() {
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
            generous(),
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
        generous(),
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
        generous(),
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
        generous(),
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
        generous(),
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
        generous(),
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(only(&run).verdict, Verdict::Deny, "only known_allowed entries are classified");
    assert_eq!(run.proxy_slot0, U256::ZERO);
}
