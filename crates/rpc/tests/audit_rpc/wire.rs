//! The audit RPC's request boundary: the raw JSON-RPC decode, the three-key request body, the
//! rule-rejection detail, the producer-rendered rule document and the four-field response shape.

use alloy_primitives::{Address, B256, U256};
use jsonrpsee::types::Params;
use rcs_filter::rules::{load_rules, RawRule};
use revm::database::InMemoryDB;
use revm::state::{AccountInfo, Bytecode};
use revm::Database as _;
use xlayer_rpc::audit::rpc::observation_recorder;
use xlayer_rpc::audit::rpc::test_seams::execute_audit_batch;
use xlayer_rpc::audit::rpc::{AuditTransactionsRequest, AuditTransactionsResponse};
use xlayer_rpc::audit::verdict::Verdict;

use crate::harness::*;

const PROXY_HEX: &str = "0x00000000000000000000000000000000000000aa";
const OTHER_PROXY_HEX: &str = "0x00000000000000000000000000000000000000bb";

/// Decodes one raw JSON-RPC parameter array exactly as the generated server body does.
///
/// On failure the whole refusal is reported as the server would surface it: the JSON-RPC
/// error code plus the `data` payload, which is where jsonrpsee puts the serde reason — its
/// `message` is the fixed `"Invalid params"` string and carries no detail.
fn decode_params(raw_params: &str) -> Result<AuditTransactionsRequest, (i32, String)> {
    Params::new(Some(raw_params)).sequence().next::<AuditTransactionsRequest>().map_err(|error| {
        let detail =
            error.data().map(|data| data.get().trim_matches('"').to_string()).unwrap_or_default();
        (error.code(), detail)
    })
}

/// Asserts a refusal is the request-level `-32602` and that its reason names `expected`.
fn assert_invalid_params(
    result: Result<AuditTransactionsRequest, (i32, String)>,
    expected: &str,
    label: &str,
) {
    let (code, detail) = match result {
        Err(error) => error,
        Ok(_) => panic!("{label}: the duplicate key must be refused, but the request decoded"),
    };
    assert_eq!(code, -32602, "{label}: a malformed request is an invalid-params refusal");
    assert!(detail.contains(expected), "{label}: the reason must name what it refused: {detail}");
}

