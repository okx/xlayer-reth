//! XLayer gasless e2e tests.
//!
//! These target a running node's RPC and exercise the zero-priced (gasless) transaction path end
//! to end. They require the target nodes to run the XLayer gasless mempool — the `XLayerV1`
//! hardfork active and the gasless whitelist predeploy present at genesis with the rich account as
//! its owner. The tests then own their precondition: they enable gasless and register the transfer
//! target on-chain themselves (see `operations::ensure_gasless_whitelist`), so no devnet bootstrap
//! step is needed to approve the transfer.

use alloy_primitives::U256;
use serde_json::json;
use xlayer_e2e_test::operations;

/// Gasless e2e across the sequencer and a validator/follower node.
///
/// 1. The **sequencer** (`DEFAULT_L2_SEQ_URL`) accepts a zero-priced (`max_fee_per_gas == 0`)
///    transfer and mines it into a block. A `status == 0x1` receipt only exists once the tx is
///    included in a block, so this rules out "accepted into the mempool but never mined".
/// 2. The **validator** (`DEFAULT_L2_NETWORK_URL`) must import that same gasless tx at the *same*
///    block height **and compute the same `stateRoot`/block hash**. The matching `stateRoot` is the
///    core consensus-uniformity check: it proves the validator applied the exact same gasless rules
///    (fee/gas accounting) as the sequencer. A divergent rule would yield a different post-execution
///    state root — the validator would reject the block and never produce this receipt.
/// 3. The sequencer must keep producing blocks afterward (guards the historical gasless
///    payload-builder wedge/panic): a second gasless tx lands in a later block...
/// 4. ...and the validator follows past the gasless blocks to that second tx, again agreeing on the
///    `stateRoot`/hash.
///
/// On a node without gasless enabled the zero-priced tx is rejected as underpriced and this test
/// fails.
#[tokio::test]
async fn test_gasless_zero_price_transfer() {
    let seq_url = operations::manager::DEFAULT_L2_SEQ_URL;
    let validator_url = operations::manager::DEFAULT_L2_NETWORK_URL;
    let amount = U256::from(1u64);
    let to_address = operations::manager::DEFAULT_L2_NEW_ACC1_ADDRESS;
    let seq_client = operations::create_test_client(seq_url);
    let validator_client = operations::create_test_client(validator_url);

    // 1. Sequencer accepts and mines the gasless tx.
    let tx_hash_1 = operations::gasless_zero_price_transfer(seq_url, amount, to_address)
        .await
        .expect("zero-priced tx should be accepted and mined by the sequencer when gasless is on");
    println!("Gasless tx 1: {tx_hash_1}");
    assert!(tx_hash_1.starts_with("0x"));
    let seq_block_1 = wait_for_block_number(seq_url, &tx_hash_1).await;

    // 2. Validator imports that same gasless tx at the same height and agrees on the post-execution
    //    state root (and block hash) — the core consensus-uniformity check.
    let val_block_1 = wait_for_block_number(validator_url, &tx_hash_1).await;
    assert_eq!(
        val_block_1, seq_block_1,
        "validator must import the gasless tx at the same block as the sequencer ({seq_block_1})"
    );
    assert_nodes_agree_on_block(&seq_client, &validator_client, seq_block_1).await;

    // 3. Sequencer keeps producing blocks; a second gasless tx lands in a later block.
    operations::wait_for_blocks(&seq_client, seq_block_1).await;
    let tx_hash_2 = operations::gasless_zero_price_transfer(seq_url, amount, to_address)
        .await
        .expect("a second gasless tx should also be mined by the sequencer");
    println!("Gasless tx 2: {tx_hash_2}");
    let seq_block_2 = wait_for_block_number(seq_url, &tx_hash_2).await;
    assert!(
        seq_block_2 > seq_block_1,
        "second gasless tx must be mined in a later block ({seq_block_1} -> {seq_block_2}); \
         the sequencer must keep producing blocks after a gasless block"
    );

    // 4. Validator keeps following past the gasless blocks to the second tx and again agrees on the
    //    state root / hash.
    let val_block_2 = wait_for_block_number(validator_url, &tx_hash_2).await;
    assert_eq!(
        val_block_2, seq_block_2,
        "validator must keep following gasless blocks (second tx at block {seq_block_2})"
    );
    assert_nodes_agree_on_block(&seq_client, &validator_client, seq_block_2).await;
}

