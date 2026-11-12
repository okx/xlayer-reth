use super::*;
use serde_json::json;

fn binds(pairs: &[(&str, Value)]) -> Bindings {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

#[test]
fn compiled_matches_truthy_for_all_shapes() {
    let mut interner = AddressSetInterner::new();
    let a = "0x0101010101010101010101010101010101010101";
    let b = "0x0202020202020202020202020202020202020202";
    let conds = [
        json!({"in": [{"var": "origin"}, [a, b]]}),
        json!({"in": [{"var": "origin"}, "0xdead"]}), // substring
        json!({"==": [{"var": "origin"}, a]}),
        json!({"and": [{"in": [{"var": "origin"}, [a]]}, {"==": [{"var": "x"}, 1]}]}),
        json!({"or":  [{"in": [{"var": "origin"}, [a]]}, {"==": [{"var": "x"}, 1]}]}),
        json!({"!": {"in": [{"var": "origin"}, [a]]}}),
        json!({"and": []}),
        json!({"or":  []}),
    ];
    let binding_sets = [
        binds(&[("origin", json!(a))]),
        binds(&[("origin", json!(b))]),
        binds(&[("origin", json!("0xdeadbeef")), ("x", json!(1))]),
        Bindings::new(),
    ];
    for cond in &conds {
        assert!(validate(cond).is_ok());
        let compiled = compile(cond, &mut interner);
        for bset in &binding_sets {
            assert_eq!(eval_compiled(&compiled, bset), truthy(cond, bset), "cond={cond}");
        }
    }
}

const A: &str = "0x0101010101010101010101010101010101010101";
const B: &str = "0x0202020202020202020202020202020202020202";

#[test]
fn recognizes_only_static_all_address_arrays() {
    let mut i = AddressSetInterner::new();
    assert!(matches!(
        compile(&json!({"in": [{"var":"origin"}, [A, B]]}), &mut i),
        CompiledCondition::AddressSetIn { .. }
    ));
    // non-accelerable → Raw
    for cond in [
        json!({"in": [{"var":"origin"}, [A, 1]]}), // mixed
        json!({"in": [{"var":"origin"}, [A, "not-an-address"]]}), // bad element
        json!({"in": [{"var":"origin"}, []]}),     // empty
        json!({"in": [{"var":"origin"}, {"var":"list"}]}), // dynamic RHS
        json!({"in": [{"var":"origin"}, "0xdead"]}), // substring
        json!({"in": [1, [1, 2]]}),                // numeric array
    ] {
        assert!(matches!(compile(&cond, &mut i), CompiledCondition::Raw(_)), "cond={cond}");
    }
}

#[test]
fn fast_path_hit_miss_case_and_duplicates_match_linear() {
    let mut i = AddressSetInterner::new();
    // checksummed + lowercase + duplicate entries in the list
    let cond = json!({"in": [{"var":"origin"}, ["0x52908400098527886E0F7030069857D2E4169EE7",
                                                 "0x52908400098527886e0f7030069857d2e4169ee7", B]]});
    let compiled = compile(&cond, &mut i);
    for probe in [json!("0x52908400098527886e0f7030069857d2e4169ee7"), json!(B), json!(A)] {
        let bset = binds(&[("origin", probe.clone())]);
        assert_eq!(eval_compiled(&compiled, &bset), truthy(&cond, &bset), "probe={probe}");
    }
}

#[test]
fn non_address_needle_falls_back_to_linear() {
    let mut i = AddressSetInterner::new();
    let cond = json!({"in": [{"var":"origin"}, [A, B]]});
    let compiled = compile(&cond, &mut i);
    for probe in [Value::Null, json!(123), json!("not-an-address")] {
        let bset = binds(&[("origin", probe.clone())]);
        assert_eq!(eval_compiled(&compiled, &bset), truthy(&cond, &bset), "probe={probe}");
    }
    // missing var → null needle
    assert_eq!(eval_compiled(&compiled, &Bindings::new()), truthy(&cond, &Bindings::new()));
}

#[test]
fn identical_normalized_lists_share_one_arc() {
    let mut i = AddressSetInterner::new();
    // same set, different order + case + a duplicate
    let c1 = compile(&json!({"in": [{"var":"origin"}, [A, B]]}), &mut i);
    let c2 = compile(&json!({"in": [{"var":"origin"}, [B, A, A]]}), &mut i);
    let c3 = compile(&json!({"in": [{"var":"origin"}, [A]]}), &mut i); // different set
    let get = |c: &CompiledCondition| match c {
        CompiledCondition::AddressSetIn { set, .. } => set.clone(),
        _ => panic!("expected AddressSetIn"),
    };
    assert!(Arc::ptr_eq(&get(&c1), &get(&c2)));
    assert!(!Arc::ptr_eq(&get(&c1), &get(&c3)));
}

#[test]
fn combinators_with_accelerable_child_are_specialized_and_exact() {
    let mut i = AddressSetInterner::new();
    let a = A;
    for cond in [
        json!({"and": [{"in": [{"var":"origin"}, [a]]}, {"==": [{"var":"x"}, 1]}]}),
        json!({"or":  [{"==": [{"var":"x"}, 1]}, {"in": [{"var":"origin"}, [a]]}]}),
        json!({"!": {"in": [{"var":"origin"}, [a]]}}),
    ] {
        let compiled = compile(&cond, &mut i);
        assert!(!matches!(compiled, CompiledCondition::Raw(_)), "should specialize: {cond}");
        assert!(contains_address_set(&compiled));
        for bset in [
            binds(&[("origin", json!(a)), ("x", json!(1))]),
            binds(&[("origin", json!(B)), ("x", json!(2))]),
            Bindings::new(),
        ] {
            assert_eq!(eval_compiled(&compiled, &bset), truthy(&cond, &bset), "cond={cond}");
        }
    }
}

#[test]
fn empty_and_or_and_non_accelerable_combinators_stay_raw_and_false() {
    let mut i = AddressSetInterner::new();
    for cond in [json!({"and": []}), json!({"or": []})] {
        let compiled = compile(&cond, &mut i);
        assert!(
            matches!(compiled, CompiledCondition::Raw(_)),
            "empty combinator must stay Raw: {cond}"
        );
        assert!(!eval_compiled(&compiled, &Bindings::new())); // false, matches eval→Null
    }
    // combinator with no accelerable child stays Raw
    let cond = json!({"and": [{"==": [{"var":"x"}, 1]}, {"==": [{"var":"y"}, 2]}]});
    assert!(matches!(compile(&cond, &mut i), CompiledCondition::Raw(_)));
}

// Negative boundary: a static all-address `in` reached through a NON-boolean operator must
// never be lifted to `AddressSetIn`. `compile` recurses only into `and`/`or`/`!` (plus the
// top-level `in`), so any other parent falls through to the single `Raw` catch-all and the
// whole node stays `Raw`, evaluated verbatim by the linear evaluator. This test locks that
// scope so a future change cannot silently widen acceleration under a non-boolean operator.
#[test]
fn in_under_non_boolean_operator_stays_raw_and_matches_linear() {
    let mut i = AddressSetInterner::new();
    let a = A;
    let b = B;
    // Each condition embeds a static all-address `in` under a non-boolean parent.
    let conds = [
        json!({"==": [{"in": [{"var":"origin"}, [a, b]]}, true]}), // `in` as an `==` operand
        json!({"var": ["x", {"in": [{"var":"origin"}, [a, b]]}]}), // `in` inside a `var` default
        json!({">":  [{"in": [{"var":"origin"}, [a, b]]}, 0]}),    // `in` inside a numeric compare
    ];
    let bindings = [
        binds(&[("origin", json!(a))]),      // hit (listed)
        binds(&[("origin", json!(b))]),      // listed
        binds(&[("origin", json!("nope"))]), // non-address needle
        Bindings::new(),                     // missing var → null needle
    ];
    let miss = binds(&[("origin", json!("0x0303030303030303030303030303030303030303"))]);
    for cond in &conds {
        assert!(validate(cond).is_ok(), "cond must validate: {cond}");
        let compiled = compile(cond, &mut i);
        // (a) overall Raw — the `in` was not specialized anywhere in the tree
        assert!(matches!(compiled, CompiledCondition::Raw(_)), "must stay overall Raw: {cond}");
        assert!(!contains_address_set(&compiled), "no AddressSetIn may appear: {cond}");
        // (b) truthiness-exact vs the linear evaluator across the required bindings
        for bset in bindings.iter().chain(std::iter::once(&miss)) {
            assert_eq!(eval_compiled(&compiled, bset), truthy(cond, bset), "cond={cond}");
        }
    }
}

#[test]
fn literal_true_is_truthy() {
    assert!(truthy(&json!(true), &Bindings::new()));
}

#[test]
fn empty_and_or_are_valid_and_falsy() {
    for condition in [json!({"and": []}), json!({"or": []})] {
        assert!(validate(&condition).is_ok());
        assert_eq!(eval(&condition, &Bindings::new()), Value::Null);
        assert!(!truthy(&condition, &Bindings::new()));
    }
}

#[test]
fn eq_and_and_match_scenario_a() {
    // condition a: transfer.address == TOKEN_X AND transfer.from == BRIDGE
    let cond = json!({"and": [
        {"==": [{"var": "transfer.address"}, "0x0202020202020202020202020202020202020202"]},
        {"==": [{"var": "transfer.from"},    "0x0101010101010101010101010101010101010101"]}
    ]});
    let b = binds(&[
        ("transfer.address", json!("0x0202020202020202020202020202020202020202")),
        ("transfer.from", json!("0x0101010101010101010101010101010101010101")),
    ]);
    assert!(truthy(&cond, &b));
}

#[test]
fn in_list_blacklist() {
    let cond =
        json!({"in": [{"var": "transfer.from"}, ["0x0606060606060606060606060606060606060606"]]});
    let hit = binds(&[("transfer.from", json!("0x0606060606060606060606060606060606060606"))]);
    let miss = binds(&[("transfer.from", json!("0x0101010101010101010101010101010101010101"))]);
    assert!(truthy(&cond, &hit));
    assert!(!truthy(&cond, &miss));
}

#[test]
fn numeric_gt_18_digit_precision() {
    // one token = 1e18; ensure big-int comparison is exact (no f64 loss).
    let cond = json!({">": [{"var": "transfer.value"}, 1000000000000000000u64]});
    let over = binds(&[("transfer.value", json!("2000000000000000000"))]);
    let equal = binds(&[("transfer.value", json!("1000000000000000000"))]);
    assert!(truthy(&cond, &over));
    assert!(!truthy(&cond, &equal)); // strictly greater
}

#[test]
fn missing_var_is_null_and_comparisons_are_false() {
    let cond = json!({"==": [{"var": "transfer.from"}, "0x01"]});
    assert!(!truthy(&cond, &Bindings::new())); // null != literal
    let gt = json!({">": [{"var": "transfer.value"}, 1]});
    assert!(!truthy(&gt, &Bindings::new())); // null comparison → false
}

#[test]
fn negation_and_neq() {
    let neq = json!({"!=": [{"var": "origin"}, "0x01"]});
    assert!(truthy(&neq, &binds(&[("origin", json!("0x02"))])));
    let not = json!({"!": {"==": [{"var": "origin"}, "0x01"]}});
    assert!(truthy(&not, &binds(&[("origin", json!("0x02"))])));
}

#[test]
fn nested_and_or_return_operands() {
    let cond = json!({"==": [
        {"and": [true, {"or": [false, "selected"]}]},
        "selected"
    ]});
    assert!(truthy(&cond, &Bindings::new()));
}

#[test]
fn ordinary_strings_are_case_sensitive_but_addresses_are_not() {
    assert!(!truthy(&json!({"==": ["Quota", "quota"]}), &Bindings::new()));
    assert!(truthy(
        &json!({"==": [
            "0x52908400098527886E0F7030069857D2E4169EE7",
            "0x52908400098527886e0f7030069857d2e4169ee7"
        ]}),
        &Bindings::new()
    ));
}

#[test]
fn validates_all_supported_operators_and_rejects_bad_shapes() {
    for expression in [
        json!({"var": "x"}),
        json!({"==": [1, 1]}),
        json!({"!=": [1, 2]}),
        json!({"in": [1, [1]]}),
        json!({"and": [true, true]}),
        json!({"or": [false, true]}),
        json!({"!": true}),
        json!({">": [2, 1]}),
        json!({">=": [2, 1]}),
        json!({"<": [1, 2]}),
        json!({"<=": [1, 2]}),
    ] {
        validate(&expression).expect("supported expression");
    }
    for expression in [
        json!({"unknown": [1]}),
        json!({"==": [1]}),
        json!({"!": [true, false]}),
        json!({"and": true}),
        json!({"or": false}),
    ] {
        assert!(validate(&expression).is_err(), "accepted {expression}");
    }
}

#[test]
fn u256_boundaries_compare_exactly() {
    let max = U256::MAX.to_string();
    let below = (U256::MAX - U256::from(1)).to_string();
    assert!(truthy(&json!({">": [max, below]}), &Bindings::new()));
    assert!(truthy(&json!({"<=": ["0", max]}), &Bindings::new()));
}