/// A positional parameter array carrying one request object built from `rule_body`.
fn params_with_rule(rule_body: &str) -> String {
    format!(r#"[{{"rules":[{rule_body}],"txs":[],"known_allowed":[]}}]"#)
}

fn legal_rule() -> String {
    format!(
        r#"{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY_HEX}"}}}},"condition":{{"!":{{"in":[{{"var":"p.delegateCodeHash"}},["0x{hash}"]]}}}},"action":"deny"}}"#,
        hash = "11".repeat(32)
    )
}

/// A duplicate **modelled rule-root** key, raw, through the real request decode.
///
/// Same value, different values and swapped order must all be refused identically. Any
/// "last one wins" or "first one wins" resolution would make the second and third case
/// disagree, so order-independence is the assertion that matters.
#[test]
fn duplicate_rule_root_keys_are_refused_by_the_real_request_decode() {
    let tails = [
        (r#""action":"deny","action":"deny""#.to_string(), "same value"),
        (r#""action":"deny","action":"allow""#.to_string(), "different values"),
        (r#""action":"allow","action":"deny""#.to_string(), "swapped order"),
        (
            format!(
                r#""callstack":{{"direct_delegatecalls":{{"q":"{OTHER_PROXY_HEX}"}}}},"action":"deny""#
            ),
            "a second callstack section",
        ),
    ];
    for (tail, label) in tails {
        let rule = format!(
            r#"{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY_HEX}"}}}},"condition":true,{tail}}}"#
        );
        assert_invalid_params(decode_params(&params_with_rule(&rule)), "duplicate field", label);
    }
}

/// A duplicate `direct_delegatecalls` alias key, raw, through the real request
/// decode. Specifically NOT "accepted with the last value".
#[test]
fn duplicate_alias_keys_are_refused_by_the_real_request_decode() {
    for (first, second, label) in [
        (PROXY_HEX, PROXY_HEX, "same value"),
        (PROXY_HEX, OTHER_PROXY_HEX, "different values"),
        (OTHER_PROXY_HEX, PROXY_HEX, "swapped order"),
    ] {
        let rule = format!(
            r#"{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{first}","p":"{second}"}}}},"condition":true,"action":"deny"}}"#
        );
        assert_invalid_params(
            decode_params(&params_with_rule(&rule)),
            "direct_delegatecalls declares alias 'p' more than once",
            label,
        );
    }
}

/// The legal control through the identical decoder, so none of the refusals above can be a
/// schema or transport accident — and it goes on to pass every request-level check, which is
/// what proves the rejected bodies really did stop at the decode.
#[test]
fn the_legal_v1_request_decodes_and_prepares_through_the_same_decoder() {
    let req =
        decode_params(&params_with_rule(&legal_rule())).expect("the legal control must decode");
    assert_eq!(req.rules.len(), 1);
    let mut db = InMemoryDB::default();
    let results = execute_audit_batch(&req, &mut db, audit_env())
        .expect("the legal control passes every check");
    assert!(results.is_empty(), "no transactions were submitted");
}

/// The executable counterexample that makes the two rows above falsifiable: route the same
/// bytes through `serde_json::Value` and both duplicate classes are **accepted**, one
/// occurrence silently discarded. Production has no such hop — which
/// [`no_value_hop_exists_on_the_production_audit_request_path`] pins — and this is
/// exactly the acceptance that would reappear if it ever grew one.
#[test]
fn the_value_hop_this_module_excludes_would_accept_the_duplicate() {
    let root_dup = format!(
        r#"{{"rules":[{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY_HEX}"}}}},"condition":true,"action":"deny","action":"allow"}}],"txs":[],"known_allowed":[]}}"#
    );
    let alias_dup = format!(
        r#"{{"rules":[{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY_HEX}","p":"{OTHER_PROXY_HEX}"}}}},"condition":true,"action":"deny"}}],"txs":[],"known_allowed":[]}}"#
    );
    for (body, label) in [(&root_dup, "modelled root key"), (&alias_dup, "alias key")] {
        let via_value: serde_json::Value =
            serde_json::from_str(body).expect("a duplicate key is still legal JSON text");
        assert!(
            serde_json::from_value::<AuditTransactionsRequest>(via_value).is_ok(),
            "{label}: the Value hop silently accepts the duplicate"
        );
        // And the raw decode the server actually performs refuses it.
        assert!(
            decode_params(&format!("[{body}]")).is_err(),
            "{label}: the real request decode must refuse it"
        );
    }
}

/// The fidelity guard, by the type system: the rule vector that crosses the audit request boundary is
/// `Vec<RawRule>`. Retyping it to a vector of `Value`s stops this compiling.
#[test]
fn the_request_rules_field_type_is_pinned() {
    fn rules_of(req: AuditTransactionsRequest) -> Vec<RawRule> {
        req.rules
    }
    let pinned: fn(AuditTransactionsRequest) -> Vec<RawRule> = rules_of;
    let req = decode_params(&params_with_rule(&legal_rule())).expect("the control decodes");
    assert_eq!(pinned(req).len(), 1);
}

/// The fidelity guard, second half: no `from_value` hop exists in the production region of this file.
///
/// Textual on purpose — it is the only form that catches a hop being *introduced*, which a
/// behavioural test only catches if someone also remembers to keep the raw cases. The
/// boundary is the first test module (`#[cfg(test)]\nmod `), not merely the first
/// `#[cfg(test)]`, because production code above it legitimately carries test-only counters;
/// finding that boundary is asserted so the guard can never degrade into scanning nothing.
#[test]
fn no_value_hop_exists_on_the_production_audit_request_path() {
    const SOURCE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/audit/rpc.rs"));
    let boundary = SOURCE
        .find("#[cfg(test)]\nmod ")
        .expect("the production/test boundary must be findable, or this guard scans nothing");
    let production = &SOURCE[..boundary];
    assert!(
        production.contains("pub struct AuditTransactionsRequest"),
        "the scanned region must actually contain the request type"
    );
    assert!(
        !production.contains("from_value"),
        "a serde_json::Value hop appeared on the production audit request path; a duplicate \
             key cannot survive it, so the duplicate-key rejection would silently stop working"
    );
    assert!(
        !production.contains("Vec<serde_json::Value>"),
        "the request's rule vector must stay typed"
    );
}

// ---------------------------------------------------------------- request boundary --------

/// The **verbatim upstream rule-source document** — the same bytes the rule-layer tests use —
/// is accepted at this entry point too, with no rejections.
///
/// Both entry points share one compiler, and this pins that: a rule set accepted on the
/// hot-reload path must be accepted here, by the same `load_rules` call, over the same file.
#[test]
fn the_upstream_rule_source_document_is_accepted_at_this_entry_point() {
    const UPSTREAM_RULES: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rcs-filter/tests/data/upstream-rule-source-examples.json"
    ));
    let document: serde_json::Value = serde_json::from_str(UPSTREAM_RULES).expect("valid JSON");

    // The caller's own body shape: three keys, no protocol version, no label map.
    let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
        "rules": document["rules"].clone(),
        "txs": [],
        "known_allowed": []
    }))
    .expect("the upstream rules deserialize at this entry point");
    assert_eq!(req.rules.len(), 7);

    let mut db = InMemoryDB::default();
    let results = execute_audit_batch(&req, &mut db, audit_env())
        .expect("every upstream rule must be accepted");
    assert!(results.is_empty());

    let rules = load_rules(1, 0, req.rules.clone());
    assert!(rules.rejected.is_empty(), "{:?}", rules.rejected);
    assert_eq!(rules.rules.len(), 7);
    assert_eq!(rules.rules.iter().filter(|r| !r.callstack.is_empty()).count(), 4);
    assert_eq!(
        rules.callstack_selectors().len(),
        3,
        "three distinct proxy addresses, because two rules observe the same one"
    );
    assert!(rules.has_callstack_rules(), "an alias-bearing request arms capture");
}

