//! `GET /rules` response-root schema: an unknown root-level key rejects the whole response,
//! and a correct response root still decodes.

use super::*;

/// an unknown key at the `GET /rules` response **root** rejects the whole response.
/// `RulesResponse` never reaches `compile_rule`, so there is no per-rule gate to carry this
/// failure: the conversion itself must fail, which surfaces on the existing decode path and
/// leaves `install_rules` unreached.
#[test]
fn unknown_root_level_field_rejects_the_whole_response() {
    let error = serde_json::from_value::<RulesResponse>(json!({
        "protocol_version": 1,
        "content_version": 1,
        "rules": [],
        "contentVersion": 2
    }))
    .expect_err("an unknown root key must reject the response");
    assert!(
        error.to_string().contains("unknown field(s) in rules response: contentVersion"),
        "{error}"
    );
}

/// Control: a correct response root still decodes, and the required fields stay
/// required (covered for the missing-field direction by
/// `required_response_fields_fail_decoding_when_missing` above).
#[test]
fn correct_response_root_still_decodes() {
    let response = serde_json::from_value::<RulesResponse>(json!({
        "protocol_version": 2,
        "content_version": 9,
        "rules": []
    }))
    .expect("a correct response root must still decode");
    assert_eq!(response.protocol_version, 2);
    assert_eq!(response.content_version, 9);
    assert!(response.rules.is_empty());
}
