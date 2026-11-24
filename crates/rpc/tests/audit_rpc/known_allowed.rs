//! `known_allowed` behaviour on the audit RPC: an entry in the set is committed and allowed
//! without rule (re-)classification, whatever its execution or capture outcome, while every
//! other entry follows the ordinary path.

use alloy_primitives::{hex, keccak256, Address, Keccak256, B256, U256};
use xlayer_rpc::audit::deposit::{build_deposit_tx, DepositTxRequest};
use xlayer_rpc::audit::verdict::Verdict;

use crate::harness::*;

/// RCS's v1 synthetic-ban derivation, reproduced here so the fixture is literally the calldata
/// RCS sends for a replay prefix. The class concept no longer exists on this path — the fixture
/// only has to be an entry whose bytes look special while its membership does not.
const SYNTHETIC_BAN_SOURCE_HASH_DOMAIN: &[u8] = b"xlayer-rcs:blacklist-simulation:v1";
const BATCH_ADD_TO_BLACKLIST_SIGNATURE: &str = "batchAddToBlacklist(bytes32[])";

/// A ban-shaped deposit: `batchAddToBlacklist([element])` with the matching derived source hash.
fn ban_shaped_entry(to: Address) -> DepositTxRequest {
    let element = B256::repeat_byte(0x99);
    let mut data = keccak256(BATCH_ADD_TO_BLACKLIST_SIGNATURE.as_bytes())[..4].to_vec();
    data.extend_from_slice(&U256::from(32u64).to_be_bytes::<32>());
    data.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
    data.extend_from_slice(element.as_slice());
    let mut hasher = Keccak256::new();
    hasher.update(SYNTHETIC_BAN_SOURCE_HASH_DOMAIN);
    hasher.update(element.as_slice());
    DepositTxRequest {
        data: format!("0x{}", hex::encode(data)),
        ..deposit_request(&format!("{:#x}", hasher.finalize()), to)
    }
}

#[test]
fn undecodable_data_or_source_hash_is_malformed_and_never_executed_or_classified() {
    let entry = ban_shaped_entry(PROXY);
    let odd = DepositTxRequest { data: format!("{}0", entry.data), ..entry.clone() };
    let non_hex = DepositTxRequest { data: format!("{}zz", entry.data), ..entry.clone() };
    let bad_hash = DepositTxRequest { source_hash: "0x1234".to_string(), ..entry.clone() };
    for (case, req) in [("odd length", odd), ("non-hex", non_hex), ("short source_hash", bad_hash)]
    {
        assert!(build_deposit_tx(&req).is_err(), "{case}: the build fails");
        // The batch records a failed build as `None` and never executes it, so the result is
        // the non-verdict.
        let run = run_batch(
            std::slice::from_ref(&req),
            vec![proxy_impl_allowlist_rule()],
            &[],
            seeded_db(APPROVED_IMPL, 1),
        );
        let result = only(&run);
        assert_eq!(result.verdict, Verdict::Malformed, "{case}");
        assert_eq!(result.source_hash, req.source_hash, "{case}");
    }
}

// ---------------------------------------------------------------- class behaviour ---------

/// a `known_allowed` entry executing unapproved implementation code is still `Allow`, and its
/// state IS committed: the entry is never re-checked for codeHash.
#[test]
fn known_allowed_entry_running_unapproved_code_is_allowed_and_committed() {
    let source = format!("0x{}", "33".repeat(32));
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[&source],
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::from(2u64), "the known_allowed commit is never withheld");
    assert_eq!(run.depositor_nonce, 1);
}