/// The caller's exact body — `{rules, txs, known_allowed}`, with NO `protocol_version` and NO
/// label map — is accepted with alias-bearing rules and a NON-EMPTY `known_allowed`.
///
/// A first round with an empty `known_allowed` proves nothing, which is why this one is
/// non-empty.
#[test]
fn the_callers_exact_three_key_body_is_accepted_with_a_non_empty_known_allowed() {
    let source = format!("0x{}", "66".repeat(32));
    let body = serde_json::json!({
        "rules": [proxy_impl_allowlist_rule_json()],
        "txs": [],
        "known_allowed": [source]
    });
    assert_eq!(
        body.as_object().unwrap().len(),
        3,
        "the fixture must really be the three-key body, not one that merely resembles it"
    );
    let req: AuditTransactionsRequest =
        serde_json::from_value(body).expect("the three-key body deserializes");
    assert!(!req.known_allowed.is_empty());

    let mut db = InMemoryDB::default();
    execute_audit_batch(&req, &mut db, audit_env())
        .expect("a request with no label map is the ordinary case");
}

/// A multi-round sequence in the caller's own shape: round 1 with an empty `known_allowed`,
/// then each later round carrying the entries the previous round resolved to Allow. Every
/// round is accepted and adjudicated.
///
/// Convergence is also pinned: an entry already in `known_allowed` stays `Allow` and never
/// re-enters `Audit`, so the sequence terminates instead of looping.
#[test]
fn a_multi_round_known_allowed_sequence_is_accepted_and_converges() {
    let first = format!("0x{}", "66".repeat(32));
    let second = format!("0x{}", "67".repeat(32));
    let mut resolved: Vec<String> = Vec::new();

    for round in 0..3u8 {
        let known: Vec<&str> = resolved.iter().map(String::as_str).collect();

        // The boundary accepts the round, with no label map at any point.
        let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
            "rules": [proxy_impl_allowlist_rule_json()],
            "txs": [],
            "known_allowed": resolved.clone()
        }))
        .expect("the round body deserializes");
        let mut db = InMemoryDB::default();
        execute_audit_batch(&req, &mut db, audit_env())
            .unwrap_or_else(|e| panic!("round {round} must be accepted: {e:?}"));

        // And the batch body adjudicates it, with an approved implementation.
        let run = run_batch(
            &[deposit_request(&first, PROXY), deposit_request(&second, PROXY)],
            vec![proxy_impl_allowlist_rule()],
            &known,
            seeded_db(APPROVED_IMPL, 1),
        );
        for (index, result) in run.results.iter().enumerate() {
            assert_eq!(
                result.verdict,
                Verdict::Allow,
                "round {round}, deposit {index}: an approved implementation allows, and an \
                     already-resolved entry never re-enters adjudication"
            );
            assert!(result.actions.is_none(), "round {round}: no audit payload is produced");
        }
        resolved = vec![first.clone(), second.clone()];
    }
}