/// Waits until `endpoint_url` reports a successful (`status == 0x1`) receipt for `tx_hash` and
/// returns the block number that receipt is in. The wait is what proves a node imported the block;
/// for a validator this means it validated the (gasless) block the sequencer produced.
async fn wait_for_block_number(endpoint_url: &str, tx_hash: &str) -> u64 {
    let receipt = operations::wait_for_tx_mined(endpoint_url, tx_hash)
        .await
        .expect("tx should be mined with a successful receipt");
    let raw = receipt["blockNumber"].as_str().expect("receipt must include a blockNumber");
    u64::from_str_radix(raw.trim_start_matches("0x"), 16).expect("blockNumber must be valid hex")
}

/// Asserts the sequencer and validator agree on the block at `block_number`: identical `stateRoot`
/// (the post-execution state commitment — the core consensus check for gasless) and identical block
/// hash (which itself commits to the state root).
async fn assert_nodes_agree_on_block(
    seq_client: &operations::HttpClient,
    validator_client: &operations::HttpClient,
    block_number: u64,
) {
    let (seq_root, seq_hash) = block_state_root_and_hash(seq_client, block_number).await;
    let (val_root, val_hash) = block_state_root_and_hash(validator_client, block_number).await;
    assert_eq!(
        seq_root, val_root,
        "sequencer and validator must compute the same stateRoot for gasless block {block_number} \
         (seq {seq_root} vs validator {val_root})"
    );
    assert_eq!(
        seq_hash, val_hash,
        "sequencer and validator must agree on the hash of gasless block {block_number}"
    );
}

/// Fetches `(stateRoot, hash)` of the block at `block_number` from `client`.
async fn block_state_root_and_hash(
    client: &operations::HttpClient,
    block_number: u64,
) -> (String, String) {
    let block = operations::eth_get_block_by_number_or_hash(
        client,
        operations::BlockId::Number(block_number),
        false,
    )
    .await
    .expect("block should be retrievable");
    let state_root =
        block["stateRoot"].as_str().expect("block must include a stateRoot").to_string();
    let hash = block["hash"].as_str().expect("block must include a hash").to_string();
    (state_root, hash)
}

/// Regression: `debug_traceTransaction` on a gasless (zero-priced) tx must succeed against the
/// sequencer. Before the RPC gasless re-execution fix it failed with
/// `-32000: max fee per gas less than block base fee`.
#[tokio::test]
async fn test_gasless_debug_trace_transaction() {
    let seq_url = operations::manager::DEFAULT_L2_SEQ_URL;
    let amount = U256::from(1u64);
    let to_address = operations::manager::DEFAULT_L2_NEW_ACC1_ADDRESS;

    let tx_hash = operations::gasless_zero_price_transfer(seq_url, amount, to_address)
        .await
        .expect("gasless tx should be accepted and mined by the sequencer");
    println!("Gasless tx for trace: {tx_hash}");

    let client = operations::create_test_client(seq_url);
    let trace = operations::debug_trace_transaction(&client, &tx_hash)
        .await
        .expect("debug_traceTransaction must succeed for a gasless tx (no base-fee rejection)");
    println!("debug_traceTransaction result: {trace}");

    assert!(!trace.is_null(), "trace result should not be null");
    // The default struct/opcode tracer returns an object with `gas`/`structLogs`/`failed`.
    assert!(
        trace.get("structLogs").is_some() || trace.get("gas").is_some(),
        "unexpected debug_traceTransaction shape: {trace}"
    );
}