/// a `known_allowed` entry whose capture fails is likewise `Allow` and committed.
#[test]
fn known_allowed_entry_with_failed_capture_is_allowed_and_committed() {
    let source = format!("0x{}", "33".repeat(32));

    // Unavailable: the observation budget is exhausted.
    let run = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[&source],
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::from(1u64), "the known_allowed commit is never withheld");
    assert_eq!(run.depositor_nonce, 1);

    // Available under a FOREIGN stamp: the same outcome.
    let raw_rules = vec![proxy_impl_allowlist_rule()];
    let run = run_batch_stamped(
        &[deposit_request(&source, PROXY)],
        raw_rules.clone(),
        &[&source],
        seeded_db(APPROVED_IMPL, 1),
        Some(foreign_stamp(&raw_rules)),
    );
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow);
    assert_eq!(run.proxy_slot0, U256::from(1u64));
    assert_eq!(run.depositor_nonce, 1);
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
        &[],
        seeded_db(APPROVED_IMPL, 1_000),
    );
    let result = only(&run);
    assert_eq!(
        result.verdict,
        Verdict::Malformed,
        "an unidentifiable execution is never reported Allow, whichever rules were skipped"
    );
    assert_eq!(run.proxy_slot0, U256::ZERO, "a non-verdict must not commit state");

    // Counter-check: the SAME request and the SAME skipped rule under a sufficient budget is
    // adjudicated normally and committed, so the assertion above is about the unavailable
    // observation set and not about the origin narrowing by itself.
    let identified = run_batch(
        &[deposit_request(&source, PROXY)],
        vec![proxy_impl_allowlist_rule_from_origin(OTHER_ORIGIN)],
        &[],
        seeded_db(APPROVED_IMPL, 1),
    );
    assert_eq!(only(&identified).verdict, Verdict::Allow);
    assert_eq!(identified.proxy_slot0, U256::from(1u64));
}

/// A committed `known_allowed` prefix is the base state the NEXT entry runs on: the prefix's
/// state lands in the overlay even though a rule would have objected to its code, and the later
/// entry is allowed on that state.
#[test]
fn a_known_allowed_prefix_commits_and_the_next_entry_runs_on_its_state() {
    let prefix = format!("0x{}", "55".repeat(32));
    let original = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY), deposit_request(&original, APPROVED_IMPL)],
        vec![proxy_impl_allowlist_rule()],
        &[&prefix],
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    let s = &run.results[0];
    assert_eq!(s.verdict, Verdict::Allow, "a known_allowed prefix's verdict never changes");
    assert_eq!(
        run.proxy_slot0,
        U256::from(2u64),
        "the prefix's state IS committed, so later entries see the correct base state"
    );
    assert_eq!(run.results[1].verdict, Verdict::Allow);
    assert_eq!(run.depositor_nonce, 2, "both entries committed their state");
}

/// a `known_allowed` entry and an ordinary entry in the SAME request are handled independently:
/// the exception is per entry, never a request-wide switch.
#[test]
fn a_known_allowed_entry_and_an_ordinary_entry_in_one_request_are_handled_independently() {
    let prefix = format!("0x{}", "55".repeat(32));
    let ordinary = format!("0x{}", "66".repeat(32));
    let run = run_batch(
        &[deposit_request(&prefix, PROXY), deposit_request(&ordinary, PROXY)],
        vec![proxy_impl_allowlist_rule()],
        &[&prefix],
        seeded_db(UNAPPROVED_IMPL, 1),
    );
    assert_eq!(run.results[0].verdict, Verdict::Allow, "known_allowed entry");
    assert_eq!(run.results[1].verdict, Verdict::Deny, "ordinary entry");
    assert_eq!(run.proxy_slot0, U256::from(2u64), "only the known_allowed entry committed");
    assert_eq!(run.depositor_nonce, 1);
}

// ---------------------------------------------------------------- batch behaviour ---------

/// a `known_allowed` entry is never audited: the quota rule that WOULD match its emitted log is
/// not re-run, and the entry is committed with no actions.
#[test]
fn a_known_allowed_entry_is_never_audited() {
    let source = format!("0x{}", "33".repeat(32));
    let approved = keccak256(logging_impl_code());
    let rules =
        vec![proxy_impl_allowlist_rule_approving(&[approved]), audit_on_proxy_transfer_rule()];
    let run = run_batch(&[deposit_request(&source, PROXY)], rules, &[&source], logging_db());
    let result = only(&run);
    assert_eq!(result.verdict, Verdict::Allow, "the quota rule would match, and is not re-run");
    assert!(result.actions.is_none());
    assert_eq!(run.depositor_nonce, 1, "its state is committed");
}

#[test]
fn an_entry_outside_known_allowed_is_ordinary_even_when_it_verifies_as_a_ban() {
    // The entry carries RCS's ban calldata and its derived source hash, but the request does not
    // list it: membership alone decides, so it is adjudicated on the ordinary path and denied.
    let prefix = ban_shaped_entry(PROXY);
    let run =
        run_batch(&[prefix], vec![proxy_impl_allowlist_rule()], &[], seeded_db(UNAPPROVED_IMPL, 1));
    assert_eq!(only(&run).verdict, Verdict::Deny, "only known_allowed entries are exempt");
    assert_eq!(run.proxy_slot0, U256::ZERO);
}