/// A condition reading a variable the rule's own aliases do not provide is rejected at this
/// entry point too, naming the variable, with nothing executed or committed.
///
/// Asserted here as well as on the hot-reload path because both entry points share one
/// compiler and a negative that holds on only one of them is not a guarantee.
#[test]
fn a_condition_reading_an_unprovided_alias_variable_is_rejected_at_this_entry_point() {
    for field in ["delegateAddress", "contextCodeHash", "matched", "implCodeHash"] {
        let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
            "rules": [{
                "id": "unknown-alias-variable",
                "event_abis": {},
                "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
                "condition": { "==": [
                    { "var": format!("entry.{field}") },
                    format!("{APPROVED_IMPL:#x}")
                ] },
                "action": "deny"
            }],
            "txs": [],
            "known_allowed": []
        }))
        .expect("the request decodes; the rejection happens in the compiler");
        let mut db = InMemoryDB::default();
        let error = match execute_audit_batch(&req, &mut db, audit_env()) {
            Err(error) => error,
            Ok(_) => panic!("entry.{field} must be rejected"),
        };
        assert_eq!(error.code(), -32602);
        let detail = error.data().expect("the rejected rule is named").to_string();
        assert!(detail.contains(&format!("entry.{field}")), "{detail}");
        assert!(
            detail.contains(&format!(
                "condition references unknown variable 'entry.{field}'; direct_delegatecalls \
                 provides proxyAddress, proxyCodeHash, delegateTo and delegateCodeHash"
            )),
            "{detail}"
        );
    }

    // The control: the two provided variables are accepted, so the rejections above are about
    // the variable name and not about the rule shape.
    let req: AuditTransactionsRequest = serde_json::from_value(serde_json::json!({
        "rules": [proxy_impl_allowlist_rule_json()],
        "txs": [],
        "known_allowed": []
    }))
    .expect("the control decodes");
    let mut db = InMemoryDB::default();
    assert!(execute_audit_batch(&req, &mut db, audit_env()).is_ok());
}

