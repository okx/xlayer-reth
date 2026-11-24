//! Rule-installation acceptance coverage: the strict wire schema's decode and compile gates, the
//! version gate, and last-known-good retention across a refused snapshot — every case driven
//! through the production `load_and_install` entry point.

use std::sync::Arc;

use rcs_filter::client::RcsClient;
use rcs_filter::config::FilterConfig;
use rcs_filter::rules::RuleSet;
use rcs_filter::test_seams;
use rcs_filter::test_support::{golden, MockRcsClient, TestClock};
use serde_json::json;

const START: u64 = 1_751_000_000;

/// A rule `protocol_version` this build does not know. Derived from the supported set rather than
/// hard-coded, so adding a version cannot silently turn this into a supported-version test.
const UNSUPPORTED_PROTOCOL_VERSION: u32 =
    *rcs_filter::config::SUPPORTED_PROTOCOL_VERSIONS.last().unwrap() + 1;

/// The production worker state, not yet ready: the config, rule slot, clock and terminal-channel
/// capacity the in-crate worker fixtures build.
fn not_ready_shared() -> test_seams::WorkerShared {
    test_seams::worker_shared(
        FilterConfig { enabled: true, rcs_base_url: "http://mock".into(), ..Default::default() },
        RuleSet::default(),
        Arc::new(TestClock::new(START)),
        false,
        1024,
    )
}

/// the strict wire schema routes a typo into the existing all-or-nothing gate.
///
/// One typo'd rule among valid siblings makes `load_and_install` return `Err`, leaves
/// `install_rules` unreached, and keeps the previously installed set active; the very next poll
/// with a corrected snapshot installs — there is no sticky "rejected version" marker.
///
/// Bound to the real `load_and_install` entry point on purpose; a test-only helper would not
/// prove that the typo reaches the production gate.
#[tokio::test]
async fn it_undeclared_callstack_key_fails_the_decode_then_a_corrected_poll_recovers() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = not_ready_shared();
    assert!(test_seams::load_and_install(&shared, &client).await.unwrap());
    let previous = shared.rules_slot().read().unwrap().clone();
    assert_eq!(previous.rules.len(), 1);
    assert_eq!(previous.content_version, 1);

    // The retired selector form inside `callstack`, alongside a valid sibling rule. The
    // `callstack` section declares exactly one field, so this fails during the TYPED DECODE of
    // the whole response — before `load_rules` is ever called. There is therefore no per-rule
    // `(id, reason)` for it, and none is fabricated: the failure surfaces as the decode error,
    // naming the offending key.
    mock.set_rules_response_override(json!({
        "protocol_version": 1,
        "content_version": 2,
        "rules": [
            serde_json::from_str::<serde_json::Value>(golden::RULE_SCENARIO_B).unwrap(),
            json!({
                "id": "proxy-impl-allowlist",
                "event_abis": {},
                "callstack": { "entry": {
                    "address": "0x00000000000000000000000000000000000000aa",
                    "codeHash": "0x1111111111111111111111111111111111111111111111111111111111111111"
                } },
                "condition": true,
                "action": "deny"
            })
        ]
    }));
    let err = test_seams::load_and_install(&shared, &client)
        .await
        .expect_err("an undeclared callstack key must refuse the entire snapshot");
    match &err {
        rcs_filter::FilterError::Decode(reason) => {
            assert!(reason.contains("unknown field"), "{reason}");
            assert!(reason.contains("entry"), "the reason must name the offending key: {reason}");
            assert!(reason.contains("direct_delegatecalls"), "{reason}");
        }
        other => panic!("expected a decode failure, got {other:?}"),
    }
    let current = shared.rules_slot().read().unwrap().clone();
    assert!(
        Arc::ptr_eq(&previous, &current),
        "last-known-good must stay installed when the snapshot is refused"
    );
    assert_eq!(current.content_version, 1, "content_version must not advance on rejection");
    drop(current);

    // The same two rules in the correct shape install on the next poll, which proves the payload
    // reached this decode path rather than failing for an unrelated schema reason.
    mock.set_rules_response_override(json!({
        "protocol_version": 1,
        "content_version": 3,
        "rules": [
            serde_json::from_str::<serde_json::Value>(golden::RULE_SCENARIO_B).unwrap(),
            json!({
                "id": "proxy-impl-allowlist",
                "event_abis": {},
                "callstack": { "direct_delegatecalls": {
                    "entry": "0x00000000000000000000000000000000000000aa"
                } },
                "condition": { "!": { "in": [
                    { "var": "entry.delegateCodeHash" },
                    ["0x1111111111111111111111111111111111111111111111111111111111111111"]
                ]}},
                "action": "deny"
            })
        ]
    }));
    assert!(
        test_seams::load_and_install(&shared, &client).await.unwrap(),
        "a corrected snapshot must install on the next poll"
    );
    let current = shared.rules_slot().read().unwrap();
    assert_eq!(current.content_version, 3);
    assert_eq!(current.rules.len(), 2);
    assert!(current.has_callstack_rules());
}

