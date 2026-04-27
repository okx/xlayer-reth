//! Utility functions for e2e functional tests

use crate::operations::{
    contracts::*, eth_block_number, eth_call, eth_gas_price, eth_get_block_by_number_or_hash,
    eth_get_transaction_count, eth_get_transaction_receipt, get_balance, manager::*, BlockId,
    HttpClient, GWEI,
};
use eyre::{eyre, Result};
use serde_json;
use std::{collections::HashMap, str::FromStr, sync::OnceLock};
use tokio::time::{sleep, Duration};

use alloy_eips::eip2718::Encodable2718;
use alloy_network::{EthereumWallet, TransactionBuilder};
use alloy_primitives::{hex, Address, Bytes, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::TransactionRequest;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolCall, SolValue};

static DEPLOYED_CONTRACTS: OnceLock<DeployedContracts> = OnceLock::new();

/// Holds addresses of all deployed test contracts
#[derive(Debug, Clone)]
pub struct DeployedContracts {
    /// Address of Contract A
    pub contract_a: Address,
    /// Address of Contract B
    pub contract_b: Address,
    /// Address of Contract C
    pub contract_c: Address,
    /// Address of the contract factory (CREATE opcode)
    pub factory: Address,
    /// Address of the ERC20 token contract
    pub erc20: Address,
    /// Address of the CREATE2 factory contract
    pub create2_factory: Address,
    /// Address of the precompile caller contract
    pub precompile_caller: Address,
}

/// Create a new HTTP client for the given endpoint URL
pub fn create_test_client(endpoint_url: &str) -> HttpClient {
    HttpClient::builder().build(endpoint_url).unwrap()
}

/// Transfer native balance from the rich address to a target address.
/// Gas limit defaults to 21_000 if `None`. Use a higher value for transfers to
/// precompile addresses that trigger execution beyond the intrinsic cost.
pub async fn native_balance_transfer(
    endpoint_url: &str,
    amount: U256,
    to_address: &str,
    gas_limit: Option<u64>,
    wait_for_confirmation: bool,
) -> Result<String> {
    let signer = PrivateKeySigner::from_str(DEFAULT_RICH_PRIVATE_KEY.trim_start_matches("0x"))?;
    let wallet = EthereumWallet::from(signer.clone());
    let provider = ProviderBuilder::new().wallet(wallet).connect_http(endpoint_url.parse()?);

    let from = signer.address();
    let to = Address::from_str(to_address)?;
    let nonce = provider.get_transaction_count(from).pending().await?;
    let gas_price = provider.get_gas_price().await?;
    let tx = TransactionRequest::default()
        .with_from(from)
        .with_to(to)
        .with_value(amount)
        .with_nonce(nonce)
        .with_chain_id(DEFAULT_L2_CHAIN_ID)
        .with_gas_limit(gas_limit.unwrap_or(21_000))
        .with_gas_price(gas_price);
    let pending_tx = provider.send_transaction(tx).await?;

    let tx_hash = *pending_tx.tx_hash();

    if wait_for_confirmation {
        println!("Tx sent: {tx_hash:#x}, waiting for tx receipt confirmation.");
        wait_for_tx_mined(endpoint_url, &format!("{tx_hash:#x}")).await?;
        println!("Transaction {tx_hash:#x} confirmed successfully");
    } else {
        println!("Tx sent: {tx_hash:#x}");
    }

    Ok(format!("{tx_hash:#x}"))
}

/// Sign a native transfer transaction and return its raw encoded bytes as a hex string,
/// without broadcasting it to the network.
pub async fn sign_raw_transaction(
    endpoint_url: &str,
    amount: U256,
    to_address: &str,
) -> Result<String> {
    let signer = PrivateKeySigner::from_str(DEFAULT_RICH_PRIVATE_KEY.trim_start_matches("0x"))?;
    let wallet = EthereumWallet::from(signer.clone());
    let provider =
        ProviderBuilder::new().wallet(wallet.clone()).connect_http(endpoint_url.parse()?);

    let from = signer.address();
    let to = Address::from_str(to_address)?;
    let nonce = provider.get_transaction_count(from).pending().await?;
    let gas_price = provider.get_gas_price().await?;
    let tx = TransactionRequest::default()
        .with_from(from)
        .with_to(to)
        .with_value(amount)
        .with_nonce(nonce)
        .with_chain_id(DEFAULT_L2_CHAIN_ID)
        .with_gas_limit(21_000)
        .with_gas_price(gas_price);

    let envelope = tx.build(&wallet).await?;
    Ok(format!("0x{}", hex::encode(envelope.encoded_2718())))
}

