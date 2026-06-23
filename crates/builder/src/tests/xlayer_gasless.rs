//! In-process flashblocks tests for gasless (zero-priced, whitelisted) transactions.
//!
//! These exercise the gasless path end-to-end through the flashblocks builder:
//! - the mempool accepts a zero-priced tx (gasless mempool enabled, `minimal_protocol_basefee`
//!   lowered to 0), and
//! - the flashblocks payload builder executes it gaslessly when the on-chain whitelist contract
//!   (derived from the chain id) approves it (see `FlashblocksBuilderCtx::transact_maybe_gasless`).
//!
//! Note on base fee: reth's pool best-iterator (`BestTransactionsWithFees`) only yields txs whose
//! `max_fee_per_gas >= block base fee`, so a zero-priced tx is only yielded when the block base fee
//! is 0. The test genesis sets base fee 0 (a fixed point under EIP-1559), so zero-priced txs flow
//! through the builder. Gasless execution itself does *not* depend on base fee —
//! `OpEvm::transact_raw` zeroes the base fee for gasless txs — so the observable gasless
//! distinction is fee *charging*: a gasless tx pays no gas fee, so the sender's balance decreases
//! by exactly the transferred value.

use crate::{
    args::BuilderArgs,
    tests::{
        builder_signer, default_node_config, funded_signer, BlockTransactionsExt, LocalInstance,
    },
};
use alloy_consensus::TxEip1559;
use alloy_eips::{BlockNumberOrTag::Latest, Encodable2718};
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_network::ReceiptResponse;
use alloy_primitives::{address, Address, Bytes, TxKind, B256, U256};
use alloy_provider::Provider;
use macros::rb_test;
use op_alloy_consensus::OpTypedTransaction;
use reth_node_builder::NodeConfig;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::XLAYER_DEVNET_GASLESS_CONTRACT as GASLESS_CONTRACT;
use std::{sync::Arc, time::Duration};

/// Recipient of the gasless test transfers.
const RECIPIENT: Address = address!("1111111111111111111111111111111111111111");

/// Minimal contract bytecode returning ABI `(true, 0xffffff)` for any call — approves every gasless
/// query with a gas allowance far above the test tx's gas limit. Layout: `mem[0..32]=1` (allowed),
/// `mem[32..64]=0xffffff` (gasLimit), `return mem[0..64]`.
const ALLOW_HIGH_GAS_BYTECODE: [u8; 17] = [
    0x60, 0x01, 0x60, 0x00, 0x52, 0x62, 0xff, 0xff, 0xff, 0x60, 0x20, 0x52, 0x60, 0x40, 0x60, 0x00,
    0xf3,
];

/// Minimal contract bytecode returning ABI `(false, 0)` for any call (64 zero bytes) — denies every
/// gasless query.
const DENY_BYTECODE: [u8; 5] = [0x60, 0x40, 0x60, 0x00, 0xf3];

/// Builds the in-process node config with a custom `OpChainSpec` that:
/// - runs on the XLayer devnet chain id (so `OpEvmConfig` derives the gasless contract), and
/// - deploys `gasless_bytecode` at [`GASLESS_CONTRACT`] when `Some`, or leaves that address with no
///   code when `None` (simulating a chain where the gasless predeploy was never deployed).
///
/// The base genesis is the same template the default test harness uses, so the funded test
/// accounts and system contracts are present.
fn gasless_node_config_opt(gasless_bytecode: Option<&[u8]>) -> NodeConfig<OpChainSpec> {
    let genesis_json = include_str!("./framework/artifacts/genesis.json.tmpl");
    let mut genesis: Genesis =
        serde_json::from_str(genesis_json).expect("invalid genesis template JSON");

    // Run on the XLayer devnet chain id (195) so `OpEvmConfig` auto-derives the gasless contract
    // address (`XLAYER_DEVNET_GASLESS_CONTRACT`, re-exported here as `GASLESS_CONTRACT`).
    genesis.config.chain_id = 195;

    // Block base fee 0 (a fixed point under EIP-1559) so the pool's best-iterator yields the
    // zero-priced tx (`max_fee_per_gas (0) >= base_fee (0)`). Gasless execution does not need this
    // — the base-fee check is disabled for gasless txs — it only lets the 0-price tx through the
    // pool's fee filter. (The genesis base fee of 1 cannot decay to 0: the EIP-1559 step rounds to
    // 0 at base fee 1, so the value would be stuck at 1.)
    genesis.base_fee_per_gas = Some(0);

    // Deploy the gasless whitelist contract at the devnet gasless predeploy address. When `None`,
    // the address is left with no code so the gasless system call returns empty (decoded as
    // `(false, 0)` — "not gasless").
    if let Some(gasless_bytecode) = gasless_bytecode {
        genesis.alloc.insert(
            GASLESS_CONTRACT,
            GenesisAccount {
                code: Some(Bytes::copy_from_slice(gasless_bytecode)),
                ..Default::default()
            },
        );
    }

    let chain_spec = OpChainSpec::from_genesis(genesis);
    default_node_config().with_chain(Arc::new(chain_spec))
}