/// A `GET /rules` snapshot declaring any version other than the one supported one installs
/// NOTHING and leaves last-known-good — including its `content_version` — in force.
///
/// Version 2 is the case that matters: it is the version the retired `callstack` selector shape
/// was authored against, so a snapshot declaring it must be refused by name rather than
/// adjudicated under the current contract.
#[tokio::test]
async fn it_an_unsupported_protocol_version_installs_nothing_and_keeps_last_known_good() {
    assert_eq!(UNSUPPORTED_PROTOCOL_VERSION, 2, "the first unsupported version is 2");

    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = not_ready_shared();
    assert!(test_seams::load_and_install(&shared, &client).await.unwrap());
    let previous = shared.rules_slot().read().unwrap().clone();
    assert_eq!(previous.content_version, 1);

    for version in [UNSUPPORTED_PROTOCOL_VERSION, 0, 3, 99] {
        mock.set_rules_response_override(json!({
            "protocol_version": version,
            "content_version": 2,
            "rules": [
                serde_json::from_str::<serde_json::Value>(golden::RULE_SCENARIO_B).unwrap()
            ]
        }));
        assert!(
            !test_seams::load_and_install(&shared, &client).await.unwrap(),
            "version {version} must install nothing"
        );
        let current = shared.rules_slot().read().unwrap().clone();
        assert!(
            Arc::ptr_eq(&previous, &current),
            "version {version}: last-known-good must stay installed"
        );
        assert_eq!(current.content_version, 1, "version {version}: content_version must not move");
    }

    // The same snapshot at the supported version installs, which proves the refusals above were
    // about the version and not about the rule.
    mock.set_rules_response_override(json!({
        "protocol_version": 1,
        "content_version": 2,
        "rules": [serde_json::from_str::<serde_json::Value>(golden::RULE_SCENARIO_B).unwrap()]
    }));
    assert!(test_seams::load_and_install(&shared, &client).await.unwrap());
    assert_eq!(shared.rules_slot().read().unwrap().content_version, 2);
}

/// The per-rule counterpart: a rule rejected by a check inside the rule COMPILER is reached by
/// `load_rules`, so the diagnostic set really does carry `(id, reason)` for it alongside the valid
/// sibling — and the install is still refused whole, with `content_version` unchanged.
#[tokio::test]
async fn it_a_compiler_rejected_rule_is_diagnosable_and_still_installs_nothing() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = not_ready_shared();
    assert!(test_seams::load_and_install(&shared, &client).await.unwrap());
    let previous = shared.rules_slot().read().unwrap().clone();
    assert_eq!(previous.content_version, 1);

    for (rule, expected_reason) in [
        // a checksummed alias address
        (
            json!({
                "id": "uppercase-alias-address",
                "event_abis": {},
                "callstack": { "direct_delegatecalls": {
                    "entry": "0x00000000000000000000000000000000000000AA"
                } },
                "condition": { "==": [ { "var": "entry.delegateTo" }, "0x00000000000000000000000000000000000000bb" ] },
                "action": "deny"
            }),
            "direct_delegatecalls alias 'entry' is not a lowercase 0x 20-byte address",
        ),
        // a condition reading a variable the alias does not provide
        (
            json!({
                "id": "unknown-alias-variable",
                "event_abis": {},
                "callstack": { "direct_delegatecalls": {
                    "entry": "0x00000000000000000000000000000000000000aa"
                } },
                "condition": { "==": [ { "var": "entry.delegateAddress" }, "0x00000000000000000000000000000000000000bb" ] },
                "action": "deny"
            }),
            "condition references unknown variable 'entry.delegateAddress'; direct_delegatecalls provides proxyAddress, proxyCodeHash, delegateTo and delegateCodeHash",
        ),
    ] {
        mock.set_rules_response_override(json!({
            "protocol_version": 1,
            "content_version": 2,
            "rules": [
                serde_json::from_str::<serde_json::Value>(golden::RULE_SCENARIO_B).unwrap(),
                rule
            ]
        }));
        let err = test_seams::load_and_install(&shared, &client)
            .await
            .expect_err("a rejected rule must refuse the entire snapshot");
        match &err {
            rcs_filter::FilterError::InvalidRules(1, rejected) => {
                assert_eq!(rejected[0].reason, expected_reason);
            }
            other => panic!("expected one rejected rule, got {other:?}"),
        }
        let current = shared.rules_slot().read().unwrap().clone();
        assert!(Arc::ptr_eq(&previous, &current), "last-known-good must stay installed");
        assert_eq!(current.content_version, 1, "content_version must not advance on rejection");
    }
}

/// an unsupported `protocol_version` still returns `Ok(false)` with no install, even
/// when a previously-good set is already in force. Pins that the strict schema did not turn the
/// version gate into an error or let it install anything.
#[tokio::test]
async fn it_unsupported_protocol_still_returns_ok_false_with_no_install() {
    let mock = Arc::new(MockRcsClient::new());
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let client: Arc<dyn RcsClient> = mock.clone();
    let shared = not_ready_shared();
    assert!(test_seams::load_and_install(&shared, &client).await.unwrap());
    let previous = shared.rules_slot().read().unwrap().clone();

    mock.set_rules_response_override(json!({
        "protocol_version": UNSUPPORTED_PROTOCOL_VERSION,
        "content_version": 2,
        "rules": []
    }));
    let installed =
        test_seams::load_and_install(&shared, &client).await.expect("not an error, a rejection");
    assert!(!installed, "an unsupported protocol version installs nothing");
    let current = shared.rules_slot().read().unwrap();
    assert!(Arc::ptr_eq(&previous, &current));
    assert_eq!(current.content_version, 1);
}
