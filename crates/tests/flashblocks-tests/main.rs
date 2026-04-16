//! Functional tests for flashblocks e2e tests
//!
//! Run flashblocks tests without benchmark and comparison tests: `cargo test -p xlayer-e2e-test --test flashblocks_tests -- --nocapture --test-threads=1`
//! Run all tests (including ignored): `cargo test -p xlayer-e2e-test --test flashblocks_tests -- --include-ignored --nocapture --test-threads=1`
//! Run a specific test: `cargo test -p xlayer-e2e-test --test flashblocks_tests -- <test_case_name> --include-ignored --nocapture --test-threads=1`

use eyre::Result;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    str::FromStr,
    time::{Duration, Instant},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use alloy_primitives::{hex, Address, U256};
use alloy_sol_types::{sol, SolCall};

use xlayer_e2e_test::operations;

const ITERATIONS: usize = 11;
const TX_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(10);
const WEB_SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

// ========================================================================
// Flashblocks enabled/disabled tests
// ========================================================================

/// Verifies that `eth_flashblocksEnabled` returns `true` on a flashblocks-enabled node
/// whose state cache has been initialized (confirm height > 0).
#[tokio::test]
async fn fb_flashblocks_enabled_returns_true_test() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);

    let enabled = operations::eth_flashblocks_enabled(&fb_client)
        .await
        .expect("eth_flashblocksEnabled RPC call failed");

    assert!(enabled, "eth_flashblocksEnabled should return true on a flashblocks-enabled node");
}

/// Verifies that `eth_flashblocksEnabled` returns `false` on a node without flashblocks.
#[tokio::test]
#[ignore = "requires non-flashblocks node"]
async fn fb_flashblocks_enabled_returns_false_on_non_fb_node_test() {
    let non_fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_NO_FB);

    let enabled = operations::eth_flashblocks_enabled(&non_fb_client)
        .await
        .expect("eth_flashblocksEnabled RPC call failed");

    assert!(!enabled, "eth_flashblocksEnabled should return false on a non-flashblocks node");
}

// ========================================================================
// Flashblocks P2P peer status tests
// ========================================================================

/// Verifies that `eth_flashblocksPeerStatus` returns valid responses on both
/// sequencers and that each sees the other as a static peer.
#[tokio::test]
#[ignore = "requires conductor-enabled devnet with P2P keys"]
async fn fb_peer_status_returns_data_on_sequencer_test() {
    // seq1 → expects localPeerId = SEQ1, static peer = SEQ2
    let seq1_status = operations::eth_flashblocks_peer_status(&operations::create_test_client(
        operations::DEFAULT_L2_SEQ_URL,
    ))
    .await
    .expect("RPC failed")
    .expect("seq1 should return peer status");

    assert_eq!(
        seq1_status["localPeerId"].as_str().unwrap(),
        operations::DEFAULT_SEQ_FB_P2P_PEER_ID
    );
    let summary = &seq1_status["summary"];
    let total = summary["total"].as_u64().unwrap();
    assert_eq!(
        total,
        summary["connected"].as_u64().unwrap()
            + summary["disconnected"].as_u64().unwrap()
            + summary["neverConnected"].as_u64().unwrap(),
        "summary counts must add up"
    );
    let seq2_on_seq1 = seq1_status["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["peerId"].as_str() == Some(operations::DEFAULT_SEQ2_FB_P2P_PEER_ID))
        .expect("seq2 must appear in seq1's peer list");
    assert!(seq2_on_seq1["isStatic"].as_bool().unwrap());

    // seq2 → expects localPeerId = SEQ2, static peer = SEQ1
    let seq2_status = operations::eth_flashblocks_peer_status(&operations::create_test_client(
        operations::DEFAULT_L2_SEQ2_URL,
    ))
    .await
    .expect("RPC failed")
    .expect("seq2 should return peer status");

    assert_eq!(
        seq2_status["localPeerId"].as_str().unwrap(),
        operations::DEFAULT_SEQ2_FB_P2P_PEER_ID
    );
    let seq1_on_seq2 = seq2_status["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["peerId"].as_str() == Some(operations::DEFAULT_SEQ_FB_P2P_PEER_ID))
        .expect("seq1 must appear in seq2's peer list");
    assert!(seq1_on_seq2["isStatic"].as_bool().unwrap());
}

/// Verifies that `eth_flashblocksPeerStatus` returns `null` on an RPC node
/// that does not have the P2P broadcast layer.
#[tokio::test]
async fn fb_peer_status_returns_null_on_rpc_node_test() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);

    let result = operations::eth_flashblocks_peer_status(&fb_client)
        .await
        .expect("eth_flashblocksPeerStatus RPC call failed");

    assert!(result.is_none(), "RPC node (non-sequencer) should return null for peer status");
}

/// Verifies that `eth_flashblocksPeerStatus` returns `null` on a node without
/// flashblocks.
#[tokio::test]
#[ignore = "requires non-flashblocks node"]
async fn fb_peer_status_returns_null_on_non_fb_node_test() {
    let non_fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_NO_FB);

    let result = operations::eth_flashblocks_peer_status(&non_fb_client)
        .await
        .expect("eth_flashblocksPeerStatus RPC call failed");

    assert!(result.is_none(), "non-flashblocks node should return null for peer status");
}

// ========================================================================
// Flashblocks pending state tests
// ========================================================================

/// Verifies that the flashblocks node serves pending block data AHEAD of the canonical chain.
/// This is the core low-latency property: sub-block-time visibility.
#[tokio::test]
async fn fb_low_latency_pending_visibility_test() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);

    let canonical_height = operations::eth_block_number(&fb_client)
        .await
        .expect("Failed to get canonical block number");

    let pending_block = operations::eth_get_block_by_number_or_hash(
        &fb_client,
        operations::BlockId::Pending,
        false,
    )
    .await
    .expect("Pending eth_getBlockByNumber failed");
    assert!(!pending_block.is_null(), "Pending block should not be null");

    let pending_number = pending_block["number"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("Pending block number should be a valid hex string");

    assert!(
        pending_number >= canonical_height,
        "Flashblocks pending block ({pending_number}) should be ahead or equal to canonical height ({canonical_height})"
    );
}

/// Block-level RPC queries using the pending tag on the flashblocks node.
#[tokio::test]
async fn fb_pending_block_queries_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    // eth_getBlockByNumber(Pending) returns a block
    let fb_block = operations::eth_get_block_by_number_or_hash(
        &fb_client,
        operations::BlockId::Pending,
        false,
    )
    .await
    .expect("Pending eth_getBlockByNumber failed");
    assert!(!fb_block.is_null(), "Pending block should not be null");

    // Pending block is also queryable by its actual block number
    let fb_block_number = fb_block["number"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("Block number should be a valid hex string");
    let fb_block_by_number = operations::eth_get_block_by_number_or_hash(
        &fb_client,
        operations::BlockId::Number(fb_block_number),
        false,
    )
    .await
    .expect("eth_getBlockByNumber by actual number failed");
    assert!(!fb_block_by_number.is_null(), "Block by number should not be null");

    // Send a tx so pending block has at least 1 transaction
    operations::native_balance_transfer(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        test_address,
        None,
        true,
    )
    .await
    .expect("Failed to send tx");

    // eth_getBlockTransactionCountByNumber(Pending) should be >= 1 after sending a tx
    let tx_count = operations::eth_get_block_transaction_count_by_number_or_hash(
        &fb_client,
        operations::BlockId::Pending,
    )
    .await
    .expect("Pending eth_getBlockTransactionCountByNumber failed");
    assert!(
        tx_count >= 1,
        "Pending block tx count should be >= 1 after sending tx, got {tx_count}"
    );

    // eth_getBlockReceipts(Pending)
    let receipts = operations::eth_get_block_receipts(&fb_client, operations::BlockId::Pending)
        .await
        .expect("Pending eth_getBlockReceipts failed");
    assert!(!receipts.is_null(), "Pending block receipts should not be null");

    // eth_getRawTransactionByBlockNumberAndIndex using pending block number
    let pending = operations::eth_get_block_by_number_or_hash(
        &fb_client,
        operations::BlockId::Pending,
        false,
    )
    .await
    .expect("Pending eth_getBlockByNumber failed");
    let raw_tx = operations::eth_get_raw_transaction_by_block_number_and_index(
        &fb_client,
        pending["number"].as_str().expect("Block number should not be empty"),
        "0x0",
    )
    .await
    .expect("eth_getRawTransactionByBlockNumberAndIndex failed");
    assert!(!raw_tx.is_null(), "Raw transaction at index 0 should not be null");
}

/// Transaction-level RPC queries on the flashblocks node.
#[tokio::test]
async fn fb_pending_tx_queries_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    let tx_hash = operations::native_balance_transfer(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        test_address,
        None,
        true,
    )
    .await
    .expect("Failed to send tx");

    // eth_getTransactionByHash
    let tx = operations::eth_get_transaction_by_hash(&fb_client, &tx_hash)
        .await
        .expect("eth_getTransactionByHash failed");
    assert!(!tx.is_null(), "Transaction should not be null");

    // eth_getTransactionReceipt
    let receipt = operations::eth_get_transaction_receipt(&fb_client, &tx_hash)
        .await
        .expect("eth_getTransactionReceipt failed");
    assert!(!receipt.is_null(), "Receipt should not be null");
    assert_eq!(
        receipt["transactionIndex"], tx["transactionIndex"],
        "Transaction index mismatch between tx and receipt"
    );

    // eth_getRawTransactionByHash
    let raw_tx = operations::eth_get_raw_transaction_by_hash(&fb_client, &tx_hash)
        .await
        .expect("eth_getRawTransactionByHash failed");
    assert!(!raw_tx.is_null(), "Raw transaction should not be null");
}