/// Builds the in-process node config with the gasless whitelist contract deployed at
/// [`GASLESS_CONTRACT`]. See [`gasless_node_config_opt`].
fn gasless_node_config(gasless_bytecode: &[u8]) -> NodeConfig<OpChainSpec> {
    gasless_node_config_opt(Some(gasless_bytecode))
}

/// Builds [`BuilderArgs`] for the gasless tests. The gasless mempool is enabled by the test
/// harness because the chain runs on an XLayer chain id (see `gasless_node_config`), so no
/// separate arg is needed.
fn gasless_args() -> BuilderArgs {
    BuilderArgs {
        // Use the same builder signer the harness uses so the builder tx is deterministic.
        builder_signer: Some(builder_signer()),
        ..Default::default()
    }
}

/// Signs a zero-priced (`max_fee_per_gas == 0`) EIP-1559 transfer of `value` wei to [`RECIPIENT`]
/// from the genesis-funded account, returning `(encoded_tx, tx_hash)`.
async fn build_zero_priced_transfer(
    provider: &alloy_provider::RootProvider<op_alloy_network::Optimism>,
    value: u128,
) -> eyre::Result<(Vec<u8>, B256)> {
    let sender = funded_signer();
    let nonce = provider.get_transaction_count(sender.address).pending().await.unwrap_or_default();
    let tx = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 195,
        nonce,
        gas_limit: 21_000,
        max_fee_per_gas: 0,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(RECIPIENT),
        value: U256::from(value),
        ..Default::default()
    });
    let signed = sender.sign_tx(tx)?;
    let tx_hash = B256::from_slice(signed.tx_hash().as_ref());
    Ok((signed.encoded_2718(), tx_hash))
}

/// Builds a single empty block from the pool with a zero minimum base fee, one second after the
/// latest block's timestamp.
async fn build_block_from_pool(
    driver: &crate::tests::ChainDriver,
) -> eyre::Result<alloy_rpc_types_eth::Block<op_alloy_rpc_types::Transaction>> {
    let latest = driver.get_block(Latest).await?.expect("latest block must exist");
    let block_timestamp = Duration::from_secs(latest.header.timestamp) + Duration::from_secs(1);
    driver
        .build_new_block_with_txs_timestamp(vec![], None, Some(block_timestamp), None, Some(0))
        .await
}

/// Builds blocks until `tx_hash` is included (bounded), returning the block that includes it.
async fn build_until_included(
    driver: &crate::tests::ChainDriver,
    tx_hash: B256,
) -> eyre::Result<alloy_rpc_types_eth::Block<op_alloy_rpc_types::Transaction>> {
    for _ in 0..5 {
        let block = build_block_from_pool(driver).await?;
        if block.includes(&tx_hash) {
            return Ok(block);
        }
    }
    eyre::bail!("transaction {tx_hash} was not included within the expected number of blocks")
}

/// With gasless enabled and the whitelist contract approving everything, a zero-priced tx is
/// accepted by the mempool, executed gaslessly by the flashblocks builder, and included with a
/// successful receipt. Because it is gasless, the sender pays *no* gas fee — its balance decreases
/// by exactly the transferred value.
#[rb_test(
    args = gasless_args(),
    config = gasless_node_config(&ALLOW_HIGH_GAS_BYTECODE)
)]
async fn gasless_zero_price_tx_whitelisted_included(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let provider = driver.provider().clone();
    let sender = funded_signer();

    let transfer_value = 1_000u128;
    let balance_before = provider.get_balance(sender.address).await?;

    let (encoded, tx_hash) = build_zero_priced_transfer(&provider, transfer_value).await?;
    // Submit through the RPC -> mempool. The gasless mempool accepts the zero-priced tx.
    // Drop the pending-tx handle: block production is driven manually below.
    let _pending = provider.send_raw_transaction(encoded.as_slice()).await?;

    let block = build_until_included(&driver, tx_hash).await?;
    assert!(
        block.includes(&tx_hash),
        "gasless zero-priced whitelisted tx should be included in the block"
    );

    let receipt =
        provider.get_transaction_receipt(tx_hash).await?.expect("gasless tx should have a receipt");
    assert!(receipt.status(), "gasless tx receipt should be successful");

    // Gasless => no gas fee charged. The sender's balance drops by exactly the transferred value.
    let balance_after = provider.get_balance(sender.address).await?;
    assert_eq!(
        balance_before - balance_after,
        U256::from(transfer_value),
        "gasless tx must not charge the sender any gas fee"
    );

    Ok(())
}

