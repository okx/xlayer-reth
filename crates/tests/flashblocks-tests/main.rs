//! Functional tests for flashblocks e2e tests
//!
//! Run all tests with: `cargo test -p xlayer-e2e-test --test flashblocks_tests -- --nocapture --test-threads=1`
//! or run a specific test with: `cargo test -p xlayer-e2e-test --test flashblocks_tests -- <test_case_name> --nocapture --test-threads=1`
//!

use alloy_primitives::{hex, keccak256, Address, U256};
use alloy_sol_types::{sol, SolCall};
use eyre::Result;
use futures_util::StreamExt;
use scopeguard::defer;
use std::{
    collections::HashSet,
    str::FromStr,
    time::{Duration, Instant},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use xlayer_e2e_test::operations;

const ITERATIONS: usize = 11;
const TX_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(10);
const WEB_SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

/// Flashblock smoke test to verify pending tags on all flashblock supported RPCs.
#[tokio::test]
async fn fb_smoke_test() {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let sender_address = operations::DEFAULT_RICH_ADDRESS;
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    // Deploy contracts and get ERC20 address
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");
    println!("ERC20 contract at: {:#x}", contracts.erc20);

    // eth_getBlockTransactionCountByNumber
    let fb_block_transaction_count = operations::eth_get_block_transaction_count_by_number_or_hash(
        &fb_client,
        operations::BlockId::Pending,
    )
    .await
    .expect("Pending eth_getBlockTransactionCountByNumber failed");
    assert_ne!(
        fb_block_transaction_count, 0,
        "eth_getBlockTransactionCountByNumber with pending tag should return non-zero"
    );

    let tx_hash = operations::native_balance_transfer(
        operations::DEFAULT_L2_NETWORK_URL_FB,
        U256::from(operations::GWEI),
        test_address,
    )
    .await
    .expect("Failed to send tx");

    // eth_getTransactionByHash
    let tx = operations::eth_get_transaction_by_hash(&fb_client, &tx_hash)
        .await
        .expect("Pending eth_getTransactionByHash failed");
    assert!(!tx.is_null(), "Transaction should not be empty");

    // eth_getTransactionReceipt
    let receipt = operations::eth_get_transaction_receipt(&fb_client, &tx_hash)
        .await
        .expect("Pending eth_getTransactionReceipt failed");
    assert!(!receipt.is_null(), "Receipt should not be empty");
    assert_eq!(
        receipt["transactionIndex"], tx["transactionIndex"],
        "Transaction index not identical"
    );

    // eth_getRawTransactionByHash
    let raw_tx = operations::eth_get_raw_transaction_by_hash(&fb_client, &tx_hash)
        .await
        .expect("Pending eth_getRawTransactionByHash failed");
    assert!(!raw_tx.is_null(), "Raw transaction should not be empty");

    // eth_getInternalTransactions
    // let internal_transactions = operations::eth_get_internal_transactions(&fb_client, &tx_hash)
    //     .await
    //     .expect("Pending eth_getInternalTransactions failed");
    // assert!(!internal_transactions.is_null(), "Internal transactions should not be empty");

    // eth_getBalance
    let balance =
        operations::get_balance(&fb_client, sender_address, Some(operations::BlockId::Pending))
            .await
            .expect("Pending eth_getBalance failed");
    assert_ne!(balance, U256::ZERO, "Balance should not be zero");

    // eth_getTransactionCount
    let transaction_count = operations::eth_get_transaction_count(
        &fb_client,
        sender_address,
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Pending eth_getTransactionCount failed");
    assert_ne!(transaction_count, 0, "Transaction count should not be zero");

    // eth_getCode
    let code = operations::eth_get_code(
        &fb_client,
        contracts.erc20.to_string().as_str(),
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Pending eth_getCode failed");
    assert_ne!(code, "", "Code should not be empty");
    assert_ne!(code, "0x", "Code should not be empty");

    // eth_getStorageAt
    let storage = operations::eth_get_storage_at(
        &fb_client,
        contracts.erc20.to_string().as_str(),
        "0x2",
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Pending eth_getStorageAt failed");
    assert_ne!(storage, "", "Storage should not be empty");
    assert_ne!(storage, "0x", "Storage should not be empty");

    // eth_call
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

    let call = operations::eth_call(
        &fb_client,
        Some(call_args.clone()),
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Pending eth_call failed");
    assert_ne!(call, "", "Call should not be empty");
    assert_ne!(call, "0x", "Call should not be empty");

    // eth_estimateGas
    let transfer_args = serde_json::json!({
        "from":  sender_address,
        "to":    test_address,
        "value": format!("0x{:x}", operations::GWEI).as_str(),
    });
    let estimate_gas = operations::estimate_gas(
        &fb_client,
        Some(transfer_args.clone()),
        Some(operations::BlockId::Pending),
    )
    .await
    .expect("Pending eth_estimateGas failed");
    assert_eq!(estimate_gas, 21_000, "Estimate gas for native balance transfer should be 21_000");

    // eth_getBlockByNumber
    let fb_block = operations::eth_get_block_by_number_or_hash(
        &fb_client,
        operations::BlockId::Pending,
        false,
    )
    .await
    .expect("Pending eth_getBlockByNumber failed");
    assert!(!fb_block.is_null(), "Block should not be empty");

    // eth_getBlockTransactionCountByNumber
    let fb_block_transaction_count = operations::eth_get_block_transaction_count_by_number_or_hash(
        &fb_client,
        operations::BlockId::Pending,
    )
    .await
    .expect("Pending eth_getBlockTransactionCountByNumber failed");
    assert!(
        fb_block_transaction_count >= 1,
        "Block transaction count should be at least 1, got {}",
        fb_block_transaction_count
    );

    // eth_getBlockInternalTransactions
    // let _ =
    //     operations::eth_get_block_internal_transactions(&fb_client, operations::BlockId::Pending)
    //         .await
    //         .expect("Pending eth_getBlockInternalTransactions failed");

    // eth_getBlockReceipts
    let _ = operations::eth_get_block_receipts(&fb_client, operations::BlockId::Pending)
        .await
        .expect("Pending eth_getBlockReceipts failed");
}

/// Flashblock native balance transfer tx confirmation benchmark between a flashblock
/// node and a non-flashblock node.
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
        )
        .await
        .unwrap();
        println!("Sent tx: {}", signed_tx);

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

        println!("Iteration {}", i);
        println!("Flashblocks native tx transfer confirmation took: {}ms", fb_duration);
        println!("Non-flashblocks native tx transfer confirmation took: {}ms", non_fb_duration);
    }

    let avg_fb_duration = total_fb_duration / ITERATIONS as u128;
    let avg_non_fb_duration = total_non_fb_duration / ITERATIONS as u128;

    // Log out metrics
    println!("Avg flashblocks native tx transfer confirmation took: {}ms", avg_fb_duration);
    println!("Avg non-flashblocks native tx transfer confirmation took: {}ms", avg_non_fb_duration);
}

/// Flashblock erc20 transfer tx confirmation benchmark between a flashblock node
/// and a non-flashblock node.
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
        println!("Sent erc20 tx: {}", signed_tx);

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

        println!("Iteration {}", i);
        println!("Flashblocks erc20 tx transfer confirmation took: {}ms", fb_duration);
        println!("Non-flashblocks erc20 tx transfer confirmation took: {}ms", non_fb_duration);
    }

    let avg_fb_duration = total_fb_duration / ITERATIONS as u128;
    let avg_non_fb_duration = total_non_fb_duration / ITERATIONS as u128;

    // Log out metrics
    println!("Avg flashblocks erc20 tx transfer confirmation took: {}ms", avg_fb_duration);
    println!("Avg non-flashblocks erc20 tx transfer confirmation took: {}ms", avg_non_fb_duration);
}

