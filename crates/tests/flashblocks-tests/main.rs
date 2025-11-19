//! Functional tests for flashblocks e2e tests
//!
//! Run all tests with: `cargo test -p xlayer-e2e-test --test flashblocks_tests -- --nocapture --test-threads=1`
//! or run a specific test with: `cargo test -p xlayer-e2e-test --test flashblocks_tests -- <test_case_name> -- --nocapture`
//! --test-threads=1`
//!

use alloy_primitives::{hex, Address, U256};
use alloy_sol_types::{sol, SolCall};
use eyre::Result;
use std::{
    str::FromStr,
    time::{Duration, Instant},
};
use xlayer_e2e_test::operations;

const ITERATIONS: usize = 11;
const TX_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(10);

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
    let internal_transactions = operations::eth_get_internal_transactions(&fb_client, &tx_hash)
        .await
        .expect("Pending eth_getInternalTransactions failed");
    assert!(!internal_transactions.is_null(), "Internal transactions should not be empty");

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
    assert_eq!(fb_block_transaction_count, 1, "Block transaction count should be 1");

    // eth_getBlockInternalTransactions
    let _ =
        operations::eth_get_block_internal_transactions(&fb_client, operations::BlockId::Pending)
            .await
            .expect("Pending eth_getBlockInternalTransactions failed");

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
            operations::DEFAULT_L2_BUILDER_URL,
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
            operations::DEFAULT_L2_BUILDER_URL,
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
            let (tx_hashes, block_num, block_hash) = operations::transfer_erc20_token_batch(
                operations::DEFAULT_L2_NETWORK_URL_FB,
                contracts.erc20,
                U256::from(operations::GWEI),
                test_address,
                num_transactions as usize,
            )
            .await
            .expect("Failed to transfer batch ERC20 tokens");

            // eth_getBlockTransactionCountByNumber
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
                fb_block_transaction_count, num_transactions,
                "eth_getBlockTransactionCountByNumber fb not identical"
            );
            assert_eq!(
                non_fb_block_transaction_count, num_transactions,
                "eth_getBlockTransactionCountByNumber non-fb not identical"
            );

            // eth_getBlockTransactionCountByHash
            let fb_block_transaction_count =
                operations::eth_get_block_transaction_count_by_number_or_hash(
                    &fb_client,
                    operations::BlockId::Hash(block_hash.clone()),
                )
                .await
                .expect("Failed to get block transaction count from fb client");
            let non_fb_block_transaction_count =
                operations::eth_get_block_transaction_count_by_number_or_hash(
                    &non_fb_client,
                    operations::BlockId::Hash(block_hash.clone()),
                )
                .await
                .expect("Failed to get block transaction count from non-fb client");
            assert_eq!(
                fb_block_transaction_count, num_transactions,
                "eth_getBlockTransactionCountByHash fb not identical"
            );
            assert_eq!(
                non_fb_block_transaction_count, num_transactions,
                "eth_getBlockTransactionCountByHash non-fb not identical"
            );

            // eth_getBlockInternalTransactions
            let fb_block_internal_transactions = operations::eth_get_block_internal_transactions(
                &fb_client,
                operations::BlockId::Number(block_num),
            )
            .await
            .expect("Failed to get block internal transactions from fb client");
            let non_fb_block_internal_transactions =
                operations::eth_get_block_internal_transactions(
                    &non_fb_client,
                    operations::BlockId::Number(block_num),
                )
                .await
                .expect("Failed to get block internal transactions from non-fb client");
            assert_eq!(
                fb_block_internal_transactions, non_fb_block_internal_transactions,
                "eth_getBlockInternalTransactions not identical"
            );

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
            for tx_hash in tx_hashes.clone() {
                let fb_internal_transactions =
                    operations::eth_get_internal_transactions(&fb_client, &tx_hash)
                        .await
                        .expect("Failed to get internal transactions from fb client");
                let non_fb_internal_transactions =
                    operations::eth_get_internal_transactions(&non_fb_client, &tx_hash)
                        .await
                        .expect("Failed to get internal transactions from non-fb client");
                assert_eq!(
                    fb_internal_transactions, non_fb_internal_transactions,
                    "eth_getInternalTransactions not identical"
                );
            }

            // eth_getTransactionByBlockNumberAndIndex
            for tx_hash in tx_hashes.clone() {
                let receipt = operations::eth_get_transaction_receipt(&fb_client, &tx_hash)
                    .await
                    .expect("Failed to get transaction receipt from fb client");

                let tx_index = receipt["transactionIndex"]
                    .as_u64()
                    .expect("Transaction index should not be empty");
                let fb_transaction =
                    operations::eth_get_transaction_by_block_number_or_hash_and_index(
                        &fb_client,
                        operations::BlockId::Number(block_num),
                        format!("0x{:x}", tx_index).as_str(),
                    )
                    .await
                    .expect("Failed to get transaction from fb client");
                let non_fb_transaction =
                    operations::eth_get_transaction_by_block_number_or_hash_and_index(
                        &non_fb_client,
                        operations::BlockId::Number(block_num),
                        format!("0x{:x}", tx_index).as_str(),
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
            let (_, block_num, block_hash) = operations::transfer_erc20_token_batch(
                operations::DEFAULT_L2_NETWORK_URL_FB,
                contracts.erc20,
                U256::from(operations::GWEI),
                test_address,
                num_transactions as usize,
            )
            .await
            .expect("Failed to transfer batch ERC20 tokens");

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
            let fb_call = operations::eth_call(
                &fb_client,
                Some(call_args.clone()),
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to call from fb client");
            let non_fb_call = operations::eth_call(
                &non_fb_client,
                Some(call_args.clone()),
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to call from non-fb client");
            assert_eq!(fb_call, non_fb_call, "eth_call with block hash not identical");

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
            let fb_balance = operations::get_balance(
                &fb_client,
                sender_address,
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to get balance from fb client");
            let non_fb_balance = operations::get_balance(
                &non_fb_client,
                sender_address,
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to get balance from non-fb client");
            assert_eq!(fb_balance, non_fb_balance, "eth_getBalance with block hash not identical");

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
            let fb_transaction_count = operations::eth_get_transaction_count(
                &fb_client,
                sender_address,
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to get transaction count from fb client");
            let non_fb_transaction_count = operations::eth_get_transaction_count(
                &non_fb_client,
                sender_address,
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to get transaction count from non-fb client");
            assert_eq!(
                fb_transaction_count, non_fb_transaction_count,
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
            let fb_code = operations::eth_get_code(
                &fb_client,
                contracts.erc20.to_string().as_str(),
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to get code from fb client");
            let non_fb_code = operations::eth_get_code(
                &non_fb_client,
                contracts.erc20.to_string().as_str(),
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to get code from non-fb client");
            assert_eq!(fb_code, non_fb_code, "eth_getCode with block hash not identical");

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
            let fb_storage = operations::eth_get_storage_at(
                &fb_client,
                contracts.erc20.to_string().as_str(),
                "0x2",
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to get storage from fb client");
            let non_fb_storage = operations::eth_get_storage_at(
                &non_fb_client,
                contracts.erc20.to_string().as_str(),
                "0x2",
                Some(operations::BlockId::Hash(block_hash.clone())),
            )
            .await
            .expect("Failed to get storage from non-fb client");
            assert_eq!(
                fb_storage, non_fb_storage,
                "eth_getStorageAt with block hash not identical"
            );
        }
        _ => panic!("Unknown test case: {}", test_name),
    }
}
