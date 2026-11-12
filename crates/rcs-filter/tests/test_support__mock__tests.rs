use super::*;
use crate::test_support::golden;

#[tokio::test]
async fn get_rules_returns_registered_fixture() {
    let mock = MockRcsClient::new();
    mock.set_rules_fixture(&[golden::RULE_SCENARIO_A]);
    let resp = mock.get_rules().await.expect("ok");
    assert_eq!(resp.protocol_version, 1);
    assert_eq!(resp.rules.len(), 1);
    assert_eq!(mock.call_count("get_rules"), 1);
}

#[tokio::test]
async fn unavailable_errors() {
    let mock = MockRcsClient::new();
    mock.set_unavailable(true);
    assert!(mock.get_rules().await.is_err());
}

#[tokio::test]
async fn submit_echoes_and_query_returns_state() {
    let mock = MockRcsClient::new();
    mock.register_query_state_with_reason(
        golden::TX_A,
        "approved",
        Some(1_751_000_002),
        Some("quota reserved"),
    );
    let resp = mock.query(QueryParams::TxHashes(vec![golden::TX_A.to_string()])).await.unwrap();
    assert_eq!(resp.txs.len(), 1);
    assert_eq!(resp.txs[0].status, "approved");
    assert_eq!(resp.txs[0].reason.as_deref(), Some("quota reserved"));
    // Absent hash → silently missing.
    let empty = mock.query(QueryParams::TxHashes(vec![golden::TX_B.to_string()])).await.unwrap();
    assert!(empty.txs.is_empty());
}