/// Flashblock RPC comparison test compares the supported flashblocks RPC APIs with
/// a flashblock node and a non-flashblock node to ensure output is identical.
#[rstest::rstest]
#[case::stateless_api("StatelessApi")]
#[case::state_api("StateApi")]
#[tokio::test]
async fn fb_rpc_comparison_test(#[case] test_name: &str) {
    let fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_FB);
    let non_fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_NO_FB);
    let sender_address = operations::DEFAULT_RICH_ADDRESS;
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;

    let latest_block_number = operations::eth_block_number(&non_fb_client)
        .await
        .expect("Failed to get latest block number");
    let mut test_blocks = Vec::new();
    for i in 0..10 {
        test_blocks.push(operations::BlockId::Number(latest_block_number - i));
    }

    // Deploy contracts and get ERC20 address
    let contracts = operations::try_deploy_contracts().await.expect("Failed to deploy contracts");
    println!("ERC20 contract at: {:#x}", contracts.erc20);

    match test_name {
        "StatelessApi" => {
            // eth_getBlockByNumber
            for block_id in test_blocks.clone() {
                let fb_block = operations::eth_get_block_by_number_or_hash(
                    &fb_client,
                    block_id.clone(),
                    false,
                )
                .await
                .expect("Failed to get block from fb client");
                let non_fb_block = operations::eth_get_block_by_number_or_hash(
                    &non_fb_client,
                    block_id.clone(),
                    false,
                )
                .await
                .expect("Failed to get block from non-fb client");
                assert_eq!(fb_block, non_fb_block, "eth_getBlockByNumber not identical");
            }

            // eth_getBlockByHash
            for block_id in test_blocks.clone() {
                let block =
                    operations::eth_get_block_by_number_or_hash(&fb_client, block_id, false)
                        .await
                        .expect("Failed to get block from fb client");
                let block_hash = operations::BlockId::Hash(
                    block["hash"].as_str().expect("Block hash should not be empty").to_string(),
                );
                let fb_block = operations::eth_get_block_by_number_or_hash(
                    &fb_client,
                    block_hash.clone(),
                    false,
                )
                .await
                .expect("Failed to get block from fb client");
                let non_fb_block = operations::eth_get_block_by_number_or_hash(
                    &non_fb_client,
                    block_hash.clone(),
                    false,
                )
                .await
                .expect("Failed to get block from non-fb client");
                assert_eq!(fb_block, non_fb_block, "eth_getBlockByHash not identical");
            }

            // Setup batch ERC20 token transfers
            let num_transactions = 5;
            let (tx_hashes, block_num, _) = operations::transfer_erc20_token_batch(
                operations::DEFAULT_L2_NETWORK_URL_FB,
                contracts.erc20,
                U256::from(operations::GWEI),
                test_address,
                num_transactions as usize,
            )
            .await
            .expect("Failed to transfer batch ERC20 tokens");

            // Wait for block to be available on both nodes
            operations::wait_for_block_on_both_nodes(
                &fb_client,
                &non_fb_client,
                block_num,
                Duration::from_secs(10),
            )
            .await
            .expect("Failed to wait for block on both nodes");

            // Get block hashes from each node (they may differ)
            let fb_block = operations::eth_get_block_by_number_or_hash(
                &fb_client,
                operations::BlockId::Number(block_num),
                false,
            )
            .await
            .expect("Failed to get block from fb client");
            let fb_block_hash =
                fb_block["hash"].as_str().expect("Block hash should not be empty").to_string();

            let non_fb_block = operations::eth_get_block_by_number_or_hash(
                &non_fb_client,
                operations::BlockId::Number(block_num),
                false,
            )
            .await
            .expect("Failed to get block from non-fb client");
            let non_fb_block_hash =
                non_fb_block["hash"].as_str().expect("Block hash should not be empty").to_string();

            // eth_getBlockTransactionCountByNumber - compare between nodes
            let fb_block_transaction_count =
                operations::eth_get_block_transaction_count_by_number_or_hash(
                    &fb_client,
                    operations::BlockId::Number(block_num),
                )
                .await
                .expect("Failed to get block transaction count from fb client");
            let non_fb_block_transaction_count =
                operations::eth_get_block_transaction_count_by_number_or_hash(
                    &non_fb_client,
                    operations::BlockId::Number(block_num),
                )
                .await
                .expect("Failed to get block transaction count from non-fb client");
            assert_eq!(
                fb_block_transaction_count, non_fb_block_transaction_count,
                "eth_getBlockTransactionCountByNumber not identical between nodes"
            );

            // eth_getBlockTransactionCountByHash
            let fb_block_transaction_count_by_hash =
                operations::eth_get_block_transaction_count_by_number_or_hash(
                    &fb_client,
                    operations::BlockId::Hash(fb_block_hash.clone()),
                )
                .await
                .expect("Failed to get block transaction count by hash from fb client");
            let non_fb_block_transaction_count_by_hash =
                operations::eth_get_block_transaction_count_by_number_or_hash(
                    &non_fb_client,
                    operations::BlockId::Hash(non_fb_block_hash.clone()),
                )
                .await
                .expect("Failed to get block transaction count by hash from non-fb client");
            assert_eq!(
                fb_block_transaction_count, fb_block_transaction_count_by_hash,
                "FB node: transaction count by hash should match by number"
            );
            assert_eq!(
                non_fb_block_transaction_count, non_fb_block_transaction_count_by_hash,
                "Non-FB node: transaction count by hash should match by number"
            );
            assert_eq!(
                fb_block_transaction_count_by_hash, non_fb_block_transaction_count_by_hash,
                "eth_getBlockTransactionCountByHash not identical"
            );

            // // eth_getBlockInternalTransactions
            // let fb_block_internal_transactions = operations::eth_get_block_internal_transactions(
            //     &fb_client,
            //     operations::BlockId::Number(block_num),
            // )
            // .await
            // .expect("Failed to get block internal transactions from fb client");
            // let non_fb_block_internal_transactions =
            //     operations::eth_get_block_internal_transactions(
            //         &non_fb_client,
            //         operations::BlockId::Number(block_num),
            //     )
            //     .await
            //     .expect("Failed to get block internal transactions from non-fb client");
            // assert_eq!(
            //     fb_block_internal_transactions, non_fb_block_internal_transactions,
            //     "eth_getBlockInternalTransactions not identical"
            // );

            // eth_getTransactionByHash
            for tx_hash in tx_hashes.clone() {
                let fb_transaction = operations::eth_get_transaction_by_hash(&fb_client, &tx_hash)
                    .await
                    .expect("Failed to get transaction from fb client");
                let non_fb_transaction =
                    operations::eth_get_transaction_by_hash(&non_fb_client, &tx_hash)
                        .await
                        .expect("Failed to get transaction from non-fb client");
                assert_eq!(
                    fb_transaction, non_fb_transaction,
                    "eth_getTransactionByHash not identical"
                );
            }

            // eth_getRawTransactionByHash
            for tx_hash in tx_hashes.clone() {
                let fb_raw_transaction =
                    operations::eth_get_raw_transaction_by_hash(&fb_client, &tx_hash)
                        .await
                        .expect("Failed to get raw transaction from fb client");
                let non_fb_raw_transaction =
                    operations::eth_get_raw_transaction_by_hash(&non_fb_client, &tx_hash)
                        .await
                        .expect("Failed to get raw transaction from non-fb client");
                assert_eq!(
                    fb_raw_transaction, non_fb_raw_transaction,
                    "eth_getRawTransactionByHash not identical"
                );
            }

            // eth_getTransactionReceipt
            for tx_hash in tx_hashes.clone() {
                let fb_transaction_receipt =
                    operations::eth_get_transaction_receipt(&fb_client, &tx_hash)
                        .await
                        .expect("Failed to get transaction receipt from fb client");
                let non_fb_transaction_receipt =
                    operations::eth_get_transaction_receipt(&non_fb_client, &tx_hash)
                        .await
                        .expect("Failed to get transaction receipt from non-fb client");
                assert_eq!(
                    fb_transaction_receipt, non_fb_transaction_receipt,
                    "eth_getTransactionReceipt not identical"
                );
            }

            // eth_getInternalTransactions
            // for tx_hash in tx_hashes.clone() {
            //     let fb_internal_transactions =
            //         operations::eth_get_internal_transactions(&fb_client, &tx_hash)
            //             .await
            //             .expect("Failed to get internal transactions from fb client");
            //     let non_fb_internal_transactions =
            //         operations::eth_get_internal_transactions(&non_fb_client, &tx_hash)
            //             .await
            //             .expect("Failed to get internal transactions from non-fb client");
            //     assert_eq!(
            //         fb_internal_transactions, non_fb_internal_transactions,
            //         "eth_getInternalTransactions not identical"
            //     );
            // }

            // eth_getTransactionByBlockNumberAndIndex
            for tx_hash in tx_hashes.clone() {
                let receipt = operations::eth_get_transaction_receipt(&fb_client, &tx_hash)
                    .await
                    .expect("Failed to get transaction receipt from fb client");

                let tx_index_str = receipt["transactionIndex"]
                    .as_str()
                    .expect("Transaction index should not be empty");
                let fb_transaction =
                    operations::eth_get_transaction_by_block_number_or_hash_and_index(
                        &fb_client,
                        operations::BlockId::Number(block_num),
                        tx_index_str,
                    )
                    .await
                    .expect("Failed to get transaction from fb client");
                let non_fb_transaction =
                    operations::eth_get_transaction_by_block_number_or_hash_and_index(
                        &non_fb_client,
                        operations::BlockId::Number(block_num),
                        tx_index_str,
                    )
                    .await
                    .expect("Failed to get transaction from non-fb client");
                assert_eq!(
                    fb_transaction, non_fb_transaction,
                    "eth_getTransactionByBlockNumberAndIndex not identical"
                );
            }

            // eth_getBlockReceipts
            let fb_block_receipts = operations::eth_get_block_receipts(
                &fb_client,
                operations::BlockId::Number(block_num),
            )
            .await
            .expect("Failed to get block receipts from fb client");
            let non_fb_block_receipts = operations::eth_get_block_receipts(
                &non_fb_client,
                operations::BlockId::Number(block_num),
            )
            .await
            .expect("Failed to get block receipts from non-fb client");
            assert_eq!(
                fb_block_receipts, non_fb_block_receipts,
                "eth_getBlockReceipts not identical"
            );
        }
        "StateApi" => {
            // Setup batch ERC20 token transfers
            let num_transactions = 5;
            let (_, block_num, _) = operations::transfer_erc20_token_batch(
                operations::DEFAULT_L2_NETWORK_URL_FB,
                contracts.erc20,
                U256::from(operations::GWEI),
                test_address,
                num_transactions as usize,
            )
            .await
            .expect("Failed to transfer batch ERC20 tokens");

            // Wait for block to be available on both nodes
            operations::wait_for_block_on_both_nodes(
                &fb_client,
                &non_fb_client,
                block_num,
                Duration::from_secs(10),
            )
            .await
            .expect("Failed to wait for block on both nodes");

            // Get block hashes from each node (they may differ)
            let fb_block = operations::eth_get_block_by_number_or_hash(
                &fb_client,
                operations::BlockId::Number(block_num),
                false,
            )
            .await
            .expect("Failed to get block from fb client");
            let fb_block_hash =
                fb_block["hash"].as_str().expect("Block hash should not be empty").to_string();

            let non_fb_block = operations::eth_get_block_by_number_or_hash(
                &non_fb_client,
                operations::BlockId::Number(block_num),
                false,
            )
            .await
            .expect("Failed to get block from non-fb client");
            let non_fb_block_hash =
                non_fb_block["hash"].as_str().expect("Block hash should not be empty").to_string();

            // eth_call
            sol! {
                function balanceOf(address account) external view returns (uint256);
            }
            let call = balanceOfCall {
                account: Address::from_str(test_address).expect("Invalid address"),
            };
            let calldata = call.abi_encode();

            let call_args = serde_json::json!({
                "from": test_address,
                "to": contracts.erc20,
                "gas": "0x100000",
                "data": format!("0x{}", hex::encode(&calldata)),
            });

            // Test block number
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

            // Test block hash
            let fb_call_by_hash = operations::eth_call(
                &fb_client,
                Some(call_args.clone()),
                Some(operations::BlockId::Hash(fb_block_hash.clone())),
            )
            .await
            .expect("Failed to call from fb client by hash");
            let non_fb_call_by_hash = operations::eth_call(
                &non_fb_client,
                Some(call_args.clone()),
                Some(operations::BlockId::Hash(non_fb_block_hash.clone())),
            )
            .await
            .expect("Failed to call from non-fb client by hash");
            assert_eq!(
                fb_call, fb_call_by_hash,
                "FB node: eth_call by hash should match by number"
            );
            assert_eq!(
                non_fb_call, non_fb_call_by_hash,
                "Non-FB node: eth_call by hash should match by number"
            );
            assert_eq!(
                fb_call_by_hash, non_fb_call_by_hash,
                "eth_call with block hash not identical"
            );

            // eth_getBalance
            // Test block number
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

            // Test block hash
            let fb_balance_by_hash = operations::get_balance(
                &fb_client,
                sender_address,
                Some(operations::BlockId::Hash(fb_block_hash.clone())),
            )
            .await
            .expect("Failed to get balance from fb client by hash");
            let non_fb_balance_by_hash = operations::get_balance(
                &non_fb_client,
                sender_address,
                Some(operations::BlockId::Hash(non_fb_block_hash.clone())),
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

            // eth_getTransactionCount
            // Test block number
            let fb_transaction_count = operations::eth_get_transaction_count(
                &fb_client,
                sender_address,
                Some(operations::BlockId::Number(block_num)),
            )
            .await
            .expect("Failed to get transaction count from fb client");
            let non_fb_transaction_count = operations::eth_get_transaction_count(
                &non_fb_client,
                sender_address,
                Some(operations::BlockId::Number(block_num)),
            )
            .await
            .expect("Failed to get transaction count from non-fb client");
            assert_eq!(
                fb_transaction_count, non_fb_transaction_count,
                "eth_getTransactionCount not identical"
            );

            // Test block hash
            let fb_transaction_count_by_hash = operations::eth_get_transaction_count(
                &fb_client,
                sender_address,
                Some(operations::BlockId::Hash(fb_block_hash.clone())),
            )
            .await
            .expect("Failed to get transaction count from fb client by hash");
            let non_fb_transaction_count_by_hash = operations::eth_get_transaction_count(
                &non_fb_client,
                sender_address,
                Some(operations::BlockId::Hash(non_fb_block_hash.clone())),
            )
            .await
            .expect("Failed to get transaction count from non-fb client by hash");
            assert_eq!(
                fb_transaction_count, fb_transaction_count_by_hash,
                "FB node: eth_getTransactionCount by hash should match by number"
            );
            assert_eq!(
                non_fb_transaction_count, non_fb_transaction_count_by_hash,
                "Non-FB node: eth_getTransactionCount by hash should match by number"
            );
            assert_eq!(
                fb_transaction_count_by_hash, non_fb_transaction_count_by_hash,
                "eth_getTransactionCount with block hash not identical"
            );

            // eth_getCode
            // Test block number
            let fb_code = operations::eth_get_code(
                &fb_client,
                contracts.erc20.to_string().as_str(),
                Some(operations::BlockId::Number(block_num)),
            )
            .await
            .expect("Failed to get code from fb client");
            let non_fb_code = operations::eth_get_code(
                &non_fb_client,
                contracts.erc20.to_string().as_str(),
                Some(operations::BlockId::Number(block_num)),
            )
            .await
            .expect("Failed to get code from non-fb client");
            assert_eq!(fb_code, non_fb_code, "eth_getCode with block number not identical");

            // Test block hash
            let fb_code_by_hash = operations::eth_get_code(
                &fb_client,
                contracts.erc20.to_string().as_str(),
                Some(operations::BlockId::Hash(fb_block_hash.clone())),
            )
            .await
            .expect("Failed to get code from fb client by hash");
            let non_fb_code_by_hash = operations::eth_get_code(
                &non_fb_client,
                contracts.erc20.to_string().as_str(),
                Some(operations::BlockId::Hash(non_fb_block_hash.clone())),
            )
            .await
            .expect("Failed to get code from non-fb client by hash");
            assert_eq!(
                fb_code, fb_code_by_hash,
                "FB node: eth_getCode by hash should match by number"
            );
            assert_eq!(
                non_fb_code, non_fb_code_by_hash,
                "Non-FB node: eth_getCode by hash should match by number"
            );
            assert_eq!(
                fb_code_by_hash, non_fb_code_by_hash,
                "eth_getCode with block hash not identical"
            );

            // eth_getStorageAt
            // Test block number
            let fb_storage = operations::eth_get_storage_at(
                &fb_client,
                contracts.erc20.to_string().as_str(),
                "0x2",
                Some(operations::BlockId::Number(block_num)),
            )
            .await
            .expect("Failed to get storage from fb client");
            let non_fb_storage = operations::eth_get_storage_at(
                &non_fb_client,
                contracts.erc20.to_string().as_str(),
                "0x2",
                Some(operations::BlockId::Number(block_num)),
            )
            .await
            .expect("Failed to get storage from non-fb client");
            assert_eq!(
                fb_storage, non_fb_storage,
                "eth_getStorageAt with block number not identical"
            );

            // Test block hash
            let fb_storage_by_hash = operations::eth_get_storage_at(
                &fb_client,
                contracts.erc20.to_string().as_str(),
                "0x2",
                Some(operations::BlockId::Hash(fb_block_hash.clone())),
            )
            .await
            .expect("Failed to get storage from fb client by hash");
            let non_fb_storage_by_hash = operations::eth_get_storage_at(
                &non_fb_client,
                contracts.erc20.to_string().as_str(),
                "0x2",
                Some(operations::BlockId::Hash(non_fb_block_hash.clone())),
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

            // eth_flashblocksEnabled
            let fb_flashblocks_enabled = operations::eth_flashblocks_enabled(&fb_client)
                .await
                .expect("Failed to get flashblocks enabled from fb client");

            assert_eq!(fb_flashblocks_enabled, true, "Flashblocks should be enabled");

            let non_fb_flashblocks_enabled = operations::eth_flashblocks_enabled(&non_fb_client)
                .await
                .expect("Failed to get flashblocks enabled from fb client");

            assert_eq!(non_fb_flashblocks_enabled, false, "Flashblocks should be disabled");
        }
        _ => panic!("Unknown test case: {}", test_name),
    }
}

#[tokio::test]
async fn fb_subscription_test() -> Result<()> {
    let ws_url = operations::manager::DEFAULT_FLASHBLOCKS_WS_URL;
    let test_address = operations::DEFAULT_L2_NEW_ACC1_ADDRESS;
    let non_fb_client = operations::create_test_client(operations::DEFAULT_L2_NETWORK_URL_NO_FB);

    let current_block_number = operations::eth_block_number(&non_fb_client)
        .await
        .expect("Failed to get current block number");
    println!("Current block number: {}", current_block_number);

    let num_txs: usize = 5;

    println!("Connecting to flashblocks WebSocket at {ws_url}...");
    let (ws_stream, response) = connect_async(ws_url).await?;
    println!("Connected: {:?}", response.status());
    let (_, mut read) = ws_stream.split();

    // Guarantee cleanup on scope exit
    defer! {
        println!("Closing WebSocket connection");
    };

    let mut remaining: HashSet<String> = HashSet::new();
    for i in 0..num_txs {
        let tx_hash = operations::native_balance_transfer(
            operations::DEFAULT_L2_NETWORK_URL_FB,
            U256::from(operations::GWEI),
            test_address,
        )
        .await?;
        println!("Sent tx {}: {}", i + 1, tx_hash);
        remaining.insert(tx_hash);
    }
    let total = remaining.len();
    println!(
        "Waiting for {} txs to appear in flashblocks (timeout: {:?})...",
        total, WEB_SOCKET_TIMEOUT
    );

    // Read flashblocks until all txs are found or timeout
    let result = tokio::time::timeout(WEB_SOCKET_TIMEOUT, async {
        while !remaining.is_empty() {
            match read.next().await {
                Some(Ok(Message::Text(msg))) => {
                    let Ok(notification) = serde_json::from_str::<serde_json::Value>(&msg) else {
                        continue;
                    };

                    let block_num = notification["metadata"]["block_number"]
                        .as_u64()
                        .expect("Failed to get block number");

                    assert!(
                        block_num >= current_block_number,
                        "Flashblock block number {} should be >= current block {}",
                        block_num,
                        current_block_number
                    );

                    if let Some(txs) = notification["diff"]["transactions"].as_array() {
                        for tx in txs {
                            let Some(tx_rlp_hex) = tx.as_str() else { continue };
                            let raw = tx_rlp_hex.trim_start_matches("0x");
                            let Ok(bytes) = hex::decode(raw) else {
                                eprintln!("Failed to hex-decode RLP: {}", tx_rlp_hex);
                                continue;
                            };

                            let hash = keccak256(&bytes);
                            let hash_str = format!("0x{}", hex::encode(hash));

                            if remaining.remove(&hash_str) {
                                let found = total - remaining.len();
                                println!(
                                    "Found tx {}/{} in block {}: {}",
                                    found, total, block_num, hash_str
                                );
                                if remaining.is_empty() {
                                    break;
                                }
                            }
                        }
                    }
                }
                Some(Ok(_)) => {} // ignore non-text
                Some(Err(e)) => {
                    eprintln!("WebSocket error: {}", e);
                    break;
                }
                None => {
                    eprintln!("WebSocket closed");
                    break;
                }
            }
        }
    })
    .await;

    if result.is_err() {
        eprintln!("Timeout: Stopped waiting after {:?}", WEB_SOCKET_TIMEOUT);
    }

    if !remaining.is_empty() {
        eprintln!("\nMissing txs in flashblocks:");
        for tx in &remaining {
            eprintln!("  - {}", tx);
        }
    }

    assert!(
        remaining.is_empty(),
        "Expected all {} txs to appear in flashblocks, but {} were missing",
        total,
        remaining.len()
    );

    println!("All {} transactions found in flashblocks", total);

    Ok(())
}
