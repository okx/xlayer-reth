//! The four condition variables a direct-delegatecall alias binds: the selected proxy's own
//! address and executed code hash, and the delegate target's address and executed code hash.

use super::*;

fn approved_proxy_code() -> B256 {
    proxy_code()
}

fn replaced_proxy_code() -> B256 {
    B256::repeat_byte(0xa2)
}

/// "Deny unless every one of the four fields is on its allowlist" — the shape an operator writes
/// to pin both the proxy's own code and the code it delegates into.
fn four_field_allowlist(
    alias: &str,
    proxy_address: Address,
    proxy_hashes: &[B256],
    targets: &[Address],
    target_hashes: &[B256],
) -> Value {
    let hashes =
        |list: &[B256]| list.iter().map(|h| Value::String(hex_hash(*h))).collect::<Vec<_>>();
    let addresses = targets.iter().map(|a| Value::String(hex_addr(*a))).collect::<Vec<_>>();
    json!({ "!": { "and": [
        { "==": [ { "var": format!("{alias}.proxyAddress") }, hex_addr(proxy_address) ] },
        { "in": [ { "var": format!("{alias}.proxyCodeHash") }, hashes(proxy_hashes) ] },
        { "in": [ { "var": format!("{alias}.delegateTo") }, addresses ] },
        { "in": [ { "var": format!("{alias}.delegateCodeHash") }, hashes(target_hashes) ] }
    ]}})
}

fn four_field_guard() -> RuleSet {
    rule_set(vec![deny_on_alias(
        "four-field-guard",
        "p",
        proxy(),
        four_field_allowlist(
            "p",
            proxy(),
            &[approved_proxy_code()],
            &[implementation()],
            &[approved_impl_code()],
        ),
    )])
}

fn observed(proxy_hash: B256, target: Address, target_hash: B256) -> CallObservation {
    CallObservation::direct_delegatecall(proxy(), proxy_hash, target, target_hash)
}

#[test]
fn all_four_fields_on_their_allowlists_is_allowed() {
    let rules = four_field_guard();
    let items = [observed(approved_proxy_code(), implementation(), approved_impl_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Allow));
}

#[test]
fn an_unapproved_proxy_code_hash_is_denied_even_with_an_approved_target() {
    let rules = four_field_guard();
    let items = [observed(replaced_proxy_code(), implementation(), approved_impl_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));
}

#[test]
fn an_unapproved_target_address_or_target_code_hash_is_denied() {
    let rules = four_field_guard();
    let wrong_target = [observed(approved_proxy_code(), library(), approved_impl_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &wrong_target)), Ok(MatchOutcome::Deny));
    let wrong_target_code = [observed(approved_proxy_code(), implementation(), rogue_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &wrong_target_code)), Ok(MatchOutcome::Deny));
}

#[test]
fn the_proxy_variables_are_the_selected_address_and_its_executed_code_hash_in_lowercase() {
    // A checksummed-looking address and a hash with letters, so the lowercase rendering is what
    // the equality actually depends on.
    let rules = rule_set(vec![deny_on_alias(
        "exact",
        "p",
        proxy(),
        json!({ "and": [
            { "==": [ { "var": "p.proxyAddress" }, "0x1111111111111111111111111111111111111111" ] },
            { "==": [ { "var": "p.proxyCodeHash" }, format!("0x{}", "ab".repeat(32)) ] }
        ]}),
    )]);
    let items = [observed(B256::repeat_byte(0xab), implementation(), approved_impl_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));

    // The same rule against a different executed proxy hash does not match.
    let other = [observed(B256::repeat_byte(0xac), implementation(), approved_impl_code())];
    assert_eq!(screen(&rules, &[], captured(&rules, &other)), Ok(MatchOutcome::Allow));
}