/// Funds an address with native tokens and waits for the balance to be available
pub async fn fund_address_and_wait_for_balance(
    client: &HttpClient,
    endpoint_url: &str,
    to_address: &str,
    funding_amount: U256,
) -> Result<()> {
    let funding_tx_hash =
        native_balance_transfer(endpoint_url, funding_amount, to_address, None, true).await?;

    let receipt = eth_get_transaction_receipt(client, &funding_tx_hash).await?;
    let funding_block_num = receipt["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| eyre::eyre!("Failed to parse block number from receipt"))?;

    wait_for_blocks(client, funding_block_num).await;

    let balance = get_balance(client, to_address, None).await?;
    if balance < funding_amount {
        return Err(eyre::eyre!(
            "Balance {} is less than funded amount {}",
            balance,
            funding_amount
        ));
    }

    println!("Funded address {to_address} with {funding_amount} wei, balance verified");
    Ok(())
}

/// Deploys smart contract using rich address
pub async fn deploy_contract(
    endpoint_url: &str,
    bytecode_hex: &str,
    constructor_args: Option<Bytes>,
) -> Result<Address> {
    let signer = PrivateKeySigner::from_str(DEFAULT_RICH_PRIVATE_KEY.trim_start_matches("0x"))?;
    let wallet = EthereumWallet::from(signer.clone());
    let provider = ProviderBuilder::new().wallet(wallet).connect_http(endpoint_url.parse()?);

    let mut bytecode = hex::decode(bytecode_hex.trim_start_matches("0x"))?;
    if let Some(args) = constructor_args {
        bytecode.extend_from_slice(&args);
    }
    let data = Bytes::from(bytecode);

    let from = signer.address();
    let nonce = provider.get_transaction_count(from).pending().await?;
    let gas_price = provider.get_gas_price().await?;
    let tx = TransactionRequest::default()
        .with_from(from)
        .with_nonce(nonce)
        .with_chain_id(DEFAULT_L2_CHAIN_ID)
        .with_gas_limit(5_000_000)
        .with_gas_price(gas_price)
        .with_deploy_code(data);
    let pending = provider.send_transaction(tx).await?;

    let receipt = pending.get_receipt().await?;
    if !receipt.status() {
        return Err(eyre!("deployment failed"));
    }
    receipt
        .contract_address
        .ok_or(eyre!("Contract deployment failed, no contract address in receipt"))
}

/// Ensure all test contracts are deployed
pub async fn try_deploy_contracts() -> Result<&'static DeployedContracts> {
    // Return cached contracts if already deployed
    if let Some(contracts) = DEPLOYED_CONTRACTS.get() {
        println!("Contracts already deployed in this test run, returning cached addresses");
        return Ok(contracts);
    }

    println!("\n=== Deploying contracts ===");
    // Deploy ContractB (no constructor args)
    println!("Deploying ContractB...");
    let contract_b = deploy_contract(DEFAULT_L2_NETWORK_URL, CONTRACT_B_BYTECODE_STR, None).await?;
    println!("ContractB deployed at: {contract_b:#x}");

    // Deploy ContractA (requires ContractB address as constructor arg)
    println!("Deploying ContractA...");
    let contract_a_constructor_args = contract_b.abi_encode();
    let contract_a = deploy_contract(
        DEFAULT_L2_NETWORK_URL,
        CONTRACT_A_BYTECODE_STR,
        Some(Bytes::from(contract_a_constructor_args)),
    )
    .await?;
    println!("ContractA deployed at: {contract_a:#x}");

    // Deploy ContractFactory (no constructor args)
    println!("Deploying ContractFactory...");
    let factory =
        deploy_contract(DEFAULT_L2_NETWORK_URL, CONTRACT_FACTORY_BYTECODE_STR, None).await?;
    println!("ContractFactory deployed at: {factory:#x}");

    // Deploy ContractC (no constructor args)
    println!("Deploying ContractC...");
    let contract_c = deploy_contract(DEFAULT_L2_NETWORK_URL, CONTRACT_C_BYTECODE_STR, None).await?;
    println!("ContractC deployed at: {contract_c:#x}");

    // Deploy ERC20 (no constructor args)
    println!("Deploying ERC20...");
    let erc20 = deploy_contract(DEFAULT_L2_NETWORK_URL, ERC20_BYTECODE_STR, None).await?;
    println!("ERC20 deployed at: {erc20:#x}");

    // Deploy Create2Factory (no constructor args)
    println!("Deploying Create2Factory...");
    let create2_factory =
        deploy_contract(DEFAULT_L2_NETWORK_URL, CREATE2_FACTORY_BYTECODE_STR, None).await?;
    println!("Create2Factory deployed at: {create2_factory:#x}");

    // Deploy PrecompileCaller (no constructor args)
    println!("Deploying PrecompileCaller...");
    let precompile_caller =
        deploy_contract(DEFAULT_L2_NETWORK_URL, PRECOMPILE_CALLER_BYTECODE_STR, None).await?;
    println!("PrecompileCaller deployed at: {precompile_caller:#x}");

    // Store deployed contract addresses in global state
    let contracts = DeployedContracts {
        contract_a,
        contract_b,
        contract_c,
        factory,
        erc20,
        create2_factory,
        precompile_caller,
    };

    // Cache the deployed contracts (marks ContractsDeployed = true)
    DEPLOYED_CONTRACTS.set(contracts).map_err(|_| eyre!("Failed to cache deployed contracts"))?;

    println!("\n=== All contracts deployed successfully! ===");
    println!("ContractA: {contract_a:#x}");
    println!("ContractB: {contract_b:#x}");
    println!("ContractC: {contract_c:#x}");
    println!("Factory: {factory:#x}");
    println!("ERC20: {erc20:#x}");
    println!("Create2Factory: {create2_factory:#x}");
    println!("PrecompileCaller: {precompile_caller:#x}");
    println!("ContractsDeployed: true");
    Ok(DEPLOYED_CONTRACTS.get().unwrap())
}