/// An undeclared key inside `callstack` fails at this entry point too, during the request
/// decode, naming the offending key — so a near-miss spelling can never degrade a rule into a
/// weakened event-only one. A misspelled rule-ROOT sibling fails the same decode.
#[test]
fn an_undeclared_callstack_key_is_rejected_at_this_entry_point() {
    for section in [
        serde_json::json!({ "entry": { "address": format!("{PROXY:#x}") } }),
        serde_json::json!({ "direct_delegatecall": { "entry": format!("{PROXY:#x}") } }),
        serde_json::json!({ "directDelegatecalls": { "entry": format!("{PROXY:#x}") } }),
        serde_json::json!({ "Direct_DelegateCalls": { "entry": format!("{PROXY:#x}") } }),
    ] {
        let error = serde_json::from_value::<AuditTransactionsRequest>(serde_json::json!({
            "rules": [{
                "id": "undeclared-callstack-key",
                "event_abis": {},
                "callstack": section,
                "condition": true,
                "action": "deny"
            }],
            "txs": [],
            "known_allowed": []
        }))
        .expect_err("an undeclared callstack key must be rejected");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    // A misspelled rule-root sibling is refused by the rule root's own decode gate, naming the
    // key, rather than quietly losing the call-frame constraint.
    let error = serde_json::from_value::<AuditTransactionsRequest>(serde_json::json!({
        "rules": [{
            "id": "misspelled-sibling",
            "event_abis": {},
            "callStack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
            "condition": true,
            "action": "deny"
        }],
        "txs": [],
        "known_allowed": []
    }))
    .expect_err("a misspelled rule-root sibling must fail the request decode");
    assert!(error.to_string().contains("callStack"), "{error}");
}

/// a rule refused by the compiler is listed in the `-32602` detail array with its rule id and
/// the rejection reason, so the caller learns WHICH rule was refused and WHY rather than only
/// how many were.
#[test]
fn a_rejected_rule_is_listed_in_the_error_detail_with_its_id_and_reason() {
    let bad_alias: RawRule = serde_json::from_value(serde_json::json!({
        "id": "typo-rule",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": "0xAABB" } },
        "condition": true,
        "action": "deny"
    }))
    .expect("the alias is a plain string on the wire, so the rule still decodes");

    let rules = load_rules(1, 0, vec![bad_alias.clone()]);
    assert_eq!(rules.rejected.len(), 1, "the rule must be refused, not installed");

    let req = AuditTransactionsRequest {
        rules: vec![bad_alias],
        txs: Vec::new(),
        known_allowed: Vec::new(),
    };
    let mut db = InMemoryDB::default();
    let error = execute_audit_batch(&req, &mut db, audit_env())
        .expect_err("a refused rule must fail the whole request");
    assert_eq!(error.code(), -32602);
    let detail: Vec<serde_json::Value> =
        serde_json::from_str(error.data().expect("a detail array is attached").get())
            .expect("the detail is a JSON array");
    assert_eq!(detail.len(), 1, "one refusal, one entry: {detail:?}");
    assert_eq!(detail[0]["id"], "typo-rule", "the entry names the rule: {detail:?}");
    let reason = detail[0]["reason"].as_str().expect("the entry carries a reason");
    assert!(
        reason.contains("not a lowercase 0x 20-byte address"),
        "the reason must state why the rule was refused: {reason}"
    );
}

// ------------------------------------------- producer-rendered joint acceptance --------

/// The producer's own `vaultProxy` address, from its rendered rules.
const PRODUCER_VAULT_PROXY: Address =
    Address::new(alloy_primitives::hex!("00000000000000000000000000000000000ab001"));
/// An address no rendered rule declares, for the suppression control.
const UNDECLARED_TARGET: Address =
    Address::new(alloy_primitives::hex!("00000000000000000000000000000000000f00f0"));

/// The producer's rendered `GET /rules` body, verbatim.
const RENDERED_RESPONSE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../rcs-filter/tests/data/producer-rendered-rules-response.json"
));

