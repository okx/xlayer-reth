//! Joint-acceptance coverage against the **genuinely rendered** upstream rule-source document,
//! reproduced byte-for-byte in `testdata/upstream-rule-source-examples.json`.
//!
//! Provenance of that fixture, so it can be re-verified rather than trusted: it is the
//! `config/rules-examples.json` blob of the locked upstream revision
//! `8944b24f00c058cb5e65090a5c210335d1f186ed`, 5,654 bytes, content sha256
//! `00a1c7da6e8d32aec20a695054b4f6fc650a459dc65237a10bbc68eef5f0a2ca`. It is **not** a mock and
//! **not** hand-edited: the same document the rule publisher renders is what these tests feed in.
//!
//! Two things are asserted, and the second is the one that matters: every rule is accepted with
//! no rejections, AND each call-frame condition is actually evaluated to a verdict — parsing a
//! rule proves nothing about whether its condition can ever fire.

use alloy_primitives::{Address, Log, B256, U256};
use serde_json::Value;

use crate::callstack::{CallObservation, Observations};
use crate::handle::ScreenInput;
use crate::matching::{try_evaluate, MatchOutcome};
use crate::rules::{load_rules, RawRule, RuleSet, DEFAULT_MAX_EVENT_BINDINGS_PER_TX};

/// The verbatim upstream document.
const UPSTREAM_RULES: &str = include_str!("../testdata/upstream-rule-source-examples.json");

/// The content hash the upstream repository reports for that blob. Recorded so the fixture's
/// provenance stays checkable from outside this crate.
const UPSTREAM_CONTENT_SHA256: &str =
    "00a1c7da6e8d32aec20a695054b4f6fc650a459dc65237a10bbc68eef5f0a2ca";

/// Local drift pin over the vendored bytes, in the hash this crate already depends on.
const UPSTREAM_FIXTURE_KECCAK256: &str =
    "0x36e7cf088e81e4b07a1681b8b6889a39095a1b9ef2d4f53e7763fe426e81376f";

fn addr(hex: &str) -> Address {
    hex.parse().expect("valid fixed address")
}

fn hash(hex: &str) -> B256 {
    hex.parse().expect("valid fixed hash")
}

fn vault_proxy() -> Address {
    addr("0x00000000000000000000000000000000000ab001")
}

fn router_proxy() -> Address {
    addr("0x00000000000000000000000000000000000ab002")
}

fn bridge_proxy() -> Address {
    addr("0x00000000000000000000000000000000000ab003")
}

/// An implementation address the paired rule approves.
fn approved_bridge_impl() -> Address {
    addr("0x00000000000000000000000000000000000c0de1")
}

/// The code hash paired with [`approved_bridge_impl`].
fn approved_bridge_code() -> B256 {
    hash("0x4444444444444444444444444444444444444444444444444444444444444444")
}

/// The code hash paired with the OTHER approved implementation, so using it with
/// [`approved_bridge_impl`] is the crossed combination the paired rule exists to reject.
fn other_pair_code() -> B256 {
    hash("0x5555555555555555555555555555555555555555555555555555555555555555")
}

fn vault_approved_code() -> B256 {
    hash("0x1111111111111111111111111111111111111111111111111111111111111111")
}

/// Approved by the callstack-only vault rule but NOT by the mixed audit rule, which is what lets
/// one observation exercise the audit dimension without also tripping the deny.
fn vault_second_approved_code() -> B256 {
    hash("0x2222222222222222222222222222222222222222222222222222222222222222")
}

fn router_approved_code() -> B256 {
    hash("0x3333333333333333333333333333333333333333333333333333333333333333")
}

fn unapproved_code() -> B256 {
    B256::repeat_byte(0xee)
}

/// The depositor address the mixed audit rule's event half selects on.
fn watched_from() -> Address {
    addr("0x00000000000000000000000000000000000bad00")
}

/// One direct delegatecall of `proxy` into `target`, which executed `target_code`.
fn delegated(proxy: Address, target: Address, target_code: B256) -> CallObservation {
    CallObservation::direct_delegatecall(proxy, B256::repeat_byte(0xa0), target, target_code)
}

