//! Functional tests for flashblocks e2e tests
//!
//! Run all tests with: `cargo test -p xlayer-e2e-test --test flashblocks_tests -- --nocapture --test-threads=1`
//! or run a specific test with: `cargo test -p xlayer-e2e-test --test flashblocks_tests -- <test_case_name> -- --nocapture`
//! --test-threads=1`
//!

use alloy_primitives::U256;
use eyre::Result;
use std::time::{Duration, Instant};
use xlayer_e2e_test::operations;

const ITERATIONS: usize = 11;
const TX_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn test_fb_benchmark_native_tx_confirmation() {
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

        // Run benchmark
        let start = Instant::now();
        let start_clone = start;
        let signed_tx_clone = signed_tx.clone();
        let fb_duration = tokio::time::timeout(TX_CONFIRMATION_TIMEOUT, async move {
            operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &signed_tx)
                .await?;
            <Result<u128>>::Ok(start.elapsed().as_millis())
        });

        let non_fb_duration = tokio::time::timeout(TX_CONFIRMATION_TIMEOUT, async move {
            operations::wait_for_tx_mined(
                operations::DEFAULT_L2_NETWORK_URL_NO_FB,
                &signed_tx_clone,
            )
            .await?;
            <Result<u128>>::Ok(start_clone.elapsed().as_millis())
        });

        let fb_duration = fb_duration.await.expect("timeout waiting for tx to be mined").unwrap();
        let non_fb_duration =
            non_fb_duration.await.expect("timeout waiting for tx to be mined").unwrap();
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

#[tokio::test]
async fn test_fb_benchmark_erc20_tx_confirmation() {
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

        // Run benchmark
        let start = Instant::now();
        let start_clone = start;
        let signed_tx_clone = signed_tx.clone();
        let fb_duration = tokio::time::timeout(TX_CONFIRMATION_TIMEOUT, async move {
            operations::wait_for_tx_mined(operations::DEFAULT_L2_NETWORK_URL_FB, &signed_tx)
                .await?;
            <Result<u128>>::Ok(start.elapsed().as_millis())
        });

        let non_fb_duration = tokio::time::timeout(TX_CONFIRMATION_TIMEOUT, async move {
            operations::wait_for_tx_mined(
                operations::DEFAULT_L2_NETWORK_URL_NO_FB,
                &signed_tx_clone,
            )
            .await?;
            <Result<u128>>::Ok(start_clone.elapsed().as_millis())
        });

        let fb_duration = fb_duration.await.expect("timeout waiting for tx to be mined").unwrap();
        let non_fb_duration =
            non_fb_duration.await.expect("timeout waiting for tx to be mined").unwrap();
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