/// State-reading RPC queries using the pending tag on the flashblocks node.
#[tokio::test]
async fn fb_pending_state_queries_test() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let sender_address = operations::DEFAULT_RICH_ADDRESS;
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");

    // eth_getBalance(Pending)
    let balance =
        operations::get_balance(&fb_client, sender_address, Some(operations::BlockId::Pending))
            .await
            .expect("Pending eth_getBalance failed");
    assert_ne!(balance, U256::ZERO, "Balance should not be zero");

    // eth_getTransactionCount(Pending)
    let nonce = operations::eth_get_transaction_count(
        &fb_client,
        sender_address,
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Pending eth_getTransactionCount failed");
    assert_ne!(nonce, 0, "Transaction count should not be zero");

    // eth_getCode(Pending)
    let code = operations::eth_get_code(
        &fb_client,
        contracts.erc20.to_string().as_str(),
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Pending eth_getCode failed");
    assert_ne!(code, "0x", "Code should not be empty");

    // eth_getStorageAt(Pending)
    let storage = operations::eth_get_storage_at(
        &fb_client,
        contracts.erc20.to_string().as_str(),
        "0x2",
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Pending eth_getStorageAt failed");
    assert_ne!(storage, "0x", "Storage should not be empty");

    // eth_call(Pending)
    sol! {
        function balanceOf(address account) external view returns (uint256);
    }
    let call = balanceOfCall { account: Address::from_str(test_address).expect("Invalid address") };
    let calldata = call.abi_encode();
    let call_args = json!({
        "from": test_address,
        "to": contracts.erc20,
        "gas": "0x100000",
        "data": format!("0x{}", hex::encode(&calldata)),
    });
    let result =
        operations::eth_call(&fb_client, Some(call_args), Some(operations::BlockId::Pending))
            .await
            .expect("Pending eth_call failed");
    assert_ne!(result, "0x", "eth_call result should not be empty");

    // eth_estimateGas(Pending)
    let transfer_args = json!({
        "from": sender_address,
        "to": test_address,
        "value": format!("0x{:x}", operations::GWEI).as_str(),
    });
    let gas = operations::estimate_gas(
        &fb_client,
        Some(transfer_args),
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Pending eth_estimateGas failed");
    assert_eq!(gas, 21_000, "Estimate gas for native transfer should be 21_000");
}

/// Verifies that deploying a contract via CREATE2 through the flashblocks node
/// results in non-empty bytecode at the deployed address when queried with the pending tag.
#[tokio::test]
async fn fb_pending_create2_deploy_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");

    // Use current nonce as salt to avoid collision across repeated test runs
    let nonce = operations::eth_get_transaction_count(
        &fb_client,
        operations::DEFAULT_RICH_ADDRESS,
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Failed to get nonce");
    let salt = U256::from(nonce);

    // Deploy a child contract via CREATE2
    let tx_hash = operations::deploy_via_create2(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        contracts.create2_factory,
        salt,
    )
    .await
    .expect("CREATE2 deploy failed");

    operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &tx_hash)
        .await
        .expect("CREATE2 deploy tx not mined");

    // Compute the expected address
    let expected_addr = operations::compute_create2_address(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        contracts.create2_factory,
        salt,
    )
    .await
    .expect("computeAddress failed");

    // Verify code exists at the deployed address via pending tag
    let code = operations::eth_get_code(
        &fb_client,
        &format!("{expected_addr:#x}"),
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("eth_getCode(pending) failed");
    assert!(code.len() > 2, "Deployed contract should have non-empty bytecode, got: {code}");
}

/// Verifies that the CREATE2 `computeAddress` result matches the actual deployed address.
#[tokio::test]
async fn fb_pending_create2_address_determinism_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");

    // Use current nonce as salt to avoid collision across repeated test runs
    let nonce = operations::eth_get_transaction_count(
        &fb_client,
        operations::DEFAULT_RICH_ADDRESS,
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Failed to get nonce");
    let salt = U256::from(nonce);

    // Compute expected address BEFORE deployment
    let predicted_addr = operations::compute_create2_address(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        contracts.create2_factory,
        salt,
    )
    .await
    .expect("computeAddress failed");

    // Deploy
    let tx_hash = operations::deploy_via_create2(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        contracts.create2_factory,
        salt,
    )
    .await
    .expect("CREATE2 deploy failed");

    let receipt = operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &tx_hash)
        .await
        .expect("CREATE2 deploy tx not mined");

    // Extract the deployed address from the Deployed(address,uint256) event log
    let logs = receipt["logs"].as_array().expect("Receipt should have logs");
    assert!(!logs.is_empty(), "Deploy tx should emit at least one log");

    // Verify the event signature: keccak256("Deployed(address,uint256)")
    let deployed_topic = "0xb03c53b28e78a88e31607a27e1fa48234dce28d5d9d9ec7b295aeb02e674a1e1";
    let deployed_log = logs
        .iter()
        .find(|log| {
            log["topics"]
                .as_array()
                .and_then(|t| t.first())
                .and_then(|t| t.as_str())
                .map(|t| t == deployed_topic)
                .unwrap_or(false)
        })
        .expect("Should find a Deployed event log");

    // The Deployed event data contains the address in the first 32-byte word
    let event_data = deployed_log["data"].as_str().expect("Log should have data");
    let data_bytes =
        hex::decode(event_data.trim_start_matches("0x")).expect("Invalid hex in log data");
    assert!(data_bytes.len() >= 32, "Event data should be at least 32 bytes");
    let actual_addr = Address::from_slice(&data_bytes[12..32]);

    assert_eq!(
        predicted_addr, actual_addr,
        "CREATE2 predicted address should match actual deployed address"
    );
}

/// Post-Cancun: verifies that contract code persists after SELFDESTRUCT in a
/// separate transaction (EIP-6780 behavior).
#[tokio::test]
async fn fb_pending_selfdestruct_code_persists_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");

    // Use current nonce as salt to avoid collision across repeated test runs
    let nonce = operations::eth_get_transaction_count(
        &fb_client,
        operations::DEFAULT_RICH_ADDRESS,
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Failed to get nonce");
    let salt = U256::from(nonce);
    let recipient = Address::from_str(operations::DEFAULT_L2_NEW_ACC1_ADDRESS).unwrap();

    // Deploy a child contract via CREATE2
    let deploy_tx = operations::deploy_via_create2(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        contracts.create2_factory,
        salt,
    )
    .await
    .expect("CREATE2 deploy failed");
    operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &deploy_tx)
        .await
        .expect("Deploy tx not mined");

    let child_addr = operations::compute_create2_address(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        contracts.create2_factory,
        salt,
    )
    .await
    .expect("computeAddress failed");

    // Verify code exists before destroy
    let code_before = operations::eth_get_code(
        &fb_client,
        &format!("{child_addr:#x}"),
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("eth_getCode(pending) before destroy failed");
    assert!(code_before.len() > 2, "Contract should have bytecode before destroy");

    // Destroy the child contract (SELFDESTRUCT in separate tx)
    let destroy_tx = operations::destroy_via_factory(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        contracts.create2_factory,
        child_addr,
        recipient,
    )
    .await
    .expect("Destroy failed");
    operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &destroy_tx)
        .await
        .expect("Destroy tx not mined");

    // Post-Cancun: code should PERSIST after SELFDESTRUCT in a separate tx
    let code_after = operations::eth_get_code(
        &fb_client,
        &format!("{child_addr:#x}"),
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("eth_getCode(pending) after destroy failed");
    assert_eq!(
        code_before, code_after,
        "Post-Cancun: code should persist unchanged after SELFDESTRUCT in separate tx"
    );
}

/// Verifies that calling a precompile (SHA256 at 0x02) through a contract works
/// on the flashblocks node.
#[tokio::test]
async fn fb_pending_precompile_call_test() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");

    // Call SHA256 precompile through the contract via eth_call (read-only, deterministic)
    sol! {
        function callSha256(bytes memory input) external view returns (bytes32 result);
    }
    let input_data = b"hello world";
    let call = callSha256Call { input: input_data.to_vec().into() };
    let calldata = call.abi_encode();

    let call_args = json!({
        "to": contracts.precompile_caller,
        "data": format!("0x{}", hex::encode(&calldata)),
    });
    let result =
        operations::eth_call(&fb_client, Some(call_args), Some(operations::BlockId::Pending))
            .await
            .expect("eth_call to PrecompileCaller failed");

    // SHA256("hello world") is a known constant
    let expected_sha256 = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
    let result_hex = result.trim_start_matches("0x");
    assert_eq!(result_hex, expected_sha256, "SHA256 precompile result should match expected hash");
}

/// Verifies that sending native ETH to a precompile address (0x02) updates the
/// balance correctly through the flashblocks pending state.
#[tokio::test]
async fn fb_pending_transfer_to_precompile_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let precompile_addr = "0x0000000000000000000000000000000000000002";

    // Record balance before
    let balance_before =
        operations::get_balance(&fb_client, precompile_addr, Some(operations::BlockId::Pending))
            .await
            .expect("get precompile balance before failed");

    // Send 1 GWEI to the precompile address.
    // Use a higher gas limit (30_000) because transfers to precompile addresses
    // trigger precompile execution which requires gas beyond the 21_000 intrinsic cost.
    let tx_hash = operations::native_balance_transfer(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        precompile_addr,
        Some(30_000),
        true,
    )
    .await
    .expect("Transfer to precompile failed");

    // Verify tx was successful
    let receipt = operations::eth_get_transaction_receipt(&fb_client, &tx_hash)
        .await
        .expect("Receipt not found");
    assert_eq!(receipt["status"].as_str().unwrap(), "0x1", "Transfer to precompile should succeed");

    // Check balance increased
    let balance_after =
        operations::get_balance(&fb_client, precompile_addr, Some(operations::BlockId::Pending))
            .await
            .expect("get precompile balance after failed");

    assert!(
        balance_after >= balance_before + U256::from(operations::GWEI),
        "Precompile balance should increase by at least 1 GWEI, before={balance_before} after={balance_after}"
    );
}