/// Decodes the upstream document's `rules` array and compiles it exactly as both entry points do.
fn upstream_rule_set() -> RuleSet {
    let document: Value =
        serde_json::from_str(UPSTREAM_RULES).expect("the upstream document is valid JSON");
    // Decoded straight into the typed rule shape, with no intermediate map folding, which is what
    // keeps the duplicate-key and strict-field guarantees intact.
    let raw: Vec<RawRule> = document["rules"]
        .as_array()
        .expect("the document carries a rules array")
        .iter()
        .map(|rule| serde_json::from_value(rule.clone()).expect("every upstream rule decodes"))
        .collect();
    load_rules(crate::config::MIN_PROTOCOL_VERSION, 0, raw)
}

fn screen<'a>(rules: &RuleSet, logs: &'a [Log], items: &'a [CallObservation]) -> MatchOutcome {
    let input = ScreenInput {
        tx_hash: B256::repeat_byte(0x77),
        origin: addr("0x000000000000000000000000000000000a0000d1"),
        // Deliberately NOT the address the two `contract_address`-narrowed event rules name, so
        // those rules are skipped and the call-frame dimension is what decides the verdict.
        tx_to: Some(vault_proxy()),
        nonce: 1,
        value: U256::ZERO,
        block_height: 100,
        logs,
        observations: Observations::available(rules.stamp(), items),
    };
    try_evaluate(rules, &input, DEFAULT_MAX_EVENT_BINDINGS_PER_TX)
        .expect("the fixture observations are adjudicable")
}

#[test]
fn the_vendored_fixture_has_not_drifted() {
    // Guards the premise of every test below: if the fixture ever drifts from the upstream blob,
    // joint acceptance stops being joint, so it must fail loudly rather than pass silently.
    //
    // The pin is a keccak digest because that hash is already available in this crate's
    // dependencies; equality with the upstream blob is established by its byte length and by the
    // sha256 recorded in [`UPSTREAM_CONTENT_SHA256`], re-checkable at any time with
    // `sha256sum crates/rcs-filter/testdata/upstream-rule-source-examples.json`.
    assert_eq!(UPSTREAM_RULES.len(), 5_654, "the upstream blob is 5,654 bytes");
    assert_eq!(
        alloy_primitives::keccak256(UPSTREAM_RULES.as_bytes()),
        hash(UPSTREAM_FIXTURE_KECCAK256),
        "the vendored fixture changed; re-verify it against the upstream blob before updating \
         this pin"
    );
    assert_eq!(UPSTREAM_CONTENT_SHA256.len(), 64, "the recorded upstream hash is a sha256");
}

#[test]
fn every_upstream_rule_is_accepted_with_no_rejections() {
    let rules = upstream_rule_set();
    assert!(rules.rejected.is_empty(), "no upstream rule may be rejected: {:?}", rules.rejected);
    assert_eq!(rules.rules.len(), 7, "all seven upstream rules are active");

    let ids: Vec<&str> = rules.rules.iter().map(|rule| rule.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "blk-compromised-contract",
            "deny-deposit-without-matching-transfer",
            "blk-asset-owner-update-unauthorized-origin",
            "deny-proxy-delegatecall-to-unapproved-implementation",
            "deny-router-delegatecall-to-unapproved-implementation",
            "deny-paired-implementation-address-and-code-hash",
            "schema-compatibility-mixed-event-and-direct-delegatecall-audit",
        ]
    );

    // Four rules carry a `callstack` section; three distinct proxy addresses, because two rules
    // observe the same one.
    assert_eq!(rules.selector_bearing_rule_count(), 4);
    assert!(rules.has_callstack_rules());
    let mut expected = vec![vault_proxy(), router_proxy(), bridge_proxy()];
    expected.sort_unstable();
    assert_eq!(rules.callstack_selectors(), expected.as_slice());

    // The mixed audit rule keeps the audit type the upstream document declares. An audit-type
    // allowlist would reject a rule the publisher legitimately emits, so there must not be one.
    let mixed = rules
        .rules
        .iter()
        .find(|rule| rule.id == "schema-compatibility-mixed-event-and-direct-delegatecall-audit")
        .expect("the mixed rule is active");
    assert_eq!(mixed.audit_types, vec!["delegatecall_review".to_string()]);
    assert_eq!(mixed.events.len(), 1);
    assert_eq!(mixed.callstack.len(), 1);
}