/// Helper function to wait for blocks to be mined
pub async fn wait_for_blocks(client: &HttpClient, min_blocks: u64) -> u64 {
    let mut block_number: u64 = 0;
    for i in 0..30 {
        block_number = eth_block_number(client).await.unwrap();
        println!("Block number: {block_number}, attempt {i}");
        if block_number > min_blocks {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    block_number
}

/// Setup the test environment by waiting for blocks and returning a valid block hash and number
pub async fn setup_test_environment(client: &HttpClient) -> Result<(String, u64)> {
    // Wait for at least one block to be available
    let block_number = wait_for_blocks(client, 0).await;

    if block_number == 0 {
        return Err(eyre!("Block number should be greater than 0"));
    }

    // Get block data by number
    let block_data =
        eth_get_block_by_number_or_hash(client, BlockId::Number(block_number), true).await?;

    if block_data.is_null() {
        return Err(eyre!("Block data should not be null"));
    }

    // Extract block hash from the returned data
    let block_hash = if let Some(hash_value) = block_data.get("hash") {
        if let Some(hash_str) = hash_value.as_str() {
            if !hash_str.is_empty() {
                // println!("Extracted block hash: {}", hash_str);
                hash_str.to_string()
            } else {
                println!("Hash field is empty");
                return Err(eyre!("Block hash should not be empty"));
            }
        } else {
            println!("Hash field is not a string");
            return Err(eyre!("Block hash field is not a string"));
        }
    } else {
        println!("No hash field found in block data");
        return Err(eyre!("No hash field found in block data"));
    };

    if block_hash.is_empty()
        || block_hash == "0x0000000000000000000000000000000000000000000000000000000000000000"
    {
        return Err(eyre!("Block hash should not be empty or zero"));
    }

    Ok((block_hash, block_number))
}

/// Transfer ERC20 tokens from the rich address to a target address
pub async fn erc20_balance_transfer(
    endpoint_url: &str,
    amount: U256,
    gas_price: Option<u128>,
    to_address: &str,
    erc20_address: Address,
    nonce: Option<u64>,
) -> Result<String> {
    // Define the ERC20 transfer function
    sol! {
        function transfer(address to, uint256 amount) external returns (bool);
    }

    let signer = PrivateKeySigner::from_str(DEFAULT_RICH_PRIVATE_KEY.trim_start_matches("0x"))?;
    let wallet = EthereumWallet::from(signer.clone());
    let provider = ProviderBuilder::new().wallet(wallet).connect_http(endpoint_url.parse()?);

    let from = signer.address();
    let to = Address::from_str(to_address)?;
    let gas_price = if let Some(gp) = gas_price { gp } else { provider.get_gas_price().await? };
    let nonce = if let Some(n) = nonce {
        n
    } else {
        provider.get_transaction_count(from).pending().await?
    };
    let call = transferCall { to, amount };
    let calldata = call.abi_encode();
    let tx = TransactionRequest::default()
        .with_from(from)
        .with_to(erc20_address)
        .with_value(U256::ZERO) // ERC20 transfers don't send ETH
        .with_input(calldata)
        .with_nonce(nonce)
        .with_chain_id(DEFAULT_L2_CHAIN_ID)
        .with_gas_limit(60_000) // Fixed gas limit for ERC20 transfers
        .with_gas_price(gas_price);
    let pending_tx = provider.send_transaction(tx).await?;

    let tx_hash = *pending_tx.tx_hash();
    Ok(format!("{tx_hash:#x}"))
}

/// Sends a batch of ERC20 token transfers and waits for them to be mined
pub async fn transfer_erc20_token_batch(
    endpoint_url: &str,
    erc20_address: Address,
    amount: U256,
    to_address: &str,
    batch_size: usize,
) -> Result<(Vec<String>, u64, String)> {
    let mut tx_hashes: Vec<String> = Vec::new();

    let client = create_test_client(endpoint_url);
    let gas_price = eth_gas_price(&client).await?.to::<u128>();
    let start_nonce =
        eth_get_transaction_count(&client, DEFAULT_RICH_ADDRESS, Some(BlockId::Pending)).await?;

    // Send transactions with incrementing nonces (1 to batch_size-1)
    println!("Starting batch transfer of {batch_size} transactions");
    for i in 1..batch_size {
        let nonce = start_nonce + i as u64;
        let tx_hash = erc20_balance_transfer(
            endpoint_url,
            amount,
            Some(gas_price),
            to_address,
            erc20_address,
            Some(nonce),
        )
        .await?;
        tx_hashes.push(tx_hash);
        println!("Sent transaction {}/{}: {}", i, batch_size - 1, tx_hashes.last().unwrap());
    }

    // Send the last transaction with the starting nonce
    let tx_hash = erc20_balance_transfer(
        endpoint_url,
        amount,
        Some(gas_price),
        to_address,
        erc20_address,
        Some(start_nonce),
    )
    .await?;
    tx_hashes.push(tx_hash.clone());
    println!("Sent final transaction: {tx_hash}");

    // Wait for all transactions to be mined
    println!("Waiting for all transactions to be mined...");
    for (i, tx_hash) in tx_hashes.iter().enumerate() {
        wait_for_tx_mined(endpoint_url, tx_hash).await?;
        println!("Transaction {}/{} mined", i + 1, tx_hashes.len());
    }
    println!("All {} transactions have been mined successfully", tx_hashes.len());

    // Get receipt of the last transaction
    let last_tx_hash = tx_hashes.last().unwrap();
    let receipt = wait_for_tx_mined(endpoint_url, last_tx_hash).await?;

    let block_number = receipt["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| eyre!("Failed to parse block number from receipt"))?;

    let block_hash = receipt["blockHash"]
        .as_str()
        .ok_or_else(|| eyre!("Failed to get block hash from receipt"))?
        .to_string();

    Ok((tx_hashes, block_number, block_hash))
}

/// Shared setup for comparison tests that need transactions confirmed on both nodes.
/// Sends a batch of ERC20 transfers and waits for the block to be available on both nodes.
/// Returns (fb_client, non_fb_client, tx_hashes, block_num, fb_block_hash, non_fb_block_hash, contracts).
pub async fn comparison_test_setup() -> (
    jsonrpsee::http_client::HttpClient,
    jsonrpsee::http_client::HttpClient,
    Vec<String>,
    u64,
    String,
    String,
    &'static DeployedContracts,
) {
    let fb_client = create_test_client(DEFAULT_L2_NETWORK_URL_FB);
    let non_fb_client = create_test_client(DEFAULT_L2_NETWORK_URL_NO_FB);
    let test_address = DEFAULT_L2_NEW_ACC1_ADDRESS;

    let contracts = try_deploy_contracts().await.expect("Failed to deploy contracts");

    let (tx_hashes, block_num, _) = transfer_erc20_token_batch(
        DEFAULT_L2_NETWORK_URL_FB,
        contracts.erc20,
        U256::from(GWEI),
        test_address,
        5,
    )
    .await
    .expect("Failed to transfer batch ERC20 tokens");

    wait_for_block_on_both_nodes(&fb_client, &non_fb_client, block_num, Duration::from_secs(10))
        .await
        .expect("Failed to wait for block on both nodes");

    let fb_block = eth_get_block_by_number_or_hash(&fb_client, BlockId::Number(block_num), false)
        .await
        .expect("Failed to get block from fb client");
    let fb_block_hash =
        fb_block["hash"].as_str().expect("Block hash should not be empty").to_string();

    let non_fb_block =
        eth_get_block_by_number_or_hash(&non_fb_client, BlockId::Number(block_num), false)
            .await
            .expect("Failed to get block from non-fb client");
    let non_fb_block_hash =
        non_fb_block["hash"].as_str().expect("Block hash should not be empty").to_string();

    (fb_client, non_fb_client, tx_hashes, block_num, fb_block_hash, non_fb_block_hash, contracts)
}

/// Waits until all pending transactions for the rich address are confirmed.
/// This ensures a clean nonce state between tests so they don't interfere with each other.
pub async fn settle_pending_transactions(endpoint_url: &str) -> Result<()> {
    let client = create_test_client(endpoint_url);
    tokio::time::timeout(DEFAULT_TIMEOUT_TX_TO_BE_MINED, async {
        loop {
            let pending_nonce =
                eth_get_transaction_count(&client, DEFAULT_RICH_ADDRESS, Some(BlockId::Pending))
                    .await?;
            let latest_nonce =
                eth_get_transaction_count(&client, DEFAULT_RICH_ADDRESS, Some(BlockId::Latest))
                    .await?;
            if pending_nonce == latest_nonce {
                return <Result<()>>::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| eyre!("timeout waiting for pending transactions to settle"))?
}

/// Waits for the canonical chain to reach the given block number.
/// This is needed when a receipt is obtained from the flashblock cache (ahead of canonical)
/// but subsequent operations (e.g., `debug_traceTransaction`, `eth_getLogs`) require canonical data.
pub async fn wait_for_canonical_block(endpoint_url: &str, target_block: u64) -> Result<()> {
    let client = create_test_client(endpoint_url);
    tokio::time::timeout(DEFAULT_TIMEOUT_TX_TO_BE_MINED, async {
        loop {
            let current = eth_block_number(&client).await?;
            if current >= target_block {
                return <Result<()>>::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| eyre!("timeout waiting for canonical block {target_block}"))?
}

/// Waits for a transaction receipt with timeout
pub async fn wait_for_tx_mined(endpoint_url: &str, tx_hash: &str) -> Result<serde_json::Value> {
    let client = create_test_client(endpoint_url);
    tokio::time::timeout(DEFAULT_TIMEOUT_TX_TO_BE_MINED, async {
        loop {
            if let Ok(receipt) = eth_get_transaction_receipt(&client, tx_hash).await
                && !receipt.is_null()
            {
                let status =
                    receipt["status"].as_str().ok_or(eyre!("tx receipt missing status field"))?;
                if status == "0x1" {
                    return Ok(receipt);
                } else {
                    return Err(eyre!("tx execution failed with status: {}", status));
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| eyre!("timeout waiting for tx to be mined: {}", tx_hash))?
}

/// Waits for a block to be available on both flashblock and non-flashblock nodes
/// This ensures synchronization before running comparison tests
pub async fn wait_for_block_on_both_nodes(
    fb_client: &HttpClient,
    non_fb_client: &HttpClient,
    block_num: u64,
    timeout: Duration,
) -> Result<()> {
    println!("Waiting for block {block_num} to be available on both nodes...");

    tokio::time::timeout(timeout, async {
        loop {
            // Check if block is available on flashblock node
            let fb_available =
                eth_get_block_by_number_or_hash(fb_client, BlockId::Number(block_num), false)
                    .await
                    .map(|block| !block.is_null())
                    .unwrap_or(false);

            // Check if block is available on non-flashblock node
            let non_fb_available =
                eth_get_block_by_number_or_hash(non_fb_client, BlockId::Number(block_num), false)
                    .await
                    .map(|block| !block.is_null())
                    .unwrap_or(false);

            if fb_available && non_fb_available {
                println!("Block {block_num} is now available on both nodes");
                return Ok(());
            }

            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| eyre!("timeout waiting for block {} to be available on both nodes", block_num))?
}

/// Send the tx request and wait for it to be mined
pub async fn sign_and_send_transaction(
    endpoint_url: &str,
    private_key: &str,
    tx_request: TransactionRequest,
) -> Result<(String, serde_json::Value)> {
    let key_str = private_key.trim_start_matches("0x");
    let signer = PrivateKeySigner::from_str(key_str)
        .map_err(|e| eyre!("Failed to parse private key: {}", e))?;
    let wallet = EthereumWallet::from(signer);
    let provider = ProviderBuilder::new().wallet(wallet).connect_http(endpoint_url.parse()?);

    let pending_tx = provider
        .send_transaction(tx_request)
        .await
        .map_err(|e| eyre!("Failed to send transaction: {}", e))?;

    let tx_hash = *pending_tx.tx_hash();
    println!("tx sent: {tx_hash:#x}");

    let receipt = wait_for_tx_mined(endpoint_url, &format!("{tx_hash:#x}")).await?;
    let block_number = receipt["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| eyre!("Failed to parse block number"))?;
    let gas_used = receipt["gasUsed"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| eyre!("Failed to parse gas used"))?;
    println!("tx mined in block {block_number}, gas used: {gas_used}");

    Ok((format!("{tx_hash:#x}"), receipt))
}

/// Extracts the refund counter for a specific opcode from a debug trace result
pub fn get_refund_counter_from_trace(trace_result: &serde_json::Value, opcode: &str) -> u64 {
    let mut refund_counter = 0u64;

    // Get the structLogs array
    let struct_logs = match trace_result["structLogs"].as_array() {
        Some(logs) => logs,
        None => return refund_counter,
    };

    // Iterate through each log entry
    for entry in struct_logs {
        // Check if this log has the matching opcode
        if let Some(op) = entry["op"].as_str()
            && op == opcode
        {
            // Extract the refund value
            if let Some(refund) = entry["refund"].as_u64() {
                refund_counter = refund;
                break;
            } else if let Some(refund) = entry["refund"].as_f64() {
                refund_counter = refund as u64;
                break;
            }
        }
    }

    refund_counter
}

pub async fn make_contract_call(
    endpoint_url: &str,
    private_key: &str,
    contract_address: Address,
    calldata: Bytes,
    value: U256,
    tx_request: TransactionRequest,
) -> Result<serde_json::Value> {
    let key_str = private_key.trim_start_matches("0x");
    let signer = PrivateKeySigner::from_str(key_str)
        .map_err(|e| eyre!("Failed to parse private key: {}", e))?;
    let wallet = EthereumWallet::from(signer.clone());
    let provider = ProviderBuilder::new().wallet(wallet).connect_http(endpoint_url.parse()?);

    let from = signer.address();
    let nonce = provider.get_transaction_count(from).pending().await?;
    let gas_price = provider.get_gas_price().await?;

    let tx = tx_request
        .to(contract_address)
        .input(calldata.into())
        .value(value)
        .with_from(from)
        .with_nonce(nonce)
        .with_chain_id(DEFAULT_L2_CHAIN_ID)
        .with_gas_price(gas_price);

    let pending_tx = provider
        .send_transaction(tx)
        .await
        .map_err(|e| eyre!("Failed to send contract call transaction: {}", e))?;

    let tx_hash = *pending_tx.tx_hash();
    println!("Contract call tx sent: {tx_hash:#x}");

    let receipt = wait_for_tx_mined(endpoint_url, &format!("{tx_hash:#x}")).await?;
    let block_number = receipt["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| eyre!("Failed to parse block number"))?;
    let gas_used = receipt["gasUsed"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| eyre!("Failed to parse gas used"))?;
    println!("Contract call tx mined in block {block_number}, gas used: {gas_used}");

    // Return transaction hash as JSON
    Ok(serde_json::json!({
        "transactionHash": format!("{:#x}", tx_hash),
        "receipt": receipt
    }))
}

/// Deploy a child contract via the `Create2Factory` using the given salt.
/// Returns the tx hash of the deploy transaction.
pub async fn deploy_via_create2(
    endpoint_url: &str,
    factory_address: Address,
    salt: U256,
) -> Result<String> {
    sol! {
        function deploy(uint256 salt) external payable returns (address addr);
    }
    let call = deployCall { salt };
    let calldata = Bytes::from(call.abi_encode());

    let signer = PrivateKeySigner::from_str(DEFAULT_RICH_PRIVATE_KEY.trim_start_matches("0x"))?;
    let wallet = EthereumWallet::from(signer.clone());
    let provider = ProviderBuilder::new().wallet(wallet).connect_http(endpoint_url.parse()?);

    let from = signer.address();
    let nonce = provider.get_transaction_count(from).pending().await?;
    let gas_price = provider.get_gas_price().await?;
    let tx = TransactionRequest::default()
        .with_from(from)
        .with_to(factory_address)
        .with_input(calldata)
        .with_nonce(nonce)
        .with_chain_id(DEFAULT_L2_CHAIN_ID)
        .with_gas_limit(500_000)
        .with_gas_price(gas_price);
    let pending_tx = provider.send_transaction(tx).await?;
    let tx_hash = *pending_tx.tx_hash();
    Ok(format!("{tx_hash:#x}"))
}

/// Compute the deterministic CREATE2 address for a given salt via the factory.
pub async fn compute_create2_address(
    endpoint_url: &str,
    factory_address: Address,
    salt: U256,
) -> Result<Address> {
    sol! {
        function computeAddress(uint256 salt) external view returns (address);
    }
    let call = computeAddressCall { salt };
    let calldata = call.abi_encode();

    let client = create_test_client(endpoint_url);
    let call_args = serde_json::json!({
        "to": factory_address,
        "data": format!("0x{}", hex::encode(&calldata)),
    });
    let result = eth_call(&client, Some(call_args), Some(BlockId::Latest)).await?;

    let bytes = hex::decode(result.trim_start_matches("0x"))?;
    if bytes.len() < 32 {
        return Err(eyre!("computeAddress returned unexpected length: {}", bytes.len()));
    }
    // Address is in the last 20 bytes of the 32-byte ABI-encoded word
    Ok(Address::from_slice(&bytes[12..32]))
}

/// Destroy a child contract via the `Create2Factory`.
/// Returns the tx hash.
pub async fn destroy_via_factory(
    endpoint_url: &str,
    factory_address: Address,
    child_address: Address,
    recipient: Address,
) -> Result<String> {
    sol! {
        function destroyChild(address child, address recipient) external;
    }
    let call = destroyChildCall { child: child_address, recipient };
    let calldata = Bytes::from(call.abi_encode());

    let signer = PrivateKeySigner::from_str(DEFAULT_RICH_PRIVATE_KEY.trim_start_matches("0x"))?;
    let wallet = EthereumWallet::from(signer.clone());
    let provider = ProviderBuilder::new().wallet(wallet).connect_http(endpoint_url.parse()?);

    let from = signer.address();
    let nonce = provider.get_transaction_count(from).pending().await?;
    let gas_price = provider.get_gas_price().await?;
    let tx = TransactionRequest::default()
        .with_from(from)
        .with_to(factory_address)
        .with_input(calldata)
        .with_nonce(nonce)
        .with_chain_id(DEFAULT_L2_CHAIN_ID)
        .with_gas_limit(100_000)
        .with_gas_price(gas_price);
    let pending_tx = provider.send_transaction(tx).await?;
    let tx_hash = *pending_tx.tx_hash();
    Ok(format!("{tx_hash:#x}"))
}

/// Check if a flashblocks notification contains a specific transaction hash
pub fn contains_tx_hash(notification: &serde_json::Value, tx_hash: &str) -> bool {
    let Some(tx_hash_str) = notification
        .get("transaction")
        .and_then(|tx| tx.get("txHash"))
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };

    tx_hash_str == tx_hash
}

pub fn process_flashblock_message(
    msg: &str,
    count: &mut HashMap<String, u64>,
    current_block_number: u64,
    source: &str,
) {
    let Ok(notification) = serde_json::from_str::<serde_json::Value>(msg) else {
        return;
    };

    let Some(block_num) = notification["metadata"]["block_number"].as_u64() else {
        return;
    };

    assert!(
        block_num >= current_block_number,
        "Flashblock block number {block_num} should be >= current block {current_block_number}"
    );

    if let Some(txs) = notification["diff"]["transactions"].as_array() {
        for tx in txs {
            let Some(tx_rlp_hex) = tx.as_str() else { continue };
            let raw = tx_rlp_hex.trim_start_matches("0x");
            let Ok(bytes) = hex::decode(raw) else {
                eprintln!("Failed to hex-decode RLP: {tx_rlp_hex}");
                continue;
            };

            let hash = alloy_primitives::keccak256(&bytes);
            let hash_str = format!("0x{}", hex::encode(hash));

            if let Some(c) = count.get_mut(&hash_str) {
                *c += 1;
                println!("[{source}] Found tx in block {block_num}: {hash_str} (count: {c})");
            }
        }
    }
}

pub fn validate_internal_transaction(
    inner_tx: &serde_json::Value,
    expected_from: String,
    expected_to: String,
    context: &str,
) -> Result<()> {
    let has_error = inner_tx["is_error"]
        .as_bool()
        .ok_or_else(|| eyre!("{}: 'is_error' field must be a boolean", context))?;

    if has_error {
        return Err(eyre!(
            "{}: Inner transaction should not have an error (is_error: {:?})",
            context,
            inner_tx["is_error"]
        ));
    }

    let from_addr = inner_tx["from"]
        .as_str()
        .ok_or_else(|| eyre!("{}: Inner transaction should have 'from' field", context))?;

    if from_addr.to_lowercase() != expected_from.to_lowercase() {
        return Err(eyre!(
            "{}: Inner transaction from address mismatch. Expected: {}, Got: {}",
            context,
            expected_from,
            from_addr
        ));
    }

    let to_addr = inner_tx["to"]
        .as_str()
        .ok_or_else(|| eyre!("{}: Inner transaction should have 'to' field", context))?;

    if to_addr.to_lowercase() != expected_to.to_lowercase() {
        return Err(eyre!(
            "{}: Inner transaction to address mismatch. Expected: {}, Got: {}",
            context,
            expected_to,
            to_addr
        ));
    }

    Ok(())
}
