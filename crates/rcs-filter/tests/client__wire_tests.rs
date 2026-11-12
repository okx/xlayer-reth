use super::*;
use serde_json::json;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn spawn_http_script(
    responses: Vec<(u16, String, Duration)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut request_lines = Vec::new();
        for (status, body, body_delay) in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 2048];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request);
            request_lines.push(text.lines().next().unwrap_or_default().to_string());
            let reason = match status {
                200 => "OK",
                201 => "Created",
                202 => "Accepted",
                204 => "No Content",
                500 => "Internal Server Error",
                _ => "Response",
            };
            let headers = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            if !body_delay.is_zero() {
                tokio::time::sleep(body_delay).await;
            }
            let _ = stream.write_all(body.as_bytes()).await;
        }
        request_lines
    });
    (format!("http://{address}"), task)
}

async fn spawn_oversized_response() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = stream.read(&mut request).await.unwrap();
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_RESPONSE_BODY_BYTES + 1
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
    });
    (format!("http://{address}"), task)
}

#[test]
fn required_response_fields_fail_decoding_when_missing() {
    assert!(serde_json::from_value::<RulesResponse>(json!({
        "protocol_version": 1,
        "content_version": 1
    }))
    .is_err());
    assert!(serde_json::from_value::<SubmitResponse>(json!({
        "accepted": []
    }))
    .is_err());
    assert!(serde_json::from_value::<SubmitResponse>(json!({
        "rejected_malformed": []
    }))
    .is_err());
    assert!(serde_json::from_value::<QueryResponse>(json!({})).is_err());
}

#[path = "client__wire_tests__response_root_tests.rs"]
#[cfg(test)]
mod response_root_tests;

