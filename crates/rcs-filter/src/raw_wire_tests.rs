//! Duplicate rule keys refused on the **real** `GET /rules` read/install chain,
//! driven from raw bytes.
//!
//! The distinction this module exists for is narrow and load-bearing. `serde_json::Value` is a
//! map: it keeps one occurrence of a duplicate key and silently drops the rest. So a test that
//! builds its payload as a `Value`, or hands the bytes to `serde_json::from_value`, has already
//! destroyed the very evidence it means to assert on — whatever it then observes says nothing
//! about what the production path does with the original bytes.
//!
//! Every case here therefore starts as a raw JSON **string**, is written onto a socket, and is
//! read back by the production [`crate::client::ReqwestRcsClient`] and installed (or refused) by
//! the production [`crate::worker::load_and_install`]. Nothing in between converts it to a
//! `Value`, and [`the_value_hop_these_tests_exclude_would_accept_the_duplicate`] demonstrates,
//! executably, that the shortcut would have accepted exactly what the real path refuses.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::client::{RcsClient, ReqwestRcsClient, RulesResponse};
use crate::config::FilterConfig;
use crate::handle::Shared;
use crate::pool::BufferPool;
use crate::rules::{RawRule, RuleSet};
use crate::test_support::TestClock;
use crate::worker;

const START: u64 = 1_751_000_000;

const PROXY: &str = "0x1111111111111111111111111111111111111111";
const OTHER_PROXY: &str = "0x2222222222222222222222222222222222222222";

/// Serves exactly one `200 OK` response carrying `body` verbatim, then closes.
///
/// The body is passed through as the caller wrote it — this is the whole point, so it must not be
/// re-serialised, canonicalised or parsed anywhere on the way out.
async fn serve_one_raw_body(body: String) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind a loopback port");
    let address = listener.local_addr().expect("the listener has an address");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept the client");
        let mut request = Vec::new();
        let mut buffer = [0u8; 2048];
        loop {
            let read = stream.read(&mut buffer).await.expect("read the request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).await.expect("write the headers");
        let _ = stream.write_all(body.as_bytes()).await;
    });
    (format!("http://{address}"), task)
}

fn last_known_good() -> RuleSet {
    let rule: RawRule =
        serde_json::from_str(crate::test_support::golden::RULE_SCENARIO_A).expect("golden fixture");
    crate::rules::load_rules(crate::config::MIN_PROTOCOL_VERSION, 7, vec![rule])
}

fn shared_with_last_known_good() -> Shared {
    let (terminal_events, _) = tokio::sync::broadcast::channel(16);
    Shared {
        config: FilterConfig {
            enabled: true,
            rcs_base_url: "http://unused".into(),
            ..Default::default()
        },
        rules: Arc::new(RwLock::new(Arc::new(last_known_good()))),
        pool: Arc::new(Mutex::new(BufferPool::default())),
        clock: Arc::new(TestClock::new(START)),
        ready: Arc::new(AtomicBool::new(true)),
        terminal_events,
        metrics: crate::metrics::RcsFilterMetrics::default(),
    }
}

/// What one raw `GET /rules` body did to the installed snapshot.
struct WireOutcome {
    result: crate::Result<bool>,
    /// `true` when the very same `Arc` is still installed — i.e. nothing was swapped in.
    last_known_good_retained: bool,
    content_version: u64,
    rule_count: usize,
}

/// Pushes one raw body through the production read/install chain against a pre-installed
/// last-known-good snapshot, and reports what happened to that snapshot.
async fn install_raw_body(body: &str) -> WireOutcome {
    let shared = shared_with_last_known_good();
    let previous = shared.rules.read().expect("the rule slot is readable").clone();
    assert_eq!(previous.content_version, 7, "the fixture installs a known last-known-good");
    assert_eq!(previous.rules.len(), 1);

    let (url, server) = serve_one_raw_body(body.to_string()).await;
    let client: Arc<dyn RcsClient> =
        Arc::new(ReqwestRcsClient::new(url).expect("the production client builds"));
    let result = worker::load_and_install(&shared, &client).await;
    server.await.expect("the one-shot server finishes");

    let current = shared.rules.read().expect("the rule slot is readable").clone();
    WireOutcome {
        result,
        last_known_good_retained: Arc::ptr_eq(&previous, &current),
        content_version: current.content_version,
        rule_count: current.rules.len(),
    }
}

