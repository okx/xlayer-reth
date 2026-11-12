//! Duplicate rule keys refused at the **audit RPC request** entry point, driven from raw JSON-RPC parameter
//! text through jsonrpsee's own parameter decoder.
//!
//! Why the decoder and not `serde_json::from_value`: `jsonrpsee`'s generated server body for
//! `audit_transactions(req: AuditTransactionsRequest)` is `params.sequence()` followed by
//! `seq.next::<AuditTransactionsRequest>()`, and `ParamsSequence::next` streams the raw parameter
//! text straight into `serde_json::Deserializer::from_str`. Calling it here is therefore the same
//! decode the running server performs — byte for byte, with duplicate keys still present. A test
//! that built the body as a `serde_json::Value` first would have collapsed the duplicate before
//! the decode ever saw it, and so could not have detected anything.

use super::*;
use jsonrpsee::types::Params;

const PROXY: &str = "0x00000000000000000000000000000000000000aa";
const OTHER_PROXY: &str = "0x00000000000000000000000000000000000000bb";

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
        r#"{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY}"}}}},"condition":{{"!":{{"in":[{{"var":"p.delegateCodeHash"}},["0x{hash}"]]}}}},"action":"deny"}}"#,
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
                r#""callstack":{{"direct_delegatecalls":{{"q":"{OTHER_PROXY}"}}}},"action":"deny""#
            ),
            "a second callstack section",
        ),
    ];
    for (tail, label) in tails {
        let rule = format!(
            r#"{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY}"}}}},"condition":true,{tail}}}"#
        );
        assert_invalid_params(decode_params(&params_with_rule(&rule)), "duplicate field", label);
    }
}

/// A duplicate `direct_delegatecalls` alias key, raw, through the real request
/// decode. Specifically NOT "accepted with the last value".
#[test]
fn duplicate_alias_keys_are_refused_by_the_real_request_decode() {
    for (first, second, label) in [
        (PROXY, PROXY, "same value"),
        (PROXY, OTHER_PROXY, "different values"),
        (OTHER_PROXY, PROXY, "swapped order"),
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
    assert_eq!(req.protocol_version, MIN_PROTOCOL_VERSION, "an omitted version means 1");
    assert_eq!(req.rules.len(), 1);
    let plan = AuditBatchPlan::prepare(&req).expect("the legal control passes every check");
    assert!(plan.rules().rejected.is_empty(), "{:?}", plan.rules().rejected);
    assert_eq!(plan.rules().rules.len(), 1);
}

/// The executable counterexample that makes the two rows above falsifiable: route the same
/// bytes through `serde_json::Value` and both duplicate classes are **accepted**, one
/// occurrence silently discarded. Production has no such hop — which
/// [`no_value_hop_exists_on_the_production_audit_request_path`] pins — and this is
/// exactly the acceptance that would reappear if it ever grew one.
#[test]
fn the_value_hop_this_module_excludes_would_accept_the_duplicate() {
    let root_dup = format!(
        r#"{{"rules":[{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY}"}}}},"condition":true,"action":"deny","action":"allow"}}],"txs":[],"known_allowed":[]}}"#
    );
    let alias_dup = format!(
        r#"{{"rules":[{{"id":"r","event_abis":{{}},"callstack":{{"direct_delegatecalls":{{"p":"{PROXY}","p":"{OTHER_PROXY}"}}}},"condition":true,"action":"deny"}}],"txs":[],"known_allowed":[]}}"#
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
    const SOURCE: &str = include_str!("../src/audit/rpc.rs");
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
