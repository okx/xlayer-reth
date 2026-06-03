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
//! through the builder. Gasless execution itself does *not* depend on base fee — the
//! `GaslessFeeHook` disables the base-fee check for gasless txs — so the observable gasless
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
async fn gasless_zero_price_tx_no_contract_rejected(
    rbuilder: LocalInstance,
) -> eyre::Result<()> {
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