#[test]
fn several_triggers_of_one_proxy_deny_when_any_one_is_unapproved_in_either_order() {
    let rules = four_field_guard();
    let approved = observed(approved_proxy_code(), implementation(), approved_impl_code());
    let unapproved = observed(approved_proxy_code(), implementation(), rogue_code());

    for items in [[approved, unapproved], [unapproved, approved]] {
        assert_eq!(screen(&rules, &[], captured(&rules, &items)), Ok(MatchOutcome::Deny));
        // Each trigger is one candidate, so the binding budget is charged twice.
        assert_eq!(
            screen_with_limit(&rules, &[], captured(&rules, &items), 2),
            Ok(MatchOutcome::Deny),
            "two candidates fit a budget of two"
        );
        assert_eq!(
            screen_with_limit(&rules, &[], captured(&rules, &items), 1),
            Err(MatchError::BindingBudgetExceeded {
                rule_id: "four-field-guard".to_string(),
                attempted: 2,
                limit: 1,
            }),
            "two triggers are two candidates"
        );
    }

    // Two approved triggers stay allowed.
    let both_approved = [approved, approved];
    assert_eq!(screen(&rules, &[], captured(&rules, &both_approved)), Ok(MatchOutcome::Allow));
}

#[test]
fn a_nested_proxy_binds_only_its_own_alias() {
    // P delegates into I, I calls proxy Q, and Q delegates into J. Capture reports one
    // observation per selected proxy; each alias binds only its own.
    let rules = rule_set(vec![json!({
        "id": "nested",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": {
            "outer": hex_addr(proxy()),
            "inner": hex_addr(other_proxy())
        }},
        "condition": { "or": [
            { "!=": [ { "var": "outer.proxyAddress" }, hex_addr(proxy()) ] },
            { "!=": [ { "var": "inner.proxyAddress" }, hex_addr(other_proxy()) ] },
            { "!": { "in": [ { "var": "inner.proxyCodeHash" }, [hex_hash(approved_proxy_code())] ] } }
        ]},
        "action": "deny"
    })]);
    let outer = observed(approved_proxy_code(), implementation(), approved_impl_code());
    let inner_approved = CallObservation::direct_delegatecall(
        other_proxy(),
        approved_proxy_code(),
        library(),
        approved_impl_code(),
    );
    let inner_replaced = CallObservation::direct_delegatecall(
        other_proxy(),
        replaced_proxy_code(),
        library(),
        approved_impl_code(),
    );
    let fine = [outer, inner_approved];
    assert_eq!(screen(&rules, &[], captured(&rules, &fine)), Ok(MatchOutcome::Allow));
    let replaced = [outer, inner_replaced];
    assert_eq!(screen(&rules, &[], captured(&rules, &replaced)), Ok(MatchOutcome::Deny));
}

#[test]
fn the_four_provided_names_are_accepted_and_only_with_exact_spelling() {
    for field in ["proxyAddress", "proxyCodeHash", "delegateTo", "delegateCodeHash"] {
        assert!(
            compile_rule(raw(deny_on_alias(
                "r",
                "p",
                proxy(),
                json!({ "==": [ { "var": format!("p.{field}") }, null ] })
            )))
            .is_ok(),
            "p.{field} is provided"
        );
    }
    for field in ["codeHash", "address", "implCodeHash", "delegateto", "proxycodehash", "proxyAddr"]
    {
        assert_eq!(
            reject_reason(deny_on_alias(
                "r",
                "p",
                proxy(),
                json!({ "==": [ { "var": format!("p.{field}") }, null ] })
            )),
            format!(
                "condition references unknown variable 'p.{field}'; direct_delegatecalls provides \
                 proxyAddress, proxyCodeHash, delegateTo and delegateCodeHash"
            )
        );
    }
}

#[test]
fn a_proxy_field_name_on_an_event_alias_is_not_touched_by_the_alias_guard() {
    // `transfer` is an event alias, not a call-frame alias, so the guard leaves it alone and the
    // event-only rule loads and evaluates exactly as before.
    let rules = rule_set(vec![json!({
        "id": "event-only",
        "event_abis": { "transfer": transfer_abi() },
        "condition": { "==": [ { "var": "transfer.proxyCodeHash" }, null ] },
        "action": "deny"
    })]);
    use crate::test_support::log_builder::erc20_transfer;
    let logs = vec![erc20_transfer(proxy(), library(), implementation(), U256::from(1u64))];
    assert_eq!(
        screen(&rules, &logs, Observations::available(rules.stamp(), &[])),
        Ok(MatchOutcome::Deny),
        "an undeclared event field binds nothing and reads as null, as it always did"
    );
}