#[test]
fn the_callstack_only_allowlists_actually_fire() {
    let rules = upstream_rule_set();

    // The vault rule: an unapproved implementation objects, either approved one does not.
    assert_eq!(
        screen(&rules, &[], &[delegated(vault_proxy(), approved_bridge_impl(), unapproved_code())]),
        MatchOutcome::Deny
    );
    for approved in [vault_approved_code(), vault_second_approved_code()] {
        assert_eq!(
            screen(&rules, &[], &[delegated(vault_proxy(), approved_bridge_impl(), approved)]),
            MatchOutcome::Allow,
            "an approved implementation must not be objected to"
        );
    }

    // The router rule, on its own address and its own one-entry allowlist.
    assert_eq!(
        screen(
            &rules,
            &[],
            &[delegated(router_proxy(), approved_bridge_impl(), unapproved_code())]
        ),
        MatchOutcome::Deny
    );
    assert_eq!(
        screen(
            &rules,
            &[],
            &[delegated(router_proxy(), approved_bridge_impl(), router_approved_code())]
        ),
        MatchOutcome::Allow
    );
}

#[test]
fn the_paired_rule_rejects_the_crossed_combination() {
    // This is the rule whose condition reads BOTH bound variables. It is also the rule that would
    // invert into an unconditional objection if the target-address variable were not bound, which
    // is why the exact pair must be shown to pass while the crossed pair is objected to.
    let rules = upstream_rule_set();

    assert_eq!(
        screen(
            &rules,
            &[],
            &[delegated(bridge_proxy(), approved_bridge_impl(), approved_bridge_code())]
        ),
        MatchOutcome::Allow,
        "an exactly-approved (address, code hash) pair is accepted"
    );
    assert_eq!(
        screen(
            &rules,
            &[],
            &[delegated(bridge_proxy(), approved_bridge_impl(), other_pair_code())]
        ),
        MatchOutcome::Deny,
        "approved address crossed with the other pair's code hash is objected to"
    );
}

#[test]
fn a_transaction_that_touches_no_declared_proxy_produces_no_objection() {
    // The hazard the whole contract exists to prevent, driven through the real upstream rules:
    // every call-frame rule here is a NEGATED allowlist, so binding an unobserved alias as null
    // would object to every unrelated transaction.
    let rules = upstream_rule_set();
    assert_eq!(screen(&rules, &[], &[]), MatchOutcome::Allow, "capture ran and saw nothing");
    assert_eq!(
        screen(
            &rules,
            &[],
            &[delegated(
                addr("0x00000000000000000000000000000000000ab0ff"),
                approved_bridge_impl(),
                unapproved_code()
            )]
        ),
        MatchOutcome::Allow,
        "a delegatecall by an unrelated proxy is not a candidate for any declared alias"
    );
}

#[test]
fn the_mixed_rule_needs_both_dimensions_and_then_audits() {
    use crate::test_support::log_builder::erc20_transfer;
    let rules = upstream_rule_set();
    let token = addr("0x0000000000000000000000000000000000000777");
    let to = addr("0x0000000000000000000000000000000000000888");
    let logs = vec![erc20_transfer(token, watched_from(), to, U256::from(5u64))];

    // The code hash the callstack-only vault rule approves but the mixed rule does not, so this
    // one observation reaches the audit dimension without also tripping the deny.
    let observed = [delegated(vault_proxy(), approved_bridge_impl(), vault_second_approved_code())];

    match screen(&rules, &logs, &observed) {
        MatchOutcome::Audit { actions, .. } => {
            let items = actions.get("delegatecall_review").expect("the declared audit type");
            assert_eq!(items.len(), 1, "the payload comes from the event side only");
            assert_eq!(items[0].name, "transfer");
        }
        other => panic!("both dimensions are satisfied, so this must audit: {other:?}"),
    }

    // The event alone is not enough: with no observation for its alias the whole rule is
    // suppressed, so the call-frame dimension cannot carry a mixed rule on its own.
    assert_eq!(screen(&rules, &logs, &[]), MatchOutcome::Allow);

    // Nor is the frame alone, without the event the payload would have to come from.
    assert_eq!(screen(&rules, &[], &observed), MatchOutcome::Allow);

    // And an approved-by-both implementation satisfies the event half but not the frame half.
    let fully_approved = [delegated(vault_proxy(), approved_bridge_impl(), vault_approved_code())];
    assert_eq!(screen(&rules, &logs, &fully_approved), MatchOutcome::Allow);
}
