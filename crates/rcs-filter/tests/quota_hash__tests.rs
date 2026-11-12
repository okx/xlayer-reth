use super::*;

fn quota(value: &str) -> BTreeMap<String, Vec<ActionItem>> {
    let mut params = BTreeMap::new();
    params.insert(
        "from".to_string(),
        serde_json::Value::String("0x0101010101010101010101010101010101010101".to_string()),
    );
    params.insert(
        "to".to_string(),
        serde_json::Value::String("0x0303030303030303030303030303030303030303".to_string()),
    );
    params.insert("value".to_string(), serde_json::Value::String(value.to_string()));
    let mut m = BTreeMap::new();
    m.insert(
        "quota".to_string(),
        vec![ActionItem {
            name: "transfer".to_string(),
            address: "0x0202020202020202020202020202020202020202".to_string(),
            params,
        }],
    );
    m
}

#[test]
fn identical_content_hashes_equal() {
    assert_eq!(
        encode_and_hash(&quota("1000000000000000000")),
        encode_and_hash(&quota("1000000000000000000"))
    );
}

#[test]
fn param_insertion_order_hashes_equal() {
    // Same semantic content, params inserted in different order → canonical (BTreeMap)
    // ordering makes the hash identical; this is not a JSON-string hash.
    let mut a_params = BTreeMap::new();
    a_params.insert(
        "from".to_string(),
        serde_json::Value::String("0x0101010101010101010101010101010101010101".to_string()),
    );
    a_params
        .insert("value".to_string(), serde_json::Value::String("1000000000000000000".to_string()));
    a_params.insert(
        "to".to_string(),
        serde_json::Value::String("0x0303030303030303030303030303030303030303".to_string()),
    );
    let mut a = BTreeMap::new();
    a.insert(
        "quota".to_string(),
        vec![ActionItem {
            name: "transfer".to_string(),
            address: "0x0202020202020202020202020202020202020202".to_string(),
            params: a_params,
        }],
    );
    assert_eq!(encode_and_hash(&a), encode_and_hash(&quota("1000000000000000000")));
}

#[test]
fn different_value_hashes_differ() {
    assert_ne!(
        encode_and_hash(&quota("1000000000000000000")),
        encode_and_hash(&quota("2000000000000000000"))
    );
}

#[test]
fn transfer_batch_arrays_are_order_sensitive_and_deterministic() {
    let mut first = quota("1");
    first.get_mut(QUOTA).unwrap()[0]
        .params
        .insert("values".to_string(), serde_json::json!(["100", "200"]));
    let same = first.clone();
    let mut reversed = first.clone();
    reversed.get_mut(QUOTA).unwrap()[0]
        .params
        .insert("values".to_string(), serde_json::json!(["200", "100"]));

    assert_eq!(encode_and_hash(&first), encode_and_hash(&same));
    assert_ne!(encode_and_hash(&first), encode_and_hash(&reversed));
}