/// With the gasless contract denying everything, the mempool's gasless admission gate rejects the
/// zero-priced tx at `eth_sendRawTransaction`: it is not whitelisted, so it cannot be gasless, and a
/// non-gasless zero-priced tx is underpriced. This asserts the whitelist gate is enforced at
/// admission — the deny contract is consulted (returns false) and the tx is rejected rather than
/// admitted.
#[rb_test(
    args = gasless_args(),
    config = gasless_node_config(&DENY_BYTECODE)
)]
async fn gasless_zero_price_tx_not_whitelisted_rejected(
    rbuilder: LocalInstance,
) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let provider = driver.provider().clone();

    let (encoded, _tx_hash) = build_zero_priced_transfer(&provider, 1_000u128).await?;
    let result = provider.send_raw_transaction(encoded.as_slice()).await;

    assert!(
        result.is_err(),
        "a non-whitelisted zero-priced tx must be rejected by the gasless mempool admission gate"
    );

    Ok(())
}

/// With no gasless contract deployed at [`GASLESS_CONTRACT`] (the chain runs an XLayer chain id, so
/// the contract address is configured, but the predeploy was never deployed), the gasless system
/// call hits an account with no code and returns empty, decoded as `(false, 0)` — "not gasless".
/// The zero-priced tx is therefore not whitelisted and is rejected at admission, exactly as in the
/// deny case. This guards the empty-account path (distinct from a deployed contract returning
/// false).
#[rb_test(
    args = gasless_args(),
    config = gasless_node_config_opt(None)
)]
async fn gasless_zero_price_tx_no_contract_rejected(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let provider = driver.provider().clone();

    let (encoded, _tx_hash) = build_zero_priced_transfer(&provider, 1_000u128).await?;
    let result = provider.send_raw_transaction(encoded.as_slice()).await;

    assert!(
        result.is_err(),
        "a zero-priced tx must be rejected when no gasless contract is deployed to whitelist it"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Gasless block gas limit tests
// ---------------------------------------------------------------------------

const NUM_GASLESS_SIGNERS: usize = 55;
const GASLESS_TX_GAS: u64 = 21_000;
const GASLESS_BUDGET: u64 = 1_000_000;
const MAX_GASLESS_PER_BLOCK: usize = (GASLESS_BUDGET / GASLESS_TX_GAS) as usize; // 47

/// Builds a node config with 55 pre-funded random signers for gasless budget testing.
/// Returns `(config, signers)`.
fn gasless_budget_node_config(signers: &[crate::signer::Signer]) -> NodeConfig<OpChainSpec> {
    let genesis_json = include_str!("./framework/artifacts/genesis.json.tmpl");
    let mut genesis: Genesis =
        serde_json::from_str(genesis_json).expect("invalid genesis template JSON");

    genesis.config.chain_id = 195;
    genesis.base_fee_per_gas = Some(0);

    // Deploy gasless whitelist contract (allow-all)
    genesis.alloc.insert(
        GASLESS_CONTRACT,
        GenesisAccount {
            code: Some(Bytes::copy_from_slice(&ALLOW_HIGH_GAS_BYTECODE)),
            ..Default::default()
        },
    );

    // Fund each random signer with enough ETH for their gasless transfer
    for signer in signers {
        genesis.alloc.insert(
            signer.address,
            GenesisAccount {
                balance: U256::from(1_000_000_000_000_000_000u128),
                ..Default::default()
            },
        );
    }

    let chain_spec = OpChainSpec::from_genesis(genesis);
    default_node_config().with_chain(Arc::new(chain_spec))
}

/// BuilderArgs with gasless block gas limit = 1,000,000 (= 1M gas)
fn gasless_budget_args() -> BuilderArgs {
    BuilderArgs {
        builder_signer: Some(builder_signer()),
        gasless_block_gas_limit_raw: Some(GASLESS_BUDGET),
        ..Default::default()
    }
}

/// Signs a zero-priced EIP-1559 transfer from a specific signer.
fn build_zero_priced_transfer_from(
    signer: &crate::signer::Signer,
    nonce: u64,
    value: u128,
) -> (Vec<u8>, B256) {
    let tx = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 195,
        nonce,
        gas_limit: GASLESS_TX_GAS,
        max_fee_per_gas: 0,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(RECIPIENT),
        value: U256::from(value),
        ..Default::default()
    });
    let signed = signer.sign_tx(tx).expect("failed to sign tx");
    let tx_hash = B256::from_slice(signed.tx_hash().as_ref());
    (signed.encoded_2718(), tx_hash)
}