/// A valid protocol-v1 `GET /rules` body carrying one callstack rule, written as raw text so the
/// rejection cases are textual edits of a body that is known to install.
fn legal_v1_body(content_version: u64) -> String {
    format!(
        r#"{{"protocol_version":1,"content_version":{content_version},"rules":[{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY}"}}}},"condition":{{"!":{{"in":[{{"var":"p.delegateCodeHash"}},["0x{hash}"]]}}}},"action":"deny"}}]}}"#,
        hash = "11".repeat(32)
    )
}

/// The same body with `tail` appended to the rule object, which is how a duplicate modelled
/// rule-root key is introduced without disturbing anything else.
fn body_with_rule_tail(tail: &str) -> String {
    format!(
        r#"{{"protocol_version":1,"content_version":8,"rules":[{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY}"}}}},"condition":true,{tail}}}]}}"#
    )
}

fn assert_refused_and_last_known_good_intact(outcome: &WireOutcome, expected_in_reason: &str) {
    let error = match &outcome.result {
        Err(error) => error,
        Ok(installed) => panic!("the duplicate key must be refused, got Ok({installed})"),
    };
    let reason = error.to_string();
    assert!(reason.contains(expected_in_reason), "the refusal must name what it refused: {reason}");
    assert!(
        outcome.last_known_good_retained,
        "last-known-good must stay installed, byte for byte: {reason}"
    );
    assert_eq!(outcome.content_version, 7, "content_version must not advance on a refusal");
    assert_eq!(outcome.rule_count, 1, "nothing may be installed on a refusal");
}

// --------------------------------------------------------------------------------------------
// Duplicate modelled rule-root key, raw, through the real read/install chain
// --------------------------------------------------------------------------------------------