/// The rendered `rules` array as raw text, sliced out without parsing so the rule
/// bytes the consumer decodes are byte-for-byte the producer's.
fn rendered_rules_array() -> &'static str {
    const KEY: &str = "\"rules\":";
    let start = RENDERED_RESPONSE.find(KEY).expect("the rendered body carries rules") + KEY.len();
    let end = RENDERED_RESPONSE.rfind(']').expect("the rules array is closed") + 1;
    let slice = RENDERED_RESPONSE[start..end].trim();
    assert!(
        slice.starts_with('[') && slice.ends_with(']'),
        "the slice must be exactly the rules array: {slice:.40}"
    );
    slice
}

/// The audit path's database with the producer's OWN proxy address delegating into
/// `target`, plus that target's code.
fn producer_proxy_db(target: Address, target_code: Vec<u8>) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: alloy_primitives::U256::from(10u128.pow(18)), ..Default::default() },
    );
    for (address, code) in [
        (PRODUCER_VAULT_PROXY, delegating_proxy_code(target, 1)),
        (target, target_code),
        (UNDECLARED_TARGET, impl_code(0x09)),
    ] {
        db.insert_account_info(
            address,
            AccountInfo { code: Some(Bytecode::new_raw(code.into())), ..Default::default() },
        );
    }
    db
}

/// Decodes the producer's rendered rules plus one deposit to `to` through the audit
/// RPC's real request decode, then runs the complete production batch against a real EVM.
///
/// Returns the number of accepted rules, the verdict, the observation set the batch
/// itself fed to screening, and the depositor nonce afterwards (the commit
/// observable).
fn adjudicate_rendered(
    to: Address,
    target: Address,
    target_code: Vec<u8>,
) -> (usize, Verdict, Option<Vec<rcs_filter::callstack::CallObservation>>, u64) {
    let body = format!(
        r#"{{"rules":{rules},"txs":[{{"source_hash":"{source:#x}","from":"{from:#x}","to":"{to:#x}","mint":"0","value":"0","gas_limit":1000000,"is_system_transaction":false,"data":"0x"}}]}}"#,
        rules = rendered_rules_array(),
        source = B256::repeat_byte(0x51),
        from = DEPOSITOR,
        to = to,
    );
    // The production decode, straight from the request text — no Value hop, and the
    // rendered rule bytes still exactly as the producer wrote them.
    let request: AuditTransactionsRequest = serde_json::from_str(&body)
        .expect("the producer's rendered rules must decode at this entry point");
    assert_eq!(request.txs.len(), 1, "the batch really carries a transaction");

    let rules = load_rules(1, 0, request.rules.clone());
    assert!(rules.rejected.is_empty(), "no rendered rule may be rejected: {:?}", rules.rejected);
    let accepted = rules.rules.len();

    let mut db = producer_proxy_db(target, target_code);
    let _ = observation_recorder::take();
    let results =
        execute_audit_batch(&request, &mut db, audit_env()).expect("the production batch executes");
    let recorded = observation_recorder::take();
    assert_eq!(recorded.len(), 1);
    let verdict = results[0].verdict;
    let nonce = db
        .basic(DEPOSITOR)
        .expect("the depositor is readable")
        .expect("the fixture seeds the depositor")
        .nonce;
    (accepted, verdict, recorded[0].clone(), nonce)
}

/// All 7 rendered rules are accepted at the audit RPC entry point, and the condition
/// of the producer's own `deny-proxy-delegatecall-to-unapproved-implementation` really
/// fires on a real execution: the proxy delegatecalls into an implementation whose
/// executed bytecode hash is not on the producer's allowlist, and the deposit is
/// denied with its state discarded.
#[test]
fn the_rendered_rules_are_accepted_and_their_condition_fires_on_a_real_execution() {
    let target = APPROVED_IMPL; // address is irrelevant to this rule; the HASH is not
    let (accepted, verdict, observed, nonce) =
        adjudicate_rendered(PRODUCER_VAULT_PROXY, target, impl_code(0x01));

    assert_eq!(accepted, 7, "all seven rendered rules are active");

    // The execution really produced the frame the rule is about.
    let items = observed.expect("the batch fed screening an AVAILABLE observation set");
    let frame = items
        .iter()
        .find(|o| {
            o.kind == rcs_filter::callstack::CallObservationKind::DirectDelegatecall
                && o.address == PRODUCER_VAULT_PROXY
        })
        .expect("the proxy's direct delegatecall was observed");
    assert_eq!(frame.impl_address, Some(target));
    assert_eq!(
        frame.impl_code_hash,
        Some(alloy_primitives::keccak256(impl_code(0x01))),
        "the observed hash is the bytecode that really executed"
    );

    assert_eq!(
        verdict,
        Verdict::Deny,
        "the producer's own allowlist rule must object to an implementation that is \
             not on it"
    );
    assert_eq!(nonce, 0, "a denied deposit's state is discarded");
}