#[tokio::test]
async fn endpoint_specific_statuses_are_enforced() {
    let submit_body = r#"{"accepted":[],"rejected_malformed":[]}"#;
    for status in [200, 201, 204, 500] {
        let (url, server) =
            spawn_http_script(vec![(status, submit_body.to_string(), Duration::ZERO)]).await;
        let client = ReqwestRcsClient::new(url).unwrap();
        let result = client.submit(SubmitRequest { xlayer_block_height: 1, txs: Vec::new() }).await;
        assert!(matches!(result, Err(FilterError::UnexpectedStatus(code)) if code == status));
        server.await.unwrap();
    }

    let (url, server) =
        spawn_http_script(vec![(202, submit_body.to_string(), Duration::ZERO)]).await;
    ReqwestRcsClient::new(url)
        .unwrap()
        .submit(SubmitRequest { xlayer_block_height: 1, txs: Vec::new() })
        .await
        .unwrap();
    server.await.unwrap();

    for status in [202, 204, 500] {
        let (url, server) =
            spawn_http_script(vec![(status, r#"{"txs":[]}"#.to_string(), Duration::ZERO)]).await;
        let result =
            ReqwestRcsClient::new(url).unwrap().query(QueryParams::Status("pending".into())).await;
        assert!(matches!(result, Err(FilterError::UnexpectedStatus(code)) if code == status));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn get_and_query_decode_only_valid_200_json() {
    let (url, server) = spawn_http_script(vec![
        (
            200,
            r#"{"protocol_version":1,"content_version":1,"rules":[]}"#.to_string(),
            Duration::ZERO,
        ),
        (200, r#"{"txs":[]}"#.to_string(), Duration::ZERO),
    ])
    .await;
    let client = ReqwestRcsClient::new(url).unwrap();
    assert!(client.get_rules().await.is_ok());
    assert!(client.query(QueryParams::Status("approved".into())).await.is_ok());
    server.await.unwrap();

    let (url, server) = spawn_http_script(vec![(200, "not-json".into(), Duration::ZERO)]).await;
    assert!(matches!(
        ReqwestRcsClient::new(url).unwrap().get_rules().await,
        Err(FilterError::Decode(_))
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn status_query_encoding_never_uses_tx_hashes() {
    let responses = (0..4).map(|_| (200, r#"{"txs":[]}"#.to_string(), Duration::ZERO)).collect();
    let (url, server) = spawn_http_script(responses).await;
    let client = ReqwestRcsClient::new(url).unwrap();
    for status in ["pending", "approved", "denied", "outdated"] {
        client.query(QueryParams::Status(status.into())).await.unwrap();
    }
    let requests = server.await.unwrap();
    for (request, status) in requests.iter().zip(["pending", "approved", "denied", "outdated"]) {
        assert!(request.contains(&format!("?status={status}")), "{request}");
        assert!(!request.contains("tx_hashes"), "{request}");
    }
}

#[tokio::test]
async fn tx_hashes_query_encodes_only_tx_hashes() {
    let (url, server) =
        spawn_http_script(vec![(200, r#"{"txs":[]}"#.to_string(), Duration::ZERO)]).await;
    let client = ReqwestRcsClient::new(url).unwrap();
    client.query(QueryParams::TxHashes(vec!["0xaaaa".into(), "0xbbbb".into()])).await.unwrap();

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("?tx_hashes=0xaaaa%2C0xbbbb"), "{}", requests[0]);
    assert!(!requests[0].contains("status="), "{}", requests[0]);
}

#[tokio::test]
async fn rules_endpoints_reject_non_200() {
    for path in ["rules", "version"] {
        let (url, server) = spawn_http_script(vec![(
            202,
            r#"{"protocol_version":1,"content_version":1,"rules":[]}"#.to_string(),
            Duration::ZERO,
        )])
        .await;
        let client = ReqwestRcsClient::new(url).unwrap();
        let result = if path == "rules" {
            client.get_rules().await.map(|_| ())
        } else {
            client.get_rules_version().await.map(|_| ())
        };
        assert!(matches!(result, Err(FilterError::UnexpectedStatus(202))));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn response_body_stall_hits_total_request_timeout() {
    let (url, server) =
        spawn_http_script(vec![(200, r#"{"txs":[]}"#.to_string(), Duration::from_secs(1))]).await;
    let client =
        ReqwestRcsClient::with_timeouts(url, Duration::from_millis(50), Duration::from_millis(100))
            .unwrap();
    assert!(matches!(
        client.query(QueryParams::Status("pending".into())).await,
        Err(FilterError::Timeout(_))
    ));
    server.abort();
}

#[tokio::test]
async fn oversized_response_is_rejected_before_body_allocation() {
    let (url, server) = spawn_oversized_response().await;
    let result = ReqwestRcsClient::new(url).unwrap().get_rules().await;
    assert!(matches!(result, Err(FilterError::Decode(message)) if message.contains("exceeds")));
    server.await.unwrap();
}

#[tokio::test]
async fn rules_version_and_required_fields_are_enforced_over_http() {
    let (url, server) = spawn_http_script(vec![(
        200,
        r#"{"protocol_version":1,"content_version":7}"#.to_string(),
        Duration::ZERO,
    )])
    .await;
    let version = ReqwestRcsClient::new(url).unwrap().get_rules_version().await.unwrap();
    assert_eq!(version.protocol_version, 1);
    assert_eq!(version.content_version, 7);
    server.await.unwrap();

    for body in [r#"{"protocol_version":1}"#, r#"{"content_version":7}"#] {
        let (url, server) = spawn_http_script(vec![(200, body.to_string(), Duration::ZERO)]).await;
        assert!(matches!(
            ReqwestRcsClient::new(url).unwrap().get_rules_version().await,
            Err(FilterError::Decode(_))
        ));
        server.await.unwrap();
    }

    let (url, server) =
        spawn_http_script(vec![(202, r#"{"accepted":[]}"#.to_string(), Duration::ZERO)]).await;
    let result = ReqwestRcsClient::new(url)
        .unwrap()
        .submit(SubmitRequest { xlayer_block_height: 1, txs: Vec::new() })
        .await;
    assert!(matches!(result, Err(FilterError::Decode(_))));
    server.await.unwrap();

    let (url, server) = spawn_http_script(vec![(200, r#"{}"#.to_string(), Duration::ZERO)]).await;
    assert!(matches!(
        ReqwestRcsClient::new(url).unwrap().query(QueryParams::Status("pending".into())).await,
        Err(FilterError::Decode(_))
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn client_recovers_after_stalled_response_without_recreation() {
    let (url, server) = spawn_http_script(vec![
        (200, r#"{"txs":[]}"#.to_string(), Duration::from_millis(150)),
        (200, r#"{"txs":[]}"#.to_string(), Duration::ZERO),
    ])
    .await;
    let client =
        ReqwestRcsClient::with_timeouts(url, Duration::from_millis(25), Duration::from_millis(75))
            .unwrap();
    assert!(matches!(
        client.query(QueryParams::Status("pending".into())).await,
        Err(FilterError::Timeout(_))
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(client.query(QueryParams::Status("pending".into())).await.is_ok());
    server.await.unwrap();
}