#[tokio::test]
async fn duplicate_rule_root_keys_are_refused_on_the_real_rules_wire() {
    // Same value, different values, and both orders. The verdict has to be identical in all
    // three: a decode that resolved the conflict by "last one wins" would differ between the
    // second and third case, and a decode that resolved it by "first one wins" would differ the
    // other way. Only a refusal is order-independent.
    let cases = [
        (r#""action":"deny","action":"deny""#.to_string(), "same value"),
        (r#""action":"deny","action":"allow""#.to_string(), "different values"),
        (r#""action":"allow","action":"deny""#.to_string(), "swapped order"),
        (
            format!(
                r#""callstack":{{"direct_delegatecalls":{{"q":"{OTHER_PROXY}"}}}},"action":"deny""#
            ),
            "a second callstack section",
        ),
    ];
    for (tail, label) in cases {
        let outcome = install_raw_body(&body_with_rule_tail(&tail)).await;
        assert_refused_and_last_known_good_intact(&outcome, "duplicate field");
        let reason = outcome.result.unwrap_err().to_string();
        assert!(
            serde_json::from_str::<RulesResponse>(&body_with_rule_tail(&tail)).is_err(),
            "{label}: the same bytes must also be refused by the typed decode in isolation"
        );
        assert!(reason.contains("duplicate field"), "{label}: {reason}");
    }
}

// --------------------------------------------------------------------------------------------
// Duplicate `direct_delegatecalls` alias key, raw, through the real chain
// --------------------------------------------------------------------------------------------

#[tokio::test]
async fn duplicate_alias_keys_are_refused_on_the_real_rules_wire() {
    for (first, second, label) in [
        (PROXY, PROXY, "same value"),
        (PROXY, OTHER_PROXY, "different values"),
        (OTHER_PROXY, PROXY, "swapped order"),
    ] {
        let body = format!(
            r#"{{"protocol_version":1,"content_version":8,"rules":[{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{first}","p":"{second}"}}}},"condition":true,"action":"deny"}}]}}"#
        );
        let outcome = install_raw_body(&body).await;
        assert_refused_and_last_known_good_intact(
            &outcome,
            "direct_delegatecalls declares alias 'p' more than once",
        );
        // Specifically NOT "accepted with the last value": neither address may survive.
        let reason = outcome.result.unwrap_err().to_string();
        assert!(!reason.contains("is not a lowercase"), "{label}: {reason}");
    }
}

// --------------------------------------------------------------------------------------------
// The legal control, over the same wire
// --------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_legal_v1_body_installs_over_the_same_wire() {
    // Without this, every refusal above could be a transport or schema accident that has nothing
    // to do with the duplicate key. The body is the same shape minus the duplicate.
    let outcome = install_raw_body(&legal_v1_body(8)).await;
    assert!(
        matches!(outcome.result, Ok(true)),
        "the legal control must install: {:?}",
        outcome.result
    );
    assert!(!outcome.last_known_good_retained, "a successful install replaces the snapshot");
    assert_eq!(outcome.content_version, 8, "content_version advances on a real install");
    assert_eq!(outcome.rule_count, 1);
}

// --------------------------------------------------------------------------------------------
// The type-fidelity guard, and why it is not decoration
// --------------------------------------------------------------------------------------------

/// The executable statement of the defect this whole module guards against: route the identical
/// bytes through `serde_json::Value` first and both duplicate classes are **accepted**, with one
/// occurrence silently discarded.
///
/// This is not a test of production behaviour — production has no such hop, which the structural
/// guard below pins. It is the counterexample that makes the rows above falsifiable: if the
/// production path ever grew a `Value` hop, this is precisely the acceptance it would start
/// exhibiting, and the raw-wire tests would be the only thing that noticed.
#[test]
fn the_value_hop_these_tests_exclude_would_accept_the_duplicate() {
    let root_dup = body_with_rule_tail(r#""action":"deny","action":"allow""#);
    let via_value: serde_json::Value =
        serde_json::from_str(&root_dup).expect("a duplicate key is still legal JSON text");
    let collapsed = serde_json::from_value::<RulesResponse>(via_value)
        .expect("the Value hop silently accepts a duplicate modelled key");
    assert_eq!(collapsed.rules.len(), 1, "and one of the two values simply disappeared");

    let alias_dup = format!(
        r#"{{"protocol_version":1,"content_version":8,"rules":[{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY}","p":"{OTHER_PROXY}"}}}},"condition":true,"action":"deny"}}]}}"#
    );
    let via_value: serde_json::Value =
        serde_json::from_str(&alias_dup).expect("still legal JSON text");
    assert!(
        serde_json::from_value::<RulesResponse>(via_value).is_ok(),
        "the Value hop silently accepts a duplicate alias too"
    );

    // And the raw path refuses both, which is the property the rows above assert over the wire.
    assert!(serde_json::from_str::<RulesResponse>(&root_dup).is_err());
    assert!(serde_json::from_str::<RulesResponse>(&alias_dup).is_err());
}

/// Enforced by the type system rather than by reading the code: the rule vector that
/// crosses the `GET /rules` boundary is `Vec<RawRule>`, not a vector of `Value`s or any map.
///
/// If someone retyped it, this stops compiling — which is the only kind of guard that cannot be
/// forgotten.
#[test]
fn the_rules_field_type_is_pinned_at_the_rules_boundary() {
    fn rules_of(response: RulesResponse) -> Vec<RawRule> {
        response.rules
    }
    // The coercion is the assertion: a `Vec<serde_json::Value>` field could not satisfy it.
    let pinned: fn(RulesResponse) -> Vec<RawRule> = rules_of;
    let response = serde_json::from_str::<RulesResponse>(&legal_v1_body(8))
        .expect("the legal control decodes");
    assert_eq!(pinned(response).len(), 1);
}

/// Second half of the same guard: no `from_value` hop exists in the production region of any file on the
/// rule path.
///
/// The scan is textual on purpose — it is the only form that catches a hop being *introduced*,
/// which a behavioural test can only catch if someone also remembers to keep the raw-wire cases.
/// The boundary is explicit: each file is cut at its first `#[cfg(test)]` (or taken whole when it
/// has none), and that cut is asserted to have been found where one is expected, so the guard can
/// never silently degrade into scanning nothing.
#[test]
fn no_value_hop_exists_on_the_production_rule_path() {
    // (path label, source, whether the file is expected to carry a test module)
    let files = [
        ("client.rs", include_str!("client.rs"), true),
        ("worker.rs", include_str!("worker.rs"), true),
        ("rules/model.rs", include_str!("rules/model.rs"), false),
        ("rules/mod.rs", include_str!("rules/mod.rs"), false),
        ("rules/validation.rs", include_str!("rules/validation.rs"), true),
    ];
    for (label, source, has_tests) in files {
        let cut = source.find("#[cfg(test)]");
        assert_eq!(
            cut.is_some(),
            has_tests,
            "{label}: the scan boundary moved, so this guard is no longer scanning what it claims"
        );
        let production = &source[..cut.unwrap_or(source.len())];
        assert!(
            !production.contains("from_value"),
            "{label}: a serde_json::Value hop appeared on the production rule path; a duplicate \
             key cannot survive it, so the duplicate-key rejection would silently stop working"
        );
        assert!(
            !production.contains("Vec<serde_json::Value>"),
            "{label}: the rule vector must stay typed"
        );
    }
}

// --------------------------------------------------------------------------------------------
// Joint acceptance against the locked producer's GENUINELY RENDERED response
// --------------------------------------------------------------------------------------------

/// The `GET /rules` body the locked producer's own renderer actually produced, captured verbatim.
///
/// Provenance, reproducible in full:
///   * producer `github/xlayer-rcs` @ `8944b24f00c058cb5e65090a5c210335d1f186ed`, clean checkout
///   * its real path, in order: `rules.LoadRulesFile(config/rules-examples.json)` →
///     `provider.SetHandWrittenRules` → `provider.Refresh(nil, nil, nil, false, 1751000000)` →
///     `api.NewRouter(..)` served over HTTP → `GET /rules`
///   * `sha256` of these bytes: see [`RENDERED_RESPONSE_SHA256`]
///
/// This is **not** the producer's static `config/rules-examples.json`: that file is 5,654 bytes of
/// hand-written source, while this is the 3,734-byte wire response its renderer emits from it —
/// different key order, `omitempty` fields dropped, the audit defaults applied, and the
/// `protocol_version` / `content_version` envelope added. Pinning the source file would prove
/// provenance only; pinning the rendered response is what makes the acceptance below joint.
const RENDERED_RESPONSE: &str = include_str!("../testdata/producer-rendered-rules-response.json");

/// `sha256` of [`RENDERED_RESPONSE`] — re-checkable with
/// `sha256sum crates/rcs-filter/testdata/producer-rendered-rules-response.json`.
const RENDERED_RESPONSE_SHA256: &str =
    "dbd8d60bc1c04d153670c859d08dfb02a7b4d5f2c0e899b7442264062a03244e";

/// keccak digest of the same bytes, pinned because keccak is already in this crate's
/// dependencies and sha2 is not.
const RENDERED_RESPONSE_KECCAK256: &str =
    "8e9f2d0eed9603e119129dcfd82d8300cc8b36ac361f4a2e6dad42746e5d43cf";

fn hash(hex: &str) -> alloy_primitives::B256 {
    hex.parse().expect("a 32-byte hex digest")
}

#[test]
fn the_rendered_response_fixture_has_not_drifted() {
    // The premise of every assertion below: drift here would make "joint" acceptance a test
    // against bytes nobody rendered, so it must fail loudly rather than pass quietly.
    assert_eq!(RENDERED_RESPONSE.len(), 3_734, "the rendered response is 3,734 bytes");
    assert_eq!(
        alloy_primitives::keccak256(RENDERED_RESPONSE.as_bytes()),
        hash(RENDERED_RESPONSE_KECCAK256),
        "the vendored rendered response changed; re-render it from the locked producer before \
         updating this pin"
    );
    assert_eq!(RENDERED_RESPONSE_SHA256.len(), 64);
}

/// The locked producer's rendered response, served verbatim and installed through the consumer's
/// real `GET /rules` read/install chain: all 7 rules accepted, nothing rejected.
#[tokio::test]
async fn the_locked_producers_rendered_response_installs_over_the_real_wire() {
    let outcome = install_raw_body(RENDERED_RESPONSE).await;
    assert!(
        matches!(outcome.result, Ok(true)),
        "the producer's own rendered response must install: {:?}",
        outcome.result
    );
    assert_eq!(outcome.rule_count, 7, "all seven rendered rules are active");
    assert_eq!(
        outcome.content_version, 1_751_000_001,
        "the content_version the producer's allocator actually issued"
    );
    assert!(!outcome.last_known_good_retained, "a successful install replaces last-known-good");
}

/// The installed snapshot's own composition and every one of its callstack conditions, evaluated.
///
/// The snapshot under test is the one the step above installed — produced by the producer's
/// renderer, carried over a socket, decoded by the production client and compiled by the
/// production loader. The condition evaluation is `matching::try_evaluate`, which is the exact
/// call `FilterHandle::screen_tx_with_snapshot` makes for a transaction it has not seen before;
/// the only thing the handle adds around it is buffer dedup, which a fresh hash does not reach.
///
/// Both directions of every alias-bearing rule are driven, because an allowlist rule that can
/// only ever deny would satisfy a one-sided test without actually evaluating anything.
#[tokio::test]
async fn every_rendered_callstack_condition_actually_fires_on_the_installed_snapshot() {
    use crate::callstack::{CallObservation, Observations};
    use crate::handle::ScreenInput;
    use crate::matching::{try_evaluate, MatchOutcome};
    use crate::test_support::log_builder::erc20_transfer;
    use alloy_primitives::{Address, B256, U256};

    let shared = shared_with_last_known_good();
    let (url, server) = serve_one_raw_body(RENDERED_RESPONSE.to_string()).await;
    let client: Arc<dyn RcsClient> =
        Arc::new(ReqwestRcsClient::new(url).expect("the production client builds"));
    assert!(
        worker::load_and_install(&shared, &client).await.expect("the rendered response installs"),
        "the rendered response must install"
    );
    server.await.expect("the one-shot server finishes");
    let rules = shared.rules.read().expect("the rule slot is readable").clone();

    // Composition, read off the installed snapshot rather than off the fixture text.
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
        ],
        "the installed snapshot is the producer's seven rendered rules, in its own order"
    );
    assert_eq!(rules.selector_bearing_rule_count(), 4, "four rules declare an alias");
    assert_eq!(rules.callstack_selectors().len(), 3, "over three distinct proxy addresses");

    // The producer's own addresses and allowlisted code hashes.
    let vault: Address = "0x00000000000000000000000000000000000ab001".parse().unwrap();
    let router: Address = "0x00000000000000000000000000000000000ab002".parse().unwrap();
    let bridge: Address = "0x00000000000000000000000000000000000ab003".parse().unwrap();
    let vault_ok: B256 = hash(&"11".repeat(32));
    let router_ok: B256 = hash(&"33".repeat(32));
    let bridge_impl_1: Address = "0x00000000000000000000000000000000000c0de1".parse().unwrap();
    let bridge_impl_2: Address = "0x00000000000000000000000000000000000c0de2".parse().unwrap();
    let bridge_hash_1: B256 = hash(&"44".repeat(32));
    let bridge_hash_2: B256 = hash(&"55".repeat(32));
    let rogue: B256 = B256::repeat_byte(0xee);
    let some_impl: Address = "0x00000000000000000000000000000000000f00d1".parse().unwrap();
    let flagged: Address = "0x00000000000000000000000000000000000bad00".parse().unwrap();

    let screen_with_limit =
        |logs: &[alloy_primitives::Log], items: &[CallObservation], limit: usize| {
            let input = ScreenInput {
                tx_hash: B256::repeat_byte(0x77),
                origin: "0x000000000000000000000000000000000a0000d1".parse().unwrap(),
                tx_to: None,
                nonce: 1,
                value: U256::ZERO,
                block_height: 100,
                logs,
                observations: Observations::available(rules.stamp(), items),
            };
            try_evaluate(&rules, &input, limit)
        };
    let screen = |logs: &[alloy_primitives::Log], items: &[CallObservation]| {
        screen_with_limit(logs, items, 10_000)
    };
    let delegated = |proxy: Address, target: Address, code: B256| {
        CallObservation::direct_delegatecall(proxy, B256::repeat_byte(0xa1), target, code)
    };

    // deny-proxy-delegatecall-to-unapproved-implementation — both directions.
    assert_eq!(
        screen(&[], &[delegated(vault, some_impl, rogue)]),
        Ok(MatchOutcome::Deny),
        "an unapproved implementation behind the vault proxy must be objected to"
    );
    assert_eq!(
        screen(&[], &[delegated(vault, some_impl, vault_ok)]),
        Ok(MatchOutcome::Allow),
        "an allowlisted implementation must not be"
    );

    // deny-router-delegatecall-to-unapproved-implementation — both directions.
    assert_eq!(screen(&[], &[delegated(router, some_impl, rogue)]), Ok(MatchOutcome::Deny));
    assert_eq!(screen(&[], &[delegated(router, some_impl, router_ok)]), Ok(MatchOutcome::Allow));

    // deny-paired-implementation-address-and-code-hash — the pairing is what is being tested, so
    // the crossed combination of two individually approved halves must still be objected to.
    assert_eq!(
        screen(&[], &[delegated(bridge, bridge_impl_1, bridge_hash_1)]),
        Ok(MatchOutcome::Allow),
        "an exactly approved (address, code hash) pair passes"
    );
    assert_eq!(
        screen(&[], &[delegated(bridge, bridge_impl_2, bridge_hash_2)]),
        Ok(MatchOutcome::Allow),
        "so does the other approved pair"
    );
    assert_eq!(
        screen(&[], &[delegated(bridge, bridge_impl_1, bridge_hash_2)]),
        Ok(MatchOutcome::Deny),
        "crossing the two approved pairs is NOT approved"
    );

    // schema-compatibility-mixed-event-and-direct-delegatecall-audit — the one mixed rule. It
    // needs BOTH a Transfer from the flagged address and an unapproved implementation behind the
    // vault proxy, and its action is `audit`, so a real action payload must come back.
    let flagged_transfer = vec![erc20_transfer(flagged, flagged, some_impl, U256::from(7u64))];
    match screen(&flagged_transfer, &[delegated(vault, some_impl, rogue)]) {
        // The vault deny rule also matches this observation and is stricter, so deny wins — which
        // is the correct merge, and is why the audit direction is driven with an implementation
        // the deny rule allows but this rule does not.
        Ok(MatchOutcome::Deny) => {}
        other => panic!("expected the stricter deny to win, got {other:?}"),
    }
    // `0x2222..` is on the vault deny rule's allowlist but NOT on this audit rule's, so only the
    // audit rule can fire here.
    let audit_only: B256 = hash(&"22".repeat(32));
    match screen(&flagged_transfer, &[delegated(vault, some_impl, audit_only)]) {
        Ok(MatchOutcome::Audit { actions, .. }) => {
            let items = actions
                .get("delegatecall_review")
                .expect("the producer's own audit type must carry the action");
            assert_eq!(items.len(), 1, "one physical log, one action item");
            assert_eq!(items[0].name, "transfer", "named by the rule's own event alias");
        }
        other => panic!("the mixed audit rule must fire and carry content, got {other:?}"),
    }
    // And the same mixed rule is SUPPRESSED when its event did not fire — the same behaviour, on
    // the producer's own rule rather than on a synthetic one.
    //
    // The verdict alone cannot show this: the producer's condition opens with
    // `transfer.from in [..]`, which is false against a null binding, so an evaluated rule and a
    // suppressed rule both return Allow. The BUDGET distinguishes them, because a suppressed rule
    // contributes zero candidates and an evaluated one contributes its product. For this
    // observation set the charged rules are exactly the event-only
    // `blk-compromised-contract` (one null candidate) and the vault deny rule (one observation);
    // the two unobserved aliases and this mixed rule contribute nothing, so the total is 2. An
    // unsuppressed mixed rule would make it 3.
    let one_vault_frame = [delegated(vault, some_impl, audit_only)];
    assert_eq!(
        screen_with_limit(&[], &one_vault_frame, 2),
        Ok(MatchOutcome::Allow),
        "with no Transfer log the mixed rule is not evaluated and costs nothing"
    );
    assert!(
        screen_with_limit(&[], &one_vault_frame, 1).is_err(),
        "the two rules that ARE charged really are charged, so the budget above is not vacuous"
    );

    // Suppression, on the real rule set: a transaction that touched none of the three proxies
    // must produce no objection, even though every alias-bearing rule is a negated allowlist.
    assert_eq!(
        screen(&[], &[]),
        Ok(MatchOutcome::Allow),
        "a transaction that touched no declared proxy must never be objected to"
    );
}