/// The suppression control on the same rendered set: a deposit that touches none of
/// the three declared proxies is allowed and commits, even though every alias-bearing
/// rendered rule is a NEGATED allowlist. Without this, the denial above could be an
/// unconditional objection rather than an evaluated one.
#[test]
fn a_deposit_touching_no_declared_proxy_is_allowed_and_commits() {
    let (accepted, verdict, observed, nonce) =
        adjudicate_rendered(UNDECLARED_TARGET, APPROVED_IMPL, impl_code(0x01));
    assert_eq!(accepted, 7);
    let items = observed.expect("the batch fed screening an observation set");
    assert!(
        !items.iter().any(|o| {
            o.kind == rcs_filter::callstack::CallObservationKind::DirectDelegatecall
                && o.address == PRODUCER_VAULT_PROXY
        }),
        "no declared proxy frame was observed: {items:?}"
    );
    assert_eq!(verdict, Verdict::Allow);
    assert_eq!(nonce, 1, "an allowed deposit commits its state");
}

// ------------------------------------------------------- diagnostic wire contract -------

/// A proxy that delegatecalls the address carried in its calldata and, with empty calldata,
/// simply stops: `CALLDATASIZE; PUSH1 5; JUMPI; STOP; JUMPDEST; DELEGATECALL(gas,
/// calldataload(0), 0,0,0,0); POP; STOP`.
///
/// Calldata is what lets ONE request carry both an approved and an unapproved execution
/// against the same observed proxy.
fn calldata_delegating_proxy_code() -> Vec<u8> {
    vec![
        0x36, // CALLDATASIZE
        0x60, 0x05, // PUSH1 5 (the delegate branch)
        0x57, // JUMPI
        0x00, // STOP
        0x5b, // JUMPDEST
        0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60,
        0x00, // retSize, retOffset, argsSize, argsOffset
        0x60, 0x00, 0x35, // PUSH1 0; CALLDATALOAD -> the target address
        0x5a, 0xf4, // GAS; DELEGATECALL
        0x50, 0x00, // POP; STOP
    ]
}

/// The observed proxy, its two implementations and the depositor.
fn calldata_db() -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    db.insert_account_info(
        PROXY,
        AccountInfo {
            code: Some(Bytecode::new_raw(calldata_delegating_proxy_code().into())),
            ..Default::default()
        },
    );
    for (address, marker) in [(APPROVED_IMPL, 0x01u8), (UNAPPROVED_IMPL, 0x02u8)] {
        db.insert_account_info(
            address,
            AccountInfo {
                code: Some(Bytecode::new_raw(impl_code(marker).into())),
                ..Default::default()
            },
        );
    }
    db
}

/// One deposit request body, spelled out as the caller sends it.
fn deposit_body(source_hash: &str, from: Address, to: Address, data: &str) -> serde_json::Value {
    serde_json::json!({
        "source_hash": source_hash,
        "from": format!("{from:#x}"),
        "to": format!("{to:#x}"),
        "mint": "0",
        "value": "0",
        "gas_limit": 1_000_000,
        "is_system_transaction": false,
        "data": data,
    })
}