/// A zero-priced (`maxFeePerGas == 0`) call object whose `to`/input match the whitelisted gasless
/// target, so it should be detected as gasless on the RPC re-execution path. `from` is the rich
/// account (funded). The explicit zero fee caps force the base-fee check that the gasless path must
/// relax — without gasless awareness the node rejects it with `max fee per gas less than block base
/// fee`. `input` carries a 4-byte probe so the whitelist's calldata-length guard passes (an empty
/// input is never gasless).
fn gasless_call_object() -> serde_json::Value {
    json!({
        "from": operations::manager::DEFAULT_RICH_ADDRESS,
        "to": operations::manager::DEFAULT_L2_NEW_ACC1_ADDRESS,
        "value": "0x1",
        "input": "0xdeadbeef",  // 4 bytes probe calldata to pass the whitelist's calldata-length guard
        "gas": "0xc350", // 50000
        "maxFeePerGas": "0x0",
        "maxPriorityFeePerGas": "0x0",
    })
}

/// Registers the gasless precondition (gasless enabled + the call target whitelisted).
async fn ensure_gasless_call_whitelisted(seq_url: &str) {
    let target =
        operations::manager::DEFAULT_L2_NEW_ACC1_ADDRESS.parse().expect("valid target address");
    operations::ensure_gasless_whitelist(seq_url, target)
        .await
        .expect("gasless whitelist precondition must be set up");
}

/// Checks whether `eth_call` supports gasless: a zero-priced, whitelisted call must execute instead
/// of being rejected by the base-fee check. The gasless-aware `Call::transact` override on the OP
/// eth API is what makes this succeed; if `eth_call` is not gasless-aware the request errors with
/// `max fee per gas less than block base fee` and this fails.
#[tokio::test]
async fn test_gasless_eth_call() {
    let seq_url = operations::manager::DEFAULT_L2_SEQ_URL;
    ensure_gasless_call_whitelisted(seq_url).await;
    let client = operations::create_test_client(seq_url);

    let result = operations::eth_call(&client, Some(gasless_call_object()), None)
        .await
        .expect("eth_call must succeed for a zero-priced gasless call (no base-fee rejection)");
    println!("eth_call result: {result}");

    // A plain native transfer to an EOA returns empty call data ("0x").
    assert!(result.starts_with("0x"), "unexpected eth_call result: {result}");
}

/// Checks whether `eth_simulateV1` supports gasless. Runs the zero-priced whitelisted call inside a
/// single `blockStateCalls` entry with `validation: true` so the base-fee/fee checks are exercised
/// (the whole point of the gasless relaxation). If the simulate path is gasless-aware the inner
/// call returns `status == 0x1`; otherwise the request is rejected with a base-fee error and this
/// fails — answering whether `eth_simulateV1` honors gasless.
#[tokio::test]
async fn test_gasless_eth_simulate_v1() {
    let seq_url = operations::manager::DEFAULT_L2_SEQ_URL;
    ensure_gasless_call_whitelisted(seq_url).await;
    let client = operations::create_test_client(seq_url);

    let payload = json!({
        "blockStateCalls": [ { "calls": [ gasless_call_object() ] } ],
        "validation": true,
        "traceTransfers": true,
        "returnFullTransactions": false,
    });

    let result = operations::eth_simulate_v1(&client, payload, None).await.expect(
        "eth_simulateV1 must succeed for a zero-priced gasless call (no base-fee rejection)",
    );
    println!("eth_simulateV1 result: {result}");

    let call_result = result
        .get(0)
        .and_then(|block| block.get("calls"))
        .and_then(|calls| calls.get(0))
        .unwrap_or_else(|| panic!("unexpected eth_simulateV1 shape: {result}"));
    assert_eq!(
        call_result.get("status").and_then(|s| s.as_str()),
        Some("0x1"),
        "gasless call must simulate successfully: {call_result}"
    );
}