/// Verifies the per-block gasless gas budget:
/// 1. **Cap**: first block includes ≤ 47 gasless txs (budget 1M / 21k gas each)
/// 2. **Reset**: second block picks up remaining gasless txs (budget resets per block)
/// 3. **Paid unaffected**: paid tx included in same block as capped gasless txs
/// 4. **No permanent discard**: all 55 gasless txs are eventually included
#[rb_test(
    args = gasless_budget_args(),
    config = gasless_budget_node_config(&*GASLESS_SIGNERS)
)]
async fn gasless_block_gas_limit_caps_and_resets(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let provider = driver.provider().clone();

    // Submit 55 gasless txs (one from each pre-funded random signer, nonce 0)
    let mut gasless_hashes = Vec::with_capacity(NUM_GASLESS_SIGNERS);
    for signer in GASLESS_SIGNERS.iter() {
        let (encoded, tx_hash) = build_zero_priced_transfer_from(signer, 0, 1_000);
        let _pending = provider.send_raw_transaction(encoded.as_slice()).await?;
        gasless_hashes.push(tx_hash);
    }

    // Submit 1 paid tx from the funded signer
    let paid_signer = funded_signer();
    let paid_nonce =
        provider.get_transaction_count(paid_signer.address).pending().await.unwrap_or_default();
    let paid_tx = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 195,
        nonce: paid_nonce,
        gas_limit: 21_000,
        max_fee_per_gas: 1_000_000,
        max_priority_fee_per_gas: 1_000_000,
        to: TxKind::Call(RECIPIENT),
        value: U256::from(500u128),
        ..Default::default()
    });
    let paid_signed = paid_signer.sign_tx(paid_tx)?;
    let paid_hash = B256::from_slice(paid_signed.tx_hash().as_ref());
    let _pending = provider.send_raw_transaction(paid_signed.encoded_2718().as_slice()).await?;

    // Build block 1
    let block1 = build_block_from_pool(&driver).await?;
    let block1_gasless_count =
        gasless_hashes.iter().filter(|h| block1.transactions.hashes().any(|bh| bh == **h)).count();

    // Assertion 1: Cap — at most 47 gasless txs in block 1
    assert!(
        block1_gasless_count <= MAX_GASLESS_PER_BLOCK,
        "block 1 should include at most {MAX_GASLESS_PER_BLOCK} gasless txs, got {block1_gasless_count}"
    );
    assert!(block1_gasless_count > 0, "block 1 should include at least some gasless txs");

    // Assertion 3: Paid tx unaffected — included in same block
    assert!(
        block1.transactions.hashes().any(|h| h == paid_hash),
        "paid tx must be included in block 1 even when gasless budget is exhausted"
    );

    // Build block 2
    let block2 = build_block_from_pool(&driver).await?;
    let block2_gasless_count =
        gasless_hashes.iter().filter(|h| block2.transactions.hashes().any(|bh| bh == **h)).count();

    // Assertion 2: Reset — block 2 picks up remaining gasless txs
    assert!(
        block2_gasless_count > 0,
        "block 2 should include previously skipped gasless txs (budget must reset per block)"
    );

    // Assertion 4: No permanent discard — all gasless txs included across both blocks
    // (may need more blocks if 55 > 47 + 47, but 55 < 94 so 2 blocks suffice)
    let total_gasless_included = block1_gasless_count + block2_gasless_count;
    assert_eq!(
        total_gasless_included, NUM_GASLESS_SIGNERS,
        "all {NUM_GASLESS_SIGNERS} gasless txs must be included across 2 blocks (no permanent discard), got {total_gasless_included}"
    );

    Ok(())
}

/// Pre-generated random signers for the gasless budget test. Using `LazyLock` ensures they are
/// created once and reused across the `config` and test body (both reference `GASLESS_SIGNERS`).
static GASLESS_SIGNERS: std::sync::LazyLock<[crate::signer::Signer; NUM_GASLESS_SIGNERS]> =
    std::sync::LazyLock::new(|| core::array::from_fn(|_| crate::signer::Signer::random()));