/// Verifies that `eth_getLogs` returns Transfer event logs from an ERC20 token
/// transfer when queried with the transaction's block range.
#[tokio::test]
async fn fb_pending_get_logs_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    // Send an ERC20 transfer
    let tx_hash = operations::erc20_balance_transfer(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        None,
        test_address,
        contracts.erc20,
        None,
    )
    .await
    .expect("ERC20 transfer failed");

    let receipt = operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &tx_hash)
        .await
        .expect("ERC20 transfer tx not mined");

    // Use the exact block from the receipt for a tight query range
    let tx_block = receipt["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("Failed to parse block number from receipt");

    // Wait for canonical chain to reach the tx block so eth_getLogs can serve it
    operations::wait_for_canonical_block(operations::DEFAULT_L2_NETWORK_URL_FB, tx_block)
        .await
        .expect("Canonical chain did not reach tx block");

    // Query logs from the ERC20 contract address
    // Transfer event topic: keccak256("Transfer(address,address,uint256)")
    let transfer_topic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
    let logs = operations::eth_get_logs(
        &fb_client,
        Some(operations::BlockId::Number(tx_block)),
        Some(operations::BlockId::Number(tx_block)),
        Some(&format!("{:#x}", contracts.erc20)),
        Some(vec![transfer_topic.to_string()]),
    )
    .await
    .expect("eth_getLogs failed");

    let logs_arr = logs.as_array().expect("Logs should be an array");
    assert!(!logs_arr.is_empty(), "Should have at least one Transfer log");

    // Find the log from our specific transaction
    let our_log = logs_arr
        .iter()
        .find(|log| log["transactionHash"].as_str() == Some(tx_hash.as_str()))
        .expect("Should find a Transfer log from our transaction");

    assert_eq!(
        our_log["address"].as_str().unwrap().to_lowercase(),
        format!("{:#x}", contracts.erc20).to_lowercase(),
        "Log address should match ERC20 contract"
    );
}

/// Verifies that a contract-to-contract call (ContractA calling ContractB via
/// `triggerCall`) works correctly through the flashblocks node.
#[tokio::test]
async fn fb_pending_contract_to_contract_call_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");

    // Read call count before
    sol! {
        function getCallCount() external view returns (uint256);
        function triggerCall() external;
    }
    let count_call = getCallCountCall {};
    let count_calldata = count_call.abi_encode();
    let call_args = json!({
        "from": operations::DEFAULT_RICH_ADDRESS,
        "to": contracts.contract_a,
        "data": format!("0x{}", hex::encode(&count_calldata)),
    });
    let count_before_hex =
        operations::eth_call(&fb_client, Some(call_args), Some(operations::BlockId::Pending))
            .await
            .expect("eth_call getCallCount before failed");

    // Send a triggerCall() tx — this calls ContractB internally
    let trigger = triggerCallCall {};
    let trigger_calldata = trigger.abi_encode();
    let tx_request = alloy_rpc_types_eth::TransactionRequest::default().gas_limit(200_000);
    let result = operations::make_contract_call(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        operations::DEFAULT_RICH_PRIVATE_KEY,
        contracts.contract_a,
        alloy_primitives::Bytes::from(trigger_calldata),
        U256::ZERO,
        tx_request,
    )
    .await
    .expect("triggerCall tx failed");

    let receipt = &result["receipt"];
    assert_eq!(receipt["status"].as_str().unwrap(), "0x1", "triggerCall should succeed");

    // Read call count after — should have incremented
    let call_args_after = json!({
        "from": operations::DEFAULT_RICH_ADDRESS,
        "to": contracts.contract_a,
        "data": format!("0x{}", hex::encode(count_call.abi_encode())),
    });
    let count_after_hex =
        operations::eth_call(&fb_client, Some(call_args_after), Some(operations::BlockId::Pending))
            .await
            .expect("eth_call getCallCount after failed");

    assert_ne!(count_before_hex, count_after_hex, "Call count should change after triggerCall()");
}

/// Verifies that state transitions are visible across sequential blocks through
/// the flashblocks node: balance at block N differs from balance at block N+1
/// after a transfer.
#[tokio::test]
async fn fb_pending_state_transition_across_blocks_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    // Get balance at the current latest block
    let block_n =
        operations::eth_block_number(&fb_client).await.expect("Failed to get block number");
    let balance_at_n = operations::get_balance(
        &fb_client,
        test_address,
        Some(operations::BlockId::Number(block_n)),
    )
    .await
    .expect("Failed to get balance at block N");

    // Send a transfer
    let tx_hash = operations::native_balance_transfer(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        test_address,
        None,
        true,
    )
    .await
    .expect("Transfer failed");

    // Get the block the tx was mined in
    let receipt = operations::eth_get_transaction_receipt(&fb_client, &tx_hash)
        .await
        .expect("Receipt not found");
    let block_n_plus = receipt["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("Failed to parse block number from receipt");

    // Balance at the new block should differ from block N
    let balance_at_new = operations::get_balance(
        &fb_client,
        test_address,
        Some(operations::BlockId::Number(block_n_plus)),
    )
    .await
    .expect("Failed to get balance at new block");

    assert!(
        balance_at_new > balance_at_n,
        "Balance at block {block_n_plus} ({balance_at_new}) should be greater than at block {block_n} ({balance_at_n})"
    );

    // Verify the old block still returns the old balance (immutable history)
    let balance_at_n_again = operations::get_balance(
        &fb_client,
        test_address,
        Some(operations::BlockId::Number(block_n)),
    )
    .await
    .expect("Failed to re-read balance at block N");

    assert_eq!(
        balance_at_n, balance_at_n_again,
        "Historical balance at block {block_n} should be immutable"
    );
}

// ========================================================================
// Flashblocks cache correctioness tests
// ========================================================================