/// The calldata word that makes the fixture proxy delegatecall `target`.
fn delegate_calldata(target: Address) -> String {
    format!("0x{}", alloy_primitives::hex::encode(target.into_word()))
}

/// Decodes the raw parameter array exactly as the server does, runs the production batch and
/// serializes the response the handler would return.
fn decode_run_and_serialize(raw_params: &str, db: &mut InMemoryDB) -> Vec<serde_json::Value> {
    let req = decode_params(raw_params).expect("the request decodes");
    let results = execute_audit_batch(&req, db, audit_env()).expect("the batch executes");
    let value = serde_json::to_value(AuditTransactionsResponse { results })
        .expect("the response serializes");
    value["results"].as_array().expect("a results array").clone()
}

/// A result object's keys, sorted.
fn sorted_keys(entry: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> =
        entry.as_object().expect("a result object").keys().cloned().collect();
    keys.sort();
    keys
}

/// A request that declares no call-frame selector keeps the four-field response shape, even
/// though the entry was executed and denied.
#[test]
fn a_selector_free_response_entry_has_exactly_the_four_base_fields() {
    let body = serde_json::json!([{
        "rules": [serde_json::json!({
            "id": "evt",
            "event_abis": { "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    { "name": "from", "type": "address", "indexed": true },
                    { "name": "to", "type": "address", "indexed": true },
                    { "name": "value", "type": "uint256", "indexed": false }
                ],
                "anonymous": false
            } },
            "condition": true,
            "action": "deny"
        })],
        "txs": [deposit_body(&format!("0x{}", "57".repeat(32)), DEPOSITOR, PROXY, "0x")],
        "known_allowed": [],
    }]);

    let entries = decode_run_and_serialize(&body.to_string(), &mut calldata_db());
    assert_eq!(entries.len(), 1, "one entry per request transaction");
    assert_eq!(entries[0]["verdict"], "deny", "the event-only rule fires");
    assert_eq!(
        sorted_keys(&entries[0]),
        vec!["actions", "source_hash", "tx_hash", "verdict"],
        "exactly the four base fields on a selector-free response"
    );
}

/// A request that declares a call-frame selector keeps the SAME four-field response shape on
/// every entry: the response carries no diagnostic field, and an entry whose deposit could not
/// be built carries the same four keys.
#[test]
fn a_selector_bearing_response_entry_has_exactly_the_four_base_fields() {
    let body = serde_json::json!([{
        "rules": [proxy_impl_allowlist_rule_json()],
        "txs": [
            deposit_body(
                &format!("0x{}", "58".repeat(32)),
                DEPOSITOR,
                PROXY,
                &delegate_calldata(APPROVED_IMPL),
            ),
            deposit_body(
                &format!("0x{}", "59".repeat(32)),
                DEPOSITOR,
                PROXY,
                &delegate_calldata(UNAPPROVED_IMPL),
            ),
            serde_json::json!({
                "source_hash": format!("0x{}", "5a".repeat(32)),
                "from": "not-an-address",
                "to": format!("{PROXY:#x}"),
                "mint": "0",
                "value": "0",
                "gas_limit": 1_000_000,
                "is_system_transaction": false,
                "data": "0x",
            }),
        ],
        "known_allowed": [],
    }]);

    let entries = decode_run_and_serialize(&body.to_string(), &mut calldata_db());
    assert_eq!(entries.len(), 3, "three entries: two executed and one unbuildable");

    assert_eq!(entries[0]["verdict"], "allow", "approved implementation");
    assert_eq!(entries[1]["verdict"], "deny", "unapproved implementation");
    assert_eq!(entries[2]["verdict"], "malformed", "the deposit cannot be built");
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(
            sorted_keys(entry),
            vec!["actions", "source_hash", "tx_hash", "verdict"],
            "entry {index} must carry exactly the four base fields: {entry}"
        );
    }
}