/// Cache correctness test: snapshots all confirmed flashblock cache entries currently ahead
/// of the canonical chain, writes them to a file, waits for canonical to catch up, then
/// compares against the non-flashblock canonical node to verify the cache was correct.
#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_cache_correctness_test() {
    const SNAPSHOT_FILE: &str = "/tmp/fb_cache_snapshot.json";
    const CATCHUP_TIMEOUT: Duration = Duration::from_secs(120);
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let canonical_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_NO_FB);

    let canonical_height = operations::eth_block_number(&canonical_client)
        .await
        .expect("Failed to get canonical block number");
    println!("Canonical height: {canonical_height}");

    println!(
        "Waiting for at least two flashblocks ahead of canonical (need confirmed + pending)..."
    );
    let pending_number = tokio::time::timeout(CATCHUP_TIMEOUT, async {
        loop {
            let pending = operations::eth_get_block_by_number_or_hash(
                &fb_client,
                operations::BlockId::Pending,
                false,
            )
            .await
            .unwrap_or(Value::Null);

            if let Some(n) = pending["number"]
                .as_str()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                && n > canonical_height + 1
            {
                println!("Flashblock pending height: {n}");
                return n;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("Timed out waiting for confirmed flashblocks ahead of canonical");

    let confirmed_block = pending_number - 1;
    let mut snapshot = Vec::new();
    for height in (canonical_height + 1)..=confirmed_block {
        let block = operations::eth_get_block_by_number_or_hash(
            &fb_client,
            operations::BlockId::Number(height),
            true,
        )
        .await
        .unwrap_or(Value::Null);

        if block.is_null() {
            println!("Cache has no block at height {height}, stopping discovery at {}", height - 1);
            break;
        }

        let receipts =
            operations::eth_get_block_receipts(&fb_client, operations::BlockId::Number(height))
                .await
                .unwrap_or(Value::Null);

        println!(
            "Snapshotted height {height}: hash={} stateRoot={}",
            block["hash"].as_str().unwrap_or("?"),
            block["stateRoot"].as_str().unwrap_or("?"),
        );
        snapshot.push(json!({ "height": height, "block": block, "receipts": receipts }));
    }

    assert!(
        !snapshot.is_empty(),
        "No flashblock cache entries found ahead of canonical height {canonical_height}"
    );
    println!("Snapshotted {} block(s) from flashblock cache", snapshot.len());
    let snapshot_target = snapshot.last().unwrap()["height"].as_u64().unwrap();

    let snapshot_json =
        serde_json::to_string_pretty(&json!(snapshot)).expect("Failed to serialize snapshot");
    fs::write(SNAPSHOT_FILE, &snapshot_json).expect("Failed to write snapshot file");
    println!("Snapshot written to {SNAPSHOT_FILE} ({} bytes)", snapshot_json.len());

    println!("Waiting for canonical node to reach height {snapshot_target}...");
    tokio::time::timeout(CATCHUP_TIMEOUT, async {
        loop {
            let h = operations::eth_block_number(&canonical_client).await.unwrap_or(0);
            println!("Canonical height: {h} / {snapshot_target}");
            if h >= snapshot_target {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("Timed out waiting for canonical node to catch up");

    let saved: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(SNAPSHOT_FILE).expect("Failed to read snapshot"))
            .expect("Failed to parse snapshot");

    let mut mismatches = 0;
    for entry in &saved {
        let height = entry["height"].as_u64().expect("Missing height in snapshot");
        let fb_block = &entry["block"];
        let fb_receipts = &entry["receipts"];

        let canonical_block = operations::eth_get_block_by_number_or_hash(
            &canonical_client,
            operations::BlockId::Number(height),
            true,
        )
        .await
        .unwrap_or_else(|_| panic!("Failed to get canonical block at height {height}"));
        assert!(!canonical_block.is_null(), "Canonical block at height {height} is null");

        let canonical_receipts = operations::eth_get_block_receipts(
            &canonical_client,
            operations::BlockId::Number(height),
        )
        .await
        .unwrap_or_else(|_| panic!("Failed to get canonical receipts at height {height}"));

        for field in ["hash", "stateRoot", "transactionsRoot", "receiptsRoot", "gasUsed"] {
            if fb_block[field] != canonical_block[field] {
                eprintln!(
                    "MISMATCH height={height} field='{field}': flashblock={} canonical={}",
                    fb_block[field], canonical_block[field]
                );
                mismatches += 1;
            }
        }
        if fb_receipts != &canonical_receipts {
            eprintln!("MISMATCH height={height}: receipts differ");
            mismatches += 1;
        }
        if mismatches == 0 {
            println!("✓ height {height}: all fields match canonical");
        }
    }

    let _ = fs::remove_file(SNAPSHOT_FILE);
    assert_eq!(mismatches, 0, "{mismatches} cache mismatch(es) — flashblock cache was incorrect");
}

// ========================================================================
// Flashblocks benchmark tests
// ========================================================================

/// Flashblock native balance transfer tx confirmation benchmark between a flashblock
/// node and a non-flashblock node.
#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_benchmark_native_tx_confirmation() {
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    // Benchmark transfer tx to test address
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let mut total_fb_duration = 0u128;
    let mut total_non_fb_duration = 0u128;
    for i in 0..ITERATIONS {
        // Send tx
        let signed_tx = operations::native_balance_transfer(
            operations::DEFAULT_L2_NETWORK_URL,
            U256::from(operations::GWEI),
            test_address,
            None,
            true,
        )
        .await
        .unwrap();
        println!("Sent tx: {signed_tx}");

        // Run benchmark - both nodes check concurrently with independent timers
        let signed_tx_clone = signed_tx.clone();
        let fb_future = async {
            let start = Instant::now();
            tokio::time::timeout(TX_CONFIRMATION_TIMEOUT, async move {
                operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &signed_tx)
                    .await?;
                <Result<u128>>::Ok(start.elapsed().as_millis())
            })
            .await
            .expect("timeout waiting for tx to be mined")
        };

        let non_fb_future = async {
            let start = Instant::now();
            tokio::time::timeout(TX_CONFIRMATION_TIMEOUT, async move {
                operations::wait_for_tx_mined(
                    operations::DEFAULT_L2_NETWORK_URL_NO_FB,
                    &signed_tx_clone,
                )
                .await?;
                <Result<u128>>::Ok(start.elapsed().as_millis())
            })
            .await
            .expect("timeout waiting for tx to be mined")
        };

        let (fb_duration, non_fb_duration) = tokio::join!(fb_future, non_fb_future);
        let fb_duration = fb_duration.unwrap();
        let non_fb_duration = non_fb_duration.unwrap();
        total_fb_duration += fb_duration;
        total_non_fb_duration += non_fb_duration;

        println!("Iteration {i}");
        println!("Flashblocks native tx transfer confirmation took: {fb_duration}ms");
        println!("Non-flashblocks native tx transfer confirmation took: {non_fb_duration}ms");
    }

    let avg_fb_duration = total_fb_duration / ITERATIONS as u128;
    let avg_non_fb_duration = total_non_fb_duration / ITERATIONS as u128;

    // Log out metrics
    println!("Avg flashblocks native tx transfer confirmation took: {avg_fb_duration}ms");
    println!("Avg non-flashblocks native tx transfer confirmation took: {avg_non_fb_duration}ms");
}

/// Flashblock erc20 transfer tx confirmation benchmark between a flashblock node
/// and a non-flashblock node.
#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_benchmark_erc20_tx_confirmation_test() {
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    // Deploy contracts and get ERC20 address
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");
    println!("ERC20 contract at: {:#x}", contracts.erc20);

    // Benchmark erc20 transfer tx to test address
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let mut total_fb_duration = 0u128;
    let mut total_non_fb_duration = 0u128;
    for i in 0..ITERATIONS {
        // Send tx
        let signed_tx = operations::erc20_balance_transfer(
            operations::DEFAULT_L2_NETWORK_URL,
            U256::from(operations::GWEI),
            None,
            test_address,
            contracts.erc20,
            None,
        )
        .await
        .unwrap();
        println!("Sent erc20 tx: {signed_tx}");

        // Run benchmark - both nodes check concurrently with independent timers
        let signed_tx_clone = signed_tx.clone();
        let fb_future = async {
            let start = Instant::now();
            tokio::time::timeout(TX_CONFIRMATION_TIMEOUT, async move {
                operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &signed_tx)
                    .await?;
                <Result<u128>>::Ok(start.elapsed().as_millis())
            })
            .await
            .expect("timeout waiting for tx to be mined")
        };

        let non_fb_future = async {
            let start = Instant::now();
            tokio::time::timeout(TX_CONFIRMATION_TIMEOUT, async move {
                operations::wait_for_tx_mined(
                    operations::DEFAULT_L2_NETWORK_URL_NO_FB,
                    &signed_tx_clone,
                )
                .await?;
                <Result<u128>>::Ok(start.elapsed().as_millis())
            })
            .await
            .expect("timeout waiting for tx to be mined")
        };

        let (fb_duration, non_fb_duration) = tokio::join!(fb_future, non_fb_future);
        let fb_duration = fb_duration.unwrap();
        let non_fb_duration = non_fb_duration.unwrap();
        total_fb_duration += fb_duration;
        total_non_fb_duration += non_fb_duration;

        println!("Iteration {i}");
        println!("Flashblocks erc20 tx transfer confirmation took: {fb_duration}ms");
        println!("Non-flashblocks erc20 tx transfer confirmation took: {non_fb_duration}ms");
    }

    let avg_fb_duration = total_fb_duration / ITERATIONS as u128;
    let avg_non_fb_duration = total_non_fb_duration / ITERATIONS as u128;

    // Log out metrics
    println!("Avg flashblocks erc20 tx transfer confirmation took: {avg_fb_duration}ms");
    println!("Avg non-flashblocks erc20 tx transfer confirmation took: {avg_non_fb_duration}ms");
}

// ========================================================================
// Flashblocks Eth RPC API tests
// ========================================================================

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_block_number() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let non_fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_NO_FB);

    let fb_block_number = operations::eth_block_number(&fb_client)
        .await
        .expect("Failed to get block number from fb client");
    let non_fb_block_number = operations::eth_block_number(&non_fb_client)
        .await
        .expect("Failed to get block number from non-fb client");
    assert!(
        fb_block_number >= non_fb_block_number,
        "FB block number ({fb_block_number}) should be >= non-FB block number ({non_fb_block_number})"
    );
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_block_by_number() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let non_fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_NO_FB);

    let latest = operations::eth_block_number(&non_fb_client)
        .await
        .expect("Failed to get latest block number");

    for i in 0..10 {
        let block_id = operations::BlockId::Number(latest - i);
        let fb_block =
            operations::eth_get_block_by_number_or_hash(&fb_client, block_id.clone(), false)
                .await
                .expect("Failed to get block from fb client");
        let non_fb_block =
            operations::eth_get_block_by_number_or_hash(&non_fb_client, block_id, false)
                .await
                .expect("Failed to get block from non-fb client");
        assert_eq!(fb_block, non_fb_block, "eth_getBlockByNumber not identical");
    }
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_block_by_hash() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let non_fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_NO_FB);

    let latest = operations::eth_block_number(&non_fb_client)
        .await
        .expect("Failed to get latest block number");

    for i in 0..10 {
        let block_id = operations::BlockId::Number(latest - i);
        let block = operations::eth_get_block_by_number_or_hash(&fb_client, block_id, false)
            .await
            .expect("Failed to get block from fb client");
        let block_hash = operations::BlockId::Hash(
            block["hash"].as_str().expect("Block hash should not be empty").to_string(),
        );
        let fb_block =
            operations::eth_get_block_by_number_or_hash(&fb_client, block_hash.clone(), false)
                .await
                .expect("Failed to get block from fb client");
        let non_fb_block =
            operations::eth_get_block_by_number_or_hash(&non_fb_client, block_hash, false)
                .await
                .expect("Failed to get block from non-fb client");
        assert_eq!(fb_block, non_fb_block, "eth_getBlockByHash not identical");
    }
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_block_transaction_count() {
    let (fb_client, non_fb_client, _, block_num, fb_block_hash, non_fb_block_hash, _) =
        operations::comparison_test_setup().await;

    // By number
    let fb_count = operations::eth_get_block_transaction_count_by_number_or_hash(
        &fb_client,
        operations::BlockId::Number(block_num),
    )
    .await
    .expect("Failed to get block transaction count from fb client");
    let non_fb_count = operations::eth_get_block_transaction_count_by_number_or_hash(
        &non_fb_client,
        operations::BlockId::Number(block_num),
    )
    .await
    .expect("Failed to get block transaction count from non-fb client");
    assert_eq!(fb_count, non_fb_count, "eth_getBlockTransactionCountByNumber not identical");

    // By hash
    let fb_count_by_hash = operations::eth_get_block_transaction_count_by_number_or_hash(
        &fb_client,
        operations::BlockId::Hash(fb_block_hash),
    )
    .await
    .expect("Failed to get block transaction count by hash from fb client");
    let non_fb_count_by_hash = operations::eth_get_block_transaction_count_by_number_or_hash(
        &non_fb_client,
        operations::BlockId::Hash(non_fb_block_hash),
    )
    .await
    .expect("Failed to get block transaction count by hash from non-fb client");
    assert_eq!(fb_count, fb_count_by_hash, "FB node: count by hash should match by number");
    assert_eq!(
        non_fb_count, non_fb_count_by_hash,
        "Non-FB node: count by hash should match by number"
    );
    assert_eq!(
        fb_count_by_hash, non_fb_count_by_hash,
        "eth_getBlockTransactionCountByHash not identical"
    );
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_transaction_by_hash() {
    let (fb_client, non_fb_client, tx_hashes, _, _, _, _) =
        operations::comparison_test_setup().await;

    for tx_hash in &tx_hashes {
        let fb_tx = operations::eth_get_transaction_by_hash(&fb_client, tx_hash)
            .await
            .expect("Failed to get transaction from fb client");
        let non_fb_tx = operations::eth_get_transaction_by_hash(&non_fb_client, tx_hash)
            .await
            .expect("Failed to get transaction from non-fb client");
        assert_eq!(fb_tx, non_fb_tx, "eth_getTransactionByHash not identical");
    }
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_raw_transaction_by_hash() {
    let (fb_client, non_fb_client, tx_hashes, _, _, _, _) =
        operations::comparison_test_setup().await;

    for tx_hash in &tx_hashes {
        let fb_raw = operations::eth_get_raw_transaction_by_hash(&fb_client, tx_hash)
            .await
            .expect("Failed to get raw transaction from fb client");
        let non_fb_raw = operations::eth_get_raw_transaction_by_hash(&non_fb_client, tx_hash)
            .await
            .expect("Failed to get raw transaction from non-fb client");
        assert_eq!(fb_raw, non_fb_raw, "eth_getRawTransactionByHash not identical");
    }
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_transaction_receipt() {
    let (fb_client, non_fb_client, tx_hashes, _, _, _, _) =
        operations::comparison_test_setup().await;

    for tx_hash in &tx_hashes {
        let fb_receipt = operations::eth_get_transaction_receipt(&fb_client, tx_hash)
            .await
            .expect("Failed to get transaction receipt from fb client");
        let non_fb_receipt = operations::eth_get_transaction_receipt(&non_fb_client, tx_hash)
            .await
            .expect("Failed to get transaction receipt from non-fb client");
        assert_eq!(fb_receipt, non_fb_receipt, "eth_getTransactionReceipt not identical");
    }
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_transaction_by_block_number_and_index() {
    let (fb_client, non_fb_client, tx_hashes, block_num, _, _, _) =
        operations::comparison_test_setup().await;

    for tx_hash in &tx_hashes {
        let receipt = operations::eth_get_transaction_receipt(&fb_client, tx_hash)
            .await
            .expect("Failed to get transaction receipt from fb client");
        let tx_index_str =
            receipt["transactionIndex"].as_str().expect("Transaction index should not be empty");

        let fb_tx = operations::eth_get_transaction_by_block_number_or_hash_and_index(
            &fb_client,
            operations::BlockId::Number(block_num),
            tx_index_str,
        )
        .await
        .expect("Failed to get transaction from fb client");
        let non_fb_tx = operations::eth_get_transaction_by_block_number_or_hash_and_index(
            &non_fb_client,
            operations::BlockId::Number(block_num),
            tx_index_str,
        )
        .await
        .expect("Failed to get transaction from non-fb client");
        assert_eq!(fb_tx, non_fb_tx, "eth_getTransactionByBlockNumberAndIndex not identical");
    }
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_transaction_by_block_hash_and_index() {
    let (fb_client, non_fb_client, tx_hashes, _, fb_block_hash, non_fb_block_hash, _) =
        operations::comparison_test_setup().await;

    for tx_hash in &tx_hashes {
        let receipt = operations::eth_get_transaction_receipt(&fb_client, tx_hash)
            .await
            .expect("Failed to get transaction receipt from fb client");
        let tx_index_str =
            receipt["transactionIndex"].as_str().expect("Transaction index should not be empty");

        let fb_tx = operations::eth_get_transaction_by_block_number_or_hash_and_index(
            &fb_client,
            operations::BlockId::Hash(fb_block_hash.clone()),
            tx_index_str,
        )
        .await
        .expect("Failed to get transaction by block hash and index from fb client");
        let non_fb_tx = operations::eth_get_transaction_by_block_number_or_hash_and_index(
            &non_fb_client,
            operations::BlockId::Hash(non_fb_block_hash.clone()),
            tx_index_str,
        )
        .await
        .expect("Failed to get transaction by block hash and index from non-fb client");
        assert_eq!(fb_tx, non_fb_tx, "eth_getTransactionByBlockHashAndIndex not identical");
    }
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_raw_transaction_by_block_number_and_index() {
    let (fb_client, non_fb_client, tx_hashes, block_num, _, _, _) =
        operations::comparison_test_setup().await;

    let block_num_hex = format!("0x{block_num:x}");
    for tx_hash in &tx_hashes {
        let receipt = operations::eth_get_transaction_receipt(&fb_client, tx_hash)
            .await
            .expect("Failed to get transaction receipt from fb client");
        let tx_index_str =
            receipt["transactionIndex"].as_str().expect("Transaction index should not be empty");

        let fb_raw = operations::eth_get_raw_transaction_by_block_number_and_index(
            &fb_client,
            &block_num_hex,
            tx_index_str,
        )
        .await
        .expect("Failed to get raw tx by block number and index from fb client");
        let non_fb_raw = operations::eth_get_raw_transaction_by_block_number_and_index(
            &non_fb_client,
            &block_num_hex,
            tx_index_str,
        )
        .await
        .expect("Failed to get raw tx by block number and index from non-fb client");
        assert_eq!(fb_raw, non_fb_raw, "eth_getRawTransactionByBlockNumberAndIndex not identical");
    }
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_raw_transaction_by_block_hash_and_index() {
    let (fb_client, non_fb_client, tx_hashes, _, fb_block_hash, non_fb_block_hash, _) =
        operations::comparison_test_setup().await;

    for tx_hash in &tx_hashes {
        let receipt = operations::eth_get_transaction_receipt(&fb_client, tx_hash)
            .await
            .expect("Failed to get transaction receipt from fb client");
        let tx_index_str =
            receipt["transactionIndex"].as_str().expect("Transaction index should not be empty");

        let fb_raw = operations::eth_get_raw_transaction_by_block_hash_and_index(
            &fb_client,
            &fb_block_hash,
            tx_index_str,
        )
        .await
        .expect("Failed to get raw tx by block hash and index from fb client");
        let non_fb_raw = operations::eth_get_raw_transaction_by_block_hash_and_index(
            &non_fb_client,
            &non_fb_block_hash,
            tx_index_str,
        )
        .await
        .expect("Failed to get raw tx by block hash and index from non-fb client");
        assert_eq!(fb_raw, non_fb_raw, "eth_getRawTransactionByBlockHashAndIndex not identical");
    }
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_block_receipts() {
    let (fb_client, non_fb_client, _, block_num, _, _, _) =
        operations::comparison_test_setup().await;

    let fb_receipts =
        operations::eth_get_block_receipts(&fb_client, operations::BlockId::Number(block_num))
            .await
            .expect("Failed to get block receipts from fb client");
    let non_fb_receipts =
        operations::eth_get_block_receipts(&non_fb_client, operations::BlockId::Number(block_num))
            .await
            .expect("Failed to get block receipts from non-fb client");
    assert_eq!(fb_receipts, non_fb_receipts, "eth_getBlockReceipts not identical");
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_logs() {
    let (fb_client, non_fb_client, _, block_num, fb_block_hash, non_fb_block_hash, _) =
        operations::comparison_test_setup().await;

    // By block number range
    let fb_logs = operations::eth_get_logs(
        &fb_client,
        Some(operations::BlockId::Number(block_num)),
        Some(operations::BlockId::Number(block_num)),
        None,
        None,
    )
    .await
    .expect("Failed to get logs from fb client");
    let non_fb_logs = operations::eth_get_logs(
        &non_fb_client,
        Some(operations::BlockId::Number(block_num)),
        Some(operations::BlockId::Number(block_num)),
        None,
        None,
    )
    .await
    .expect("Failed to get logs from non-fb client");
    assert_eq!(fb_logs, non_fb_logs, "eth_getLogs by block range not identical");

    // By block hash
    let fb_logs_by_hash =
        operations::eth_get_logs_by_block_hash(&fb_client, &fb_block_hash, None, None)
            .await
            .expect("Failed to get logs by block hash from fb client");
    let non_fb_logs_by_hash =
        operations::eth_get_logs_by_block_hash(&non_fb_client, &non_fb_block_hash, None, None)
            .await
            .expect("Failed to get logs by block hash from non-fb client");
    assert_eq!(
        fb_logs, fb_logs_by_hash,
        "FB node: eth_getLogs by hash should match by block range"
    );
    assert_eq!(
        non_fb_logs, non_fb_logs_by_hash,
        "Non-FB node: eth_getLogs by hash should match by block range"
    );
    assert_eq!(fb_logs_by_hash, non_fb_logs_by_hash, "eth_getLogs by block hash not identical");
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_call() {
    let (fb_client, non_fb_client, _, block_num, fb_block_hash, non_fb_block_hash, contracts) =
        operations::comparison_test_setup().await;
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    sol! {
        function balanceOf(address account) external view returns (uint256);
    }
    let call = balanceOfCall { account: Address::from_str(test_address).expect("Invalid address") };
    let calldata = call.abi_encode();
    let call_args = serde_json::json!({
        "from": test_address,
        "to": contracts.erc20,
        "gas": "0x100000",
        "data": format!("0x{}", hex::encode(&calldata)),
    });

    // By block number
    let fb_call = operations::eth_call(
        &fb_client,
        Some(call_args.clone()),
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to call from fb client");
    let non_fb_call = operations::eth_call(
        &non_fb_client,
        Some(call_args.clone()),
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to call from non-fb client");
    assert_eq!(fb_call, non_fb_call, "eth_call with block number not identical");

    // By block hash
    let fb_call_by_hash = operations::eth_call(
        &fb_client,
        Some(call_args.clone()),
        Some(operations::BlockId::Hash(fb_block_hash)),
    )
    .await
    .expect("Failed to call from fb client by hash");
    let non_fb_call_by_hash = operations::eth_call(
        &non_fb_client,
        Some(call_args),
        Some(operations::BlockId::Hash(non_fb_block_hash)),
    )
    .await
    .expect("Failed to call from non-fb client by hash");
    assert_eq!(fb_call, fb_call_by_hash, "FB node: eth_call by hash should match by number");
    assert_eq!(
        non_fb_call, non_fb_call_by_hash,
        "Non-FB node: eth_call by hash should match by number"
    );
    assert_eq!(fb_call_by_hash, non_fb_call_by_hash, "eth_call with block hash not identical");
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_estimate_gas() {
    let (fb_client, non_fb_client, _, block_num, fb_block_hash, non_fb_block_hash, contracts) =
        operations::comparison_test_setup().await;
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    sol! {
        function balanceOf(address account) external view returns (uint256);
    }
    let call = balanceOfCall { account: Address::from_str(test_address).expect("Invalid address") };
    let calldata = call.abi_encode();
    let estimate_args = serde_json::json!({
        "from": test_address,
        "to": contracts.erc20,
        "gas": "0x100000",
        "data": format!("0x{}", hex::encode(&calldata)),
    });

    // By block number
    let fb_estimate = operations::estimate_gas(
        &fb_client,
        Some(estimate_args.clone()),
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to estimate gas from fb client");
    let non_fb_estimate = operations::estimate_gas(
        &non_fb_client,
        Some(estimate_args.clone()),
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to estimate gas from non-fb client");
    assert_eq!(fb_estimate, non_fb_estimate, "eth_estimateGas with block number not identical");

    // By block hash
    let fb_estimate_by_hash = operations::estimate_gas(
        &fb_client,
        Some(estimate_args.clone()),
        Some(operations::BlockId::Hash(fb_block_hash)),
    )
    .await
    .expect("Failed to estimate gas from fb client by hash");
    let non_fb_estimate_by_hash = operations::estimate_gas(
        &non_fb_client,
        Some(estimate_args),
        Some(operations::BlockId::Hash(non_fb_block_hash)),
    )
    .await
    .expect("Failed to estimate gas from non-fb client by hash");
    assert_eq!(
        fb_estimate, fb_estimate_by_hash,
        "FB node: eth_estimateGas by hash should match by number"
    );
    assert_eq!(
        non_fb_estimate, non_fb_estimate_by_hash,
        "Non-FB node: eth_estimateGas by hash should match by number"
    );
    assert_eq!(
        fb_estimate_by_hash, non_fb_estimate_by_hash,
        "eth_estimateGas with block hash not identical"
    );
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_balance() {
    let (fb_client, non_fb_client, _, block_num, fb_block_hash, non_fb_block_hash, _) =
        operations::comparison_test_setup().await;
    let sender_address = operations::DEFAULT_RICH_ADDRESS;

    // By block number
    let fb_balance = operations::get_balance(
        &fb_client,
        sender_address,
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to get balance from fb client");
    let non_fb_balance = operations::get_balance(
        &non_fb_client,
        sender_address,
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to get balance from non-fb client");
    assert_eq!(fb_balance, non_fb_balance, "eth_getBalance not identical");

    // By block hash
    let fb_balance_by_hash = operations::get_balance(
        &fb_client,
        sender_address,
        Some(operations::BlockId::Hash(fb_block_hash)),
    )
    .await
    .expect("Failed to get balance from fb client by hash");
    let non_fb_balance_by_hash = operations::get_balance(
        &non_fb_client,
        sender_address,
        Some(operations::BlockId::Hash(non_fb_block_hash)),
    )
    .await
    .expect("Failed to get balance from non-fb client by hash");
    assert_eq!(
        fb_balance, fb_balance_by_hash,
        "FB node: eth_getBalance by hash should match by number"
    );
    assert_eq!(
        non_fb_balance, non_fb_balance_by_hash,
        "Non-FB node: eth_getBalance by hash should match by number"
    );
    assert_eq!(
        fb_balance_by_hash, non_fb_balance_by_hash,
        "eth_getBalance with block hash not identical"
    );
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_transaction_count() {
    let (fb_client, non_fb_client, _, block_num, fb_block_hash, non_fb_block_hash, _) =
        operations::comparison_test_setup().await;
    let sender_address = operations::DEFAULT_RICH_ADDRESS;

    // By block number
    let fb_count = operations::eth_get_transaction_count(
        &fb_client,
        sender_address,
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to get transaction count from fb client");
    let non_fb_count = operations::eth_get_transaction_count(
        &non_fb_client,
        sender_address,
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to get transaction count from non-fb client");
    assert_eq!(fb_count, non_fb_count, "eth_getTransactionCount not identical");

    // By block hash
    let fb_count_by_hash = operations::eth_get_transaction_count(
        &fb_client,
        sender_address,
        Some(operations::BlockId::Hash(fb_block_hash)),
    )
    .await
    .expect("Failed to get transaction count from fb client by hash");
    let non_fb_count_by_hash = operations::eth_get_transaction_count(
        &non_fb_client,
        sender_address,
        Some(operations::BlockId::Hash(non_fb_block_hash)),
    )
    .await
    .expect("Failed to get transaction count from non-fb client by hash");
    assert_eq!(
        fb_count, fb_count_by_hash,
        "FB node: eth_getTransactionCount by hash should match by number"
    );
    assert_eq!(
        non_fb_count, non_fb_count_by_hash,
        "Non-FB node: eth_getTransactionCount by hash should match by number"
    );
    assert_eq!(
        fb_count_by_hash, non_fb_count_by_hash,
        "eth_getTransactionCount with block hash not identical"
    );
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_code() {
    let (fb_client, non_fb_client, _, block_num, fb_block_hash, non_fb_block_hash, contracts) =
        operations::comparison_test_setup().await;
    let erc20_addr = contracts.erc20.to_string();

    // By block number
    let fb_code = operations::eth_get_code(
        &fb_client,
        &erc20_addr,
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to get code from fb client");
    let non_fb_code = operations::eth_get_code(
        &non_fb_client,
        &erc20_addr,
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to get code from non-fb client");
    assert_eq!(fb_code, non_fb_code, "eth_getCode with block number not identical");

    // By block hash
    let fb_code_by_hash = operations::eth_get_code(
        &fb_client,
        &erc20_addr,
        Some(operations::BlockId::Hash(fb_block_hash)),
    )
    .await
    .expect("Failed to get code from fb client by hash");
    let non_fb_code_by_hash = operations::eth_get_code(
        &non_fb_client,
        &erc20_addr,
        Some(operations::BlockId::Hash(non_fb_block_hash)),
    )
    .await
    .expect("Failed to get code from non-fb client by hash");
    assert_eq!(fb_code, fb_code_by_hash, "FB node: eth_getCode by hash should match by number");
    assert_eq!(
        non_fb_code, non_fb_code_by_hash,
        "Non-FB node: eth_getCode by hash should match by number"
    );
    assert_eq!(fb_code_by_hash, non_fb_code_by_hash, "eth_getCode with block hash not identical");
}

#[ignore = "Requires a second non-flashblock RPC node to be running"]
#[tokio::test]
async fn fb_compare_get_storage_at() {
    let (fb_client, non_fb_client, _, block_num, fb_block_hash, non_fb_block_hash, contracts) =
        operations::comparison_test_setup().await;
    let erc20_addr = contracts.erc20.to_string();

    // By block number
    let fb_storage = operations::eth_get_storage_at(
        &fb_client,
        &erc20_addr,
        "0x2",
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to get storage from fb client");
    let non_fb_storage = operations::eth_get_storage_at(
        &non_fb_client,
        &erc20_addr,
        "0x2",
        Some(operations::BlockId::Number(block_num)),
    )
    .await
    .expect("Failed to get storage from non-fb client");
    assert_eq!(fb_storage, non_fb_storage, "eth_getStorageAt with block number not identical");

    // By block hash
    let fb_storage_by_hash = operations::eth_get_storage_at(
        &fb_client,
        &erc20_addr,
        "0x2",
        Some(operations::BlockId::Hash(fb_block_hash)),
    )
    .await
    .expect("Failed to get storage from fb client by hash");
    let non_fb_storage_by_hash = operations::eth_get_storage_at(
        &non_fb_client,
        &erc20_addr,
        "0x2",
        Some(operations::BlockId::Hash(non_fb_block_hash)),
    )
    .await
    .expect("Failed to get storage from non-fb client by hash");
    assert_eq!(
        fb_storage, fb_storage_by_hash,
        "FB node: eth_getStorageAt by hash should match by number"
    );
    assert_eq!(
        non_fb_storage, non_fb_storage_by_hash,
        "Non-FB node: eth_getStorageAt by hash should match by number"
    );
    assert_eq!(
        fb_storage_by_hash, non_fb_storage_by_hash,
        "eth_getStorageAt with block hash not identical"
    );
}

/// Verifies `eth_sendRawTransactionSync` returns inclusion data synchronously.
#[tokio::test]
async fn fb_send_raw_transaction_sync_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    let raw_tx = operations::sign_raw_transaction(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        test_address,
    )
    .await
    .expect("Failed to sign raw transaction");

    let response = operations::eth_send_raw_transaction_sync(&fb_client, &raw_tx)
        .await
        .expect("eth_sendRawTransactionSync failed");
    assert!(!response.is_null(), "Response should not be null");

    // Verify sync inclusion data in the response
    let tx_hash =
        response["transactionHash"].as_str().expect("Response must contain transactionHash");
    assert!(tx_hash.starts_with("0x"), "Transaction hash should start with 0x");

    let block_number = response["blockNumber"]
        .as_str()
        .expect("Response must contain blockNumber for sync inclusion proof");
    assert!(block_number.starts_with("0x"), "Block number should be hex-encoded");

    // Verify the tx is immediately visible
    let tx = operations::eth_get_transaction_by_hash(&fb_client, tx_hash)
        .await
        .expect("eth_getTransactionByHash after sync send failed");
    assert!(!tx.is_null(), "Transaction should be visible after eth_sendRawTransactionSync");
}

/// Negative tests for flashblock RPCs
#[tokio::test]
async fn fb_negative_test() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);

    // Non-existent block returns null
    let result = operations::eth_get_block_by_number_or_hash(
        &fb_client,
        operations::BlockId::Number(0xFFFFFFFF),
        false,
    )
    .await
    .expect("eth_getBlockByNumber should not return an RPC error for non-existent block");
    assert!(result.is_null(), "Non-existent block should return null, got: {result}");

    // Non-existent tx returns null
    let fake_hash = "0x0000000000000000000000000000000000000000000000000000000000000001";

    let tx = operations::eth_get_transaction_by_hash(&fb_client, fake_hash)
        .await
        .expect("eth_getTransactionByHash should not return an RPC error for non-existent tx");
    assert!(tx.is_null(), "Non-existent tx should return null, got: {tx}");

    let receipt = operations::eth_get_transaction_receipt(&fb_client, fake_hash)
        .await
        .expect("eth_getTransactionReceipt should not return an RPC error for non-existent tx");
    assert!(receipt.is_null(), "Non-existent tx receipt should return null, got: {receipt}");

    // Invalid raw tx is rejected
    let result = operations::eth_send_raw_transaction_sync(&fb_client, "0xdeadbeef").await;
    assert!(
        result.is_err(),
        "eth_sendRawTransactionSync with invalid tx should return an error, got: {result:?}"
    );

    // eth_call with invalid selector reverts
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");

    let call_args = json!({
        "from": operations::DEFAULT_RICH_ADDRESS,
        "to": contracts.erc20,
        "gas": "0x100000",
        "data": "0xdeadbeef",
    });

    let result =
        operations::eth_call(&fb_client, Some(call_args), Some(operations::BlockId::Pending)).await;
    assert!(result.is_err(), "eth_call with invalid selector should revert, got: {result:?}");

    // eth_getRawTransactionByBlockNumberAndIndex with out-of-range index returns null.
    // Use an actual block that exists (latest), then query a tx index that doesn't exist.
    let current_block =
        operations::eth_block_number(&fb_client).await.expect("Failed to get block number");
    let block_hex = format!("0x{current_block:x}");
    let result = operations::eth_get_raw_transaction_by_block_number_and_index(
        &fb_client, &block_hex, "0xFFFF",
    )
    .await
    .expect("eth_getRawTransactionByBlockNumberAndIndex should not error for bad index");
    assert!(result.is_null(), "Out-of-range tx index should return null, got: {result}");
}

/// Verifies that `eth_getLogs` returns logs from a flashblock when queried
/// with the exact block number the transaction landed in.
#[tokio::test]
async fn fb_get_logs_by_block_number_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    let tx_hash = operations::erc20_balance_transfer(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        None,
        test_address,
        contracts.erc20,
        None,
    )
    .await
    .expect("ERC20 transfer failed");

    let receipt = operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &tx_hash)
        .await
        .expect("ERC20 transfer tx not mined");

    let tx_block_number = receipt["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("Failed to parse block number from receipt");

    // Wait for canonical chain to reach the tx block so eth_getLogs can serve it
    operations::wait_for_canonical_block(operations::DEFAULT_L2_NETWORK_URL_FB, tx_block_number)
        .await
        .expect("Canonical chain did not reach tx block");

    let transfer_topic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
    let logs = operations::eth_get_logs(
        &fb_client,
        Some(operations::BlockId::Number(tx_block_number)),
        Some(operations::BlockId::Number(tx_block_number)),
        Some(&format!("{:#x}", contracts.erc20)),
        Some(vec![transfer_topic.to_string()]),
    )
    .await
    .expect("eth_getLogs with block number failed");

    let logs_arr = logs.as_array().expect("Logs should be an array");
    assert!(!logs_arr.is_empty(), "Should have at least one Transfer log");

    let our_log = logs_arr
        .iter()
        .find(|log| log["transactionHash"].as_str() == Some(tx_hash.as_str()))
        .expect("Should find our Transfer log by block number");

    assert_eq!(
        our_log["address"].as_str().unwrap().to_lowercase(),
        format!("{:#x}", contracts.erc20).to_lowercase(),
        "Log address should match ERC20 contract"
    );
}

/// Verifies that `eth_getLogs` with `blockHash` filter returns logs from a
/// flashblock-cached block.
#[tokio::test]
async fn fb_get_logs_by_block_hash_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    // Send a transfer
    let tx_hash = operations::erc20_balance_transfer(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        None,
        test_address,
        contracts.erc20,
        None,
    )
    .await
    .expect("ERC20 transfer failed");

    let receipt = operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &tx_hash)
        .await
        .expect("ERC20 transfer tx not mined");

    let tx_block_number = receipt["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("Failed to parse block number from receipt");

    // Wait for canonical chain to reach the tx block so eth_getLogs can serve it
    operations::wait_for_canonical_block(operations::DEFAULT_L2_NETWORK_URL_FB, tx_block_number)
        .await
        .expect("Canonical chain did not reach tx block");

    // Re-fetch the canonical block hash (flashblock hash may differ from canonical hash)
    let canonical_block = operations::eth_get_block_by_number_or_hash(
        &fb_client,
        operations::BlockId::Number(tx_block_number),
        false,
    )
    .await
    .expect("Failed to get canonical block");
    let block_hash = canonical_block["hash"].as_str().expect("Block should have hash");

    // Query logs by blockHash
    let transfer_topic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
    let logs = operations::eth_get_logs_by_block_hash(
        &fb_client,
        block_hash,
        Some(&format!("{:#x}", contracts.erc20)),
        Some(vec![transfer_topic.to_string()]),
    )
    .await
    .expect("eth_getLogs with blockHash failed");

    let logs_arr = logs.as_array().expect("Logs should be an array");
    assert!(!logs_arr.is_empty(), "Should have at least one Transfer log from block hash query");

    let our_log = logs_arr
        .iter()
        .find(|log| log["transactionHash"].as_str() == Some(tx_hash.as_str()))
        .expect("Should find our Transfer log by block hash");

    assert_eq!(
        our_log["address"].as_str().unwrap().to_lowercase(),
        format!("{:#x}", contracts.erc20).to_lowercase(),
        "Log address should match ERC20 contract"
    );
}

/// Verifies that `eth_getLogs` with a range spanning both canonical and
/// flashblock-cached blocks returns logs in ascending order.
#[tokio::test]
async fn fb_get_logs_cross_boundary_range_test() {
    operations::settle_pending_transactions(operations::DEFAULT_L2_NETWORK_URL_FB)
        .await
        .expect("Failed to settle pending transactions");

    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    // Get the current canonical height
    let canonical_height =
        operations::eth_block_number(&fb_client).await.expect("Failed to get block number");

    // Send a transfer so a flashblock ahead of canonical has a log
    let tx_hash = operations::erc20_balance_transfer(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        None,
        test_address,
        contracts.erc20,
        None,
    )
    .await
    .expect("ERC20 transfer failed");

    let receipt = operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &tx_hash)
        .await
        .expect("ERC20 transfer tx not mined");

    let tx_block_number = receipt["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("Failed to parse block number from receipt");

    // Wait for canonical chain to reach the tx block so eth_getLogs can serve it
    operations::wait_for_canonical_block(operations::DEFAULT_L2_NETWORK_URL_FB, tx_block_number)
        .await
        .expect("Canonical chain did not reach tx block");

    // Query a range that starts a few blocks before canonical height and goes
    // to latest, ensuring we cross the canonical → flashblock boundary.
    let from_block = canonical_height.saturating_sub(2);
    let transfer_topic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
    let logs = operations::eth_get_logs(
        &fb_client,
        Some(operations::BlockId::Number(from_block)),
        Some(operations::BlockId::Pending),
        Some(&format!("{:#x}", contracts.erc20)),
        Some(vec![transfer_topic.to_string()]),
    )
    .await
    .expect("eth_getLogs cross-boundary range failed");

    let logs_arr = logs.as_array().expect("Logs should be an array");

    // Verify ascending block number order
    let mut prev_block: u64 = 0;
    for log in logs_arr {
        let block_num = log["blockNumber"]
            .as_str()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .expect("Log should have a valid blockNumber");
        assert!(
            block_num >= prev_block,
            "Logs must be in ascending block order: got {block_num} after {prev_block}"
        );
        prev_block = block_num;
    }

    // Our transfer should be in the results
    let our_log =
        logs_arr.iter().find(|log| log["transactionHash"].as_str() == Some(tx_hash.as_str()));
    assert!(our_log.is_some(), "Should find our Transfer log in cross-boundary range");
}

// ========================================================================
// Flashblocks subscription tests
// ========================================================================

#[ignore = "Requires sequencer 2 and RPC node with flashblocks enabled to be running"]
#[tokio::test]
async fn fb_subscription_test() -> Result<()> {
    // Source of flashblocks is from the RPC node which obtains flashblocks from the sequencer
    let ws_url_seq = operations::manager::DEFAULT_SEQ_FLASHBLOCKS_WS_URL;
    let ws_url_seq2 = operations::manager::DEFAULT_SEQ_2_FLASHBLOCKS_WS_URL;
    let ws_url_rpc = operations::manager::DEFAULT_RPC_FLASHBLOCKS_WS_URL;

    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;
    let rpc_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL);

    let current_block_number = operations::eth_block_number(&rpc_client)
        .await
        .expect("Failed to get current block number");
    println!("Current block number: {current_block_number}");

    let num_txs: usize = 5;

    let (ws_stream_seq, _) = connect_async(ws_url_seq).await?;
    let (_, mut read_seq) = ws_stream_seq.split();

    let (ws_stream_seq2, _) = connect_async(ws_url_seq2).await?;
    let (_, mut read_seq2) = ws_stream_seq2.split();

    let (ws_stream_rpc, _) = connect_async(ws_url_rpc).await?;
    let (_, mut read_rpc) = ws_stream_rpc.split();

    let mut count: HashMap<String, u64> = HashMap::new();
    for i in 0..num_txs {
        let tx_hash = operations::native_balance_transfer(
            operations::DEFAULT_L2_NETWORK_URL_FB,
            U256::from(operations::GWEI),
            test_address,
            None,
            true,
        )
        .await?;
        println!("Sent tx {}: {}", i + 1, tx_hash);
        count.insert(tx_hash, 0);
    }
    println!(
        "Waiting for {num_txs} txs to appear in flashblocks (timeout: {WEB_SOCKET_TIMEOUT:?})..."
    );

    // Read flashblocks until all txs are found or timeout
    let _ = tokio::time::timeout(WEB_SOCKET_TIMEOUT, async {
        loop {
            // Each tx must be seen in all 3 streams: SEQ, SEQ2, and RPC
            let all_found = count.values().all(|&c| c >= 3);
            if all_found {
                break;
            }

            tokio::select! {
                msg_seq = read_seq.next() => {
                    // Process message from sequencer stream
                    if let Some(Ok(Message::Text(msg))) = msg_seq {
                        operations::process_flashblock_message(&msg, &mut count, current_block_number, "SEQ");
                    }
                }
                msg_seq2 = read_seq2.next() => {
                    // Process message from sequencer stream
                    if let Some(Ok(Message::Text(msg))) = msg_seq2 {
                        operations::process_flashblock_message(&msg, &mut count, current_block_number, "SEQ2");
                    }
                }
                msg_rpc = read_rpc.next() => {
                    // Process message from RPC stream
                    if let Some(Ok(Message::Text(msg))) = msg_rpc {
                        operations::process_flashblock_message(&msg, &mut count, current_block_number, "RPC");
                    }
                }
            }
        }
    })
    .await;

    for (tx_hash, &count_val) in &count {
        assert_eq!(
            count_val, 3,
            "Transaction {tx_hash} appeared {count_val} times, expected exactly 3 (one per stream: SEQ, SEQ2, RPC)"
        );
    }

    Ok(())
}

#[ignore = "Requires flashblocks WebSocket server with flashblocks subscription support"]
#[tokio::test]
async fn fb_eth_subscribe_test() -> Result<()> {
    let ws_url = operations::manager::DEFAULT_WEBSOCKET_URL;
    let sender_address = operations::DEFAULT_RICH_ADDRESS;
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    println!("Connecting to flashblocks WebSocket at {ws_url}...");
    let ws_client = operations::websocket::EthWebSocketClient::connect(ws_url).await?;
    println!("Connected successfully");

    let subscription_params = json!({
        "headerInfo": true,
        "subTxFilter": {
            "txInfo": true,
            "txReceipt": true,
            "subscribeAddresses": [sender_address, test_address]
        }
    });

    let mut subscription: jsonrpsee::core::client::Subscription<Value> =
        ws_client.subscribe("flashblocks", Some(subscription_params)).await?;
    println!("Subscription created successfully");

    let num_txs = 3;

    let mut remaining: HashSet<String> = HashSet::new();
    for i in 0..num_txs {
        let tx_hash = operations::native_balance_transfer(
            operations::DEFAULT_L2_NETWORK_URL_FB,
            U256::from(operations::GWEI),
            test_address,
            None,
            true,
        )
        .await?;
        println!("Sent tx {}: {}", i + 1, tx_hash);
        remaining.insert(tx_hash);
    }
    let total = remaining.len();
    println!(
        "Waiting for {total} txs to appear in flashblocks (timeout: {WEB_SOCKET_TIMEOUT:?})..."
    );

    let result = tokio::time::timeout(WEB_SOCKET_TIMEOUT, async {
        while !remaining.is_empty() {
            match subscription.next().await {
                Some(Ok(notification)) => {
                    // Skip header messages
                    let Some(type_field) = notification.get("type") else {
                        panic!("Notification missing 'type' field");
                    };

                    let type_str = type_field.as_str().expect("type should be a string");
                    if type_str == "header" {
                        continue;
                    }

                    assert_eq!(type_str, "transaction", "Expected transaction event");

                    let Some(tx) = notification.get("transaction") else {
                        panic!("Transaction event missing 'transaction' field");
                    };

                    // Validate enrichment fields
                    assert!(
                        tx.get("txData").is_some(),
                        "txData field should be present when txInfo is true"
                    );

                    assert!(
                        tx.get("receipt").is_some(),
                        "receipt field should be present when txReceipt is true"
                    );

                    // Extract tx hash
                    let tx_hash_field =
                        tx.get("txHash").expect("txHash field should always be present");
                    let received_hash = tx_hash_field.as_str().expect("txHash should be a string");

                    if remaining.remove(received_hash) {
                        let found = total - remaining.len();
                        println!("Found tx {found}/{total}: {received_hash}");
                    }
                }
                Some(Err(e)) => {
                    eprintln!("Subscription error: {e}");
                    break;
                }
                None => {
                    eprintln!("Subscription ended unexpectedly");
                    break;
                }
            }
        }
    })
    .await;

    if result.is_err() {
        eprintln!("Timeout: Stopped waiting after {WEB_SOCKET_TIMEOUT:?}");
    }

    if !remaining.is_empty() {
        eprintln!("\nMissing txs in flashblocks:");
        for tx in &remaining {
            eprintln!("  - {tx}");
        }
    }

    assert!(
        remaining.is_empty(),
        "Expected all {} txs to appear in flashblocks, but {} were missing",
        total,
        remaining.len()
    );

    println!("\nAll {total} transactions received via flashblocks subscription");

    Ok(())
}

#[ignore = "Requires flashblocks WebSocket server with flashblocks subscription support"]
#[tokio::test]
async fn fb_eth_subscribe_empty_params_test() -> Result<()> {
    let ws_url = operations::manager::DEFAULT_WEBSOCKET_URL;
    let ws_client = operations::websocket::EthWebSocketClient::connect(ws_url).await?;
    println!("Connected successfully");

    // Expect error
    let _ = ws_client
        .subscribe("flashblocks", Option::<Value>::None)
        .await
        .expect_err("Expected subscription to fail with invalid params");

    Ok(())
}

#[ignore = "Requires flashblocks WebSocket server with flashblocks subscription support"]
#[tokio::test]
async fn fb_benchmark_new_heads_subscription_test() -> Result<()> {
    let ws_url = operations::manager::DEFAULT_WEBSOCKET_URL;

    println!("Connecting to flashblocks WebSocket at {ws_url}...");
    let ws_client = operations::websocket::EthWebSocketClient::connect(ws_url).await?;
    println!("Connected successfully");

    let subscription_params = json!({
        "headerInfo": true
    });

    let mut flashblocks_subscription: jsonrpsee::core::client::Subscription<Value> =
        ws_client.subscribe("flashblocks", Some(subscription_params)).await?;
    println!("Flashblocks subscription created successfully");

    let mut eth_subscription: jsonrpsee::core::client::Subscription<Value> =
        ws_client.subscribe("newHeads", None).await?;
    println!("Eth newHeads subscription created successfully");

    let mut total_sub_time_diff = Duration::ZERO;
    let mut heights = HashMap::<u64, Instant>::new();
    let mut count = 0;

    println!("Starting benchmark: comparing flashblocks vs eth newHeads subscriptions...");

    while count < ITERATIONS {
        tokio::select! {
            // Flashblocks subscription message
            Some(result) = flashblocks_subscription.next() => {
                match result {
                    Ok(notification) => {
                        if let Some(header) = notification.get("header")
                            && let Some(number_hex) = header.get("number").and_then(|n| n.as_str())
                            && let Ok(height) = u64::from_str_radix(number_hex.trim_start_matches("0x"), 16)
                        {
                            heights.insert(height, Instant::now());
                            println!("Flashblocks received block #{height}");
                        }
                    }
                    Err(e) => {
                        eprintln!("Flashblocks subscription error: {e}");
                        break;
                    }
                }
            }
            // Eth newHeads subscription message
            Some(result) = eth_subscription.next() => {
                match result {
                    Ok(notification) => {
                        if let Some(number_hex) = notification.get("number").and_then(|n| n.as_str())
                            && let Ok(height) = u64::from_str_radix(number_hex.trim_start_matches("0x"), 16)
                        {
                            if let Some(flashblocks_time) = heights.get(&height) {
                                let time_diff = flashblocks_time.elapsed();
                                count += 1;

                                if count == 1 {
                                    continue;
                                }

                                total_sub_time_diff += time_diff;
                                println!(
                                    "Flashblocks newHeads sub is faster than ETH newHeads sub by: {time_diff:?} (block #{height})"
                                );
                            } else {
                                println!("Eth newHeads received block #{height} (not in flashblocks yet)");
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Eth subscription error: {e}");
                        break;
                    }
                }
            }
        }
    }

    let avg_time_diff = total_sub_time_diff / (ITERATIONS - 1) as u32;
    println!("\n=== Benchmark Results ===");
    println!("Avg Flashblocks newHeads sub is faster than ETH newHeads sub by: {avg_time_diff:?}");

    Ok(())
}

#[ignore = "Requires flashblocks WebSocket server with flashblocks subscription support"]
#[tokio::test]
async fn fb_benchmark_new_transactions_subscription_test() -> Result<()> {
    let ws_url = operations::manager::DEFAULT_WEBSOCKET_URL;
    let sender_address = operations::DEFAULT_RICH_ADDRESS;
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    println!("Connecting to flashblocks WebSocket at {ws_url}...");
    let ws_client = operations::websocket::EthWebSocketClient::connect(ws_url).await?;
    println!("Connected successfully");

    let subscription_params = json!({
        "headerInfo": false,
        "subTxFilter": {
            "txInfo": true,
            "txReceipt": true,
            "subscribeAddresses": [sender_address, test_address]
        }
    });

    let mut flashblocks_subscription: jsonrpsee::core::client::Subscription<Value> =
        ws_client.subscribe("flashblocks", Some(subscription_params)).await?;
    println!("Flashblocks subscription created successfully");

    let mut total_flashblocks_duration = Duration::ZERO;

    for i in 0..ITERATIONS {
        println!("\n--- Iteration {i} ---");

        let tx_hash = operations::native_balance_transfer(
            operations::DEFAULT_L2_NETWORK_URL_FB,
            U256::from(operations::GWEI),
            test_address,
            None,
            false,
        )
        .await?;

        println!("Sent transaction: {tx_hash}");
        let start_time = Instant::now();

        // Wait for transaction to appear in flashblocks subscription
        let sub_duration = tokio::time::timeout(TX_CONFIRMATION_TIMEOUT, async {
            loop {
                match flashblocks_subscription.next().await {
                    Some(Ok(notification)) => {
                        if operations::contains_tx_hash(&notification, &tx_hash) {
                            return Ok(start_time.elapsed());
                        }
                    }
                    Some(Err(e)) => {
                        eprintln!("Flashblocks subscription error: {e}");
                        return Err(eyre::eyre!("Subscription error: {e}"));
                    }
                    None => {
                        eprintln!("Flashblocks subscription ended unexpectedly");
                        return Err(eyre::eyre!("Subscription ended"));
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            eyre::eyre!("Timeout waiting for transaction in flashblocks subscription")
        })??;

        println!("Flashblocks newTx sub duration: {sub_duration:?}");

        if i == 0 {
            continue;
        }

        total_flashblocks_duration += sub_duration;
    }

    let avg_duration = total_flashblocks_duration / (ITERATIONS - 1) as u32;
    println!("\n=== Benchmark Results ===");
    println!("Avg Flashblocks newTx sub duration: {avg_duration:?}");

    Ok(())
}
