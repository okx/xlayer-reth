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
//! through the builder. Gasless execution itself does *not* depend on base fee — the Optimism
//! execution layer applies a transaction-scoped base-fee *validation* bypass for gasless txs
//! (toggling `cfg.disable_base_fee` for the single tx and restoring it afterwards, never mutating
//! `block.basefee`) — so the observable gasless distinction is fee *charging*: a gasless tx pays no
//! gas fee, so the sender's balance decreases by exactly the transferred value.

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

/// Address the BASEFEE store probe is deployed at in genesis (see [`gasless_node_config_opt`]).
const BASEFEE_STORE_PROBE: Address = address!("00000000000000000000000000000000ba5efee0");

/// Address the BASEFEE-then-REVERT probe is deployed at in genesis (see [`gasless_node_config_opt`]).
const BASEFEE_REVERT_PROBE: Address = address!("00000000000000000000000000000000ba5efee1");

/// `BASEFEE_STORE_PROBE` runtime bytecode: `BASEFEE; PUSH1 0x00; CALLDATALOAD; SSTORE; STOP`.
/// Reads the current block base fee via the `BASEFEE` opcode and stores it to the storage slot whose
/// key is supplied as the first 32 bytes of calldata, so the observed base fee can be read back
/// post-block via `eth_getStorageAt`. Stack for `SSTORE`: value (`BASEFEE`) pushed first, then key
/// (`CALLDATALOAD`) on top — `storage[key] = basefee`.
const BASEFEE_STORE_PROBE_BYTECODE: [u8; 6] = [0x48, 0x60, 0x00, 0x35, 0x55, 0x00];

/// `BASEFEE_REVERT_PROBE` runtime bytecode: `BASEFEE; POP; PUSH1 0x00; PUSH1 0x00; REVERT`.
/// Reads the base fee then reverts with empty return data — exercises the execution-failure receipt
/// path.
const BASEFEE_REVERT_PROBE_BYTECODE: [u8; 7] = [0x48, 0x50, 0x60, 0x00, 0x60, 0x00, 0xfd];

/// Builds the in-process node config with a custom `OpChainSpec` that:
/// - runs on the XLayer devnet chain id (so `OpEvmConfig` derives the gasless contract), and
/// - deploys `gasless_bytecode` at [`GASLESS_CONTRACT`] when `Some`, or leaves that address with no
///   code when `None` (simulating a chain where the gasless predeploy was never deployed).
///
/// The base genesis is the same template the default test harness uses, so the funded test
/// accounts and system contracts are present.
fn gasless_node_config_opt(
    gasless_bytecode: Option<&[u8]>,
    base_fee_per_gas: u64,
) -> NodeConfig<OpChainSpec> {
    let genesis_json = include_str!("./framework/artifacts/genesis.json.tmpl");
    let mut genesis: Genesis =
        serde_json::from_str(genesis_json).expect("invalid genesis template JSON");

    // Run on the XLayer devnet chain id (195) so `OpEvmConfig` auto-derives the gasless contract
    // address (`XLAYER_DEVNET_GASLESS_CONTRACT`, re-exported here as `GASLESS_CONTRACT`).
    genesis.config.chain_id = 195;

    // Block base fee, parameterized per test:
    // - `0` (a fixed point under EIP-1559) lets the pool's best-iterator yield a zero-priced tx
    //   (`max_fee_per_gas (0) >= base_fee (0)`), so mempool-fed gasless tests exercise the
    //   `execute_best_transactions` path. Gasless execution does not need this — the base-fee check
    //   is disabled for gasless txs — it only lets the 0-price tx through the pool's fee filter.
    // - a non-zero value (e.g. `1`, a fixed point that does not decay to 0: the EIP-1559 step rounds
    //   to 0 at base fee 1) lets a test supply a gasless tx directly via payload attributes
    //   (`no_tx_pool = true`), where inclusion over a non-zero base fee proves gasless execution.
    genesis.base_fee_per_gas = Some(base_fee_per_gas.into());

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

    // Deploy the BASEFEE probes used by the same-block isolation tests. They are inert for the
    // other gasless tests (nothing calls their addresses), so deploying them here keeps a single
    // gasless genesis helper.
    for (address, code) in [
        (BASEFEE_STORE_PROBE, BASEFEE_STORE_PROBE_BYTECODE.as_slice()),
        (BASEFEE_REVERT_PROBE, BASEFEE_REVERT_PROBE_BYTECODE.as_slice()),
    ] {
        genesis.alloc.insert(
            address,
            GenesisAccount { code: Some(Bytes::copy_from_slice(code)), ..Default::default() },
        );
    }

    let chain_spec = OpChainSpec::from_genesis(genesis);
    default_node_config().with_chain(Arc::new(chain_spec))
}

/// Builds the in-process node config with the gasless whitelist contract deployed at
/// [`GASLESS_CONTRACT`]. See [`gasless_node_config_opt`].
fn gasless_node_config(gasless_bytecode: &[u8]) -> NodeConfig<OpChainSpec> {
    gasless_node_config_opt(Some(gasless_bytecode), 0)
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

/// Regression test for gasless tx supplied via payload attributes when no_tx_pool=true. Gasless
/// must be included in the block instead of skipping.
#[rb_test(
    args = gasless_args(),
    config = gasless_node_config_opt(Some(&ALLOW_HIGH_GAS_BYTECODE), 1)
)]
async fn gasless_tx_in_attributes_no_tx_pool_included_over_base_fee(
    rbuilder: LocalInstance,
) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let provider = driver.provider().clone();

    // Build the gasless tx but do NOT submit it to the mempool; pass it in the payload attributes
    // with `no_tx_pool = true` so it is executed by `execute_sequencer_transactions`.
    let (encoded, tx_hash) = build_zero_priced_transfer(&provider, 1_000u128).await?;

    let latest = driver.get_block(Latest).await?.expect("latest block must exist");
    let block_timestamp = Duration::from_secs(latest.header.timestamp) + Duration::from_secs(1);
    let block = driver
        .build_new_block_with_txs_timestamp(
            vec![encoded.into()],
            Some(true),
            Some(block_timestamp),
            None,
            Some(0),
        )
        .await?;

    // With base fee > 0, a non-gasless zero-priced tx is base-fee-rejected and skipped, so inclusion
    // proves it executed gaslessly.
    assert!(
        block.includes(&tx_hash),
        "gasless tx supplied via payload attributes (no_tx_pool) over a non-zero base fee must be \
         executed gaslessly and included"
    );

    let receipt =
        provider.get_transaction_receipt(tx_hash).await?.expect("gasless tx should have a receipt");
    assert!(receipt.status(), "gasless tx receipt should be successful");

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
    config = gasless_node_config_opt(None, 0)
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
        let (encoded, tx_hash) =
            build_zero_priced_transfer_from(signer, /* nonce */ 0, /* value */ 1_000);
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

// ---------------------------------------------------------------------------
// Same-block base-fee validation-bypass isolation tests
//
// Prove the per-tx `cfg.disable_base_fee` toggle is restored after a single gasless tx, so it never
// leaks base-fee relaxation into a later tx in the same block. Txs are supplied via payload
// attributes (`no_tx_pool = true`); the pool best-iterator would drop the underpriced sentinel
// (`max_fee_per_gas < base fee`).
//
// Proof shape: after the gasless tx, a non-gasless "underpriced sentinel" (`max_fee_per_gas` below
// the header base fee) is offered. Its exclusion proves base-fee validation was re-enabled — reading
// `BASEFEE` alone can't, since the opcode returns the real base fee regardless of the flag.
// ---------------------------------------------------------------------------

/// Non-zero genesis base fee for the isolation tests, also passed as `min_base_fee` when building.
/// The built block's own base fee decays from this by one EIP-1559 step (the floor only applies to
/// the *next* block) but stays far above the underpriced sentinel's `max_fee_per_gas`, which is all
/// the isolation proof needs.
const ISOLATION_BASE_FEE: u64 = 100;

/// A `max_fee_per_gas` of `1`, far below the built block's header base fee — makes a non-gasless tx
/// "underpriced" so base-fee validation (when enabled) rejects it.
const UNDERPRICED_MAX_FEE: u128 = 1;

/// A `max_fee_per_gas` comfortably above any realistic header base fee — makes a normal tx pass
/// base-fee validation.
const SUFFICIENT_MAX_FEE: u128 = 1_000_000_000;

/// A gas limit below the 21_000 intrinsic-gas floor. A gasless-recognized tx with this limit fails
/// validation on intrinsic gas *after* the gasless cfg override is enabled — exercising the
/// error-return restore path.
const INSUFFICIENT_GAS_LIMIT: u64 = 20_000;

/// A gas limit large enough for a `BASEFEE` + cold `SSTORE` call.
const PROBE_CALL_GAS_LIMIT: u64 = 200_000;

/// Isolation-test node config: reuses [`gasless_node_config_opt`] (XLayer devnet chain id, allow-all
/// gasless whitelist contract, and both BASEFEE probes deployed) with the non-zero isolation base
/// fee. The funded test account (anvil key 0) is already present in the template genesis.
fn gasless_isolation_node_config() -> NodeConfig<OpChainSpec> {
    gasless_node_config_opt(Some(&ALLOW_HIGH_GAS_BYTECODE), ISOLATION_BASE_FEE)
}

/// 32-byte big-endian calldata selecting storage slot `slot` for [`BASEFEE_STORE_PROBE`].
fn probe_slot_calldata(slot: u64) -> Bytes {
    Bytes::copy_from_slice(&U256::from(slot).to_be_bytes::<32>())
}

/// Signs an EIP-1559 call from the funded signer, returning `(encoded_tx, tx_hash)`. Chain id 195
/// and the funded signer match the harness so the tx is admissible. `max_priority_fee_per_gas` is
/// set to `min(max_fee_per_gas, UNDERPRICED_MAX_FEE)` — i.e. capped at both `max_fee_per_gas` (so
/// every built tx stays EIP-1559-valid, including the zero-priced gasless case) and a tiny `1` wei
/// tip (the priority fee is irrelevant to these tests, which only exercise base-fee validation).
fn sign_funded_call(
    nonce: u64,
    max_fee_per_gas: u128,
    gas_limit: u64,
    to: Address,
    input: Bytes,
) -> (Vec<u8>, B256) {
    let tx = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 195,
        nonce,
        gas_limit,
        max_fee_per_gas,
        max_priority_fee_per_gas: max_fee_per_gas.min(UNDERPRICED_MAX_FEE),
        to: TxKind::Call(to),
        value: U256::ZERO,
        input,
        ..Default::default()
    });
    let signed = funded_signer().sign_tx(tx).expect("failed to sign isolation tx");
    let tx_hash = B256::from_slice(signed.tx_hash().as_ref());
    (signed.encoded_2718(), tx_hash)
}

/// Gasless (`max_fee = max_priority = 0`) call to [`BASEFEE_STORE_PROBE`] storing the base fee to
/// `slot`.
fn build_gasless_store_probe_tx(nonce: u64, slot: u64) -> (Vec<u8>, B256) {
    sign_funded_call(nonce, 0, PROBE_CALL_GAS_LIMIT, BASEFEE_STORE_PROBE, probe_slot_calldata(slot))
}

/// Underpriced non-gasless sentinel: a simple transfer whose `max_fee_per_gas` is below the header
/// base fee, so base-fee validation (when enabled) rejects it.
fn build_underpriced_sentinel(nonce: u64) -> (Vec<u8>, B256) {
    sign_funded_call(nonce, UNDERPRICED_MAX_FEE, 21_000, RECIPIENT, Bytes::new())
}

/// Normal (sufficiently-priced) call to [`BASEFEE_STORE_PROBE`] storing the base fee to `slot`.
fn build_normal_store_probe_tx(nonce: u64, slot: u64) -> (Vec<u8>, B256) {
    sign_funded_call(
        nonce,
        SUFFICIENT_MAX_FEE,
        PROBE_CALL_GAS_LIMIT,
        BASEFEE_STORE_PROBE,
        probe_slot_calldata(slot),
    )
}

/// Builds a single block from the given ordered raw txs via payload attributes (`no_tx_pool = true`)
/// with the header base fee floored at [`ISOLATION_BASE_FEE`], one second after the latest block.
async fn build_isolation_block(
    driver: &crate::tests::ChainDriver,
    txs: Vec<Bytes>,
) -> eyre::Result<alloy_rpc_types_eth::Block<op_alloy_rpc_types::Transaction>> {
    let latest = driver.get_block(Latest).await?.expect("latest block must exist");
    let block_timestamp = Duration::from_secs(latest.header.timestamp) + Duration::from_secs(1);
    driver
        .build_new_block_with_txs_timestamp(
            txs,
            Some(true),
            Some(block_timestamp),
            None,
            Some(ISOLATION_BASE_FEE),
        )
        .await
}

/// Reads [`BASEFEE_STORE_PROBE`] storage `slot` and asserts it equals `expected_base_fee` (the
/// actual built-block header base fee — never a genesis constant).
async fn assert_probe_recorded_base_fee(
    provider: &alloy_provider::RootProvider<op_alloy_network::Optimism>,
    slot: u64,
    expected_base_fee: u64,
) -> eyre::Result<()> {
    let stored = provider.get_storage_at(BASEFEE_STORE_PROBE, U256::from(slot)).await?;
    assert_eq!(
        stored,
        U256::from(expected_base_fee),
        "probe storage slot {slot} must equal the built block header base fee {expected_base_fee}"
    );
    Ok(())
}

/// Same-block success-path isolation. Ordered in one block: (1) a gasless BASEFEE-probe tx,
/// (2) an underpriced sentinel, (3) a valid normal BASEFEE-probe tx. Asserts the gasless tx is
/// included and succeeds, the sentinel is excluded (proving base-fee validation was restored after
/// the gasless tx), the normal tx is included and succeeds, and both probes recorded the real
/// header base fee.
#[rb_test(
    args = gasless_args(),
    config = gasless_isolation_node_config()
)]
async fn gasless_same_block_success_isolation(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let provider = driver.provider().clone();

    // nonce 0: gasless probe → included; nonce 1: sentinel (underpriced) → excluded; nonce 1 again:
    // normal probe → included (reuses the sentinel's would-be nonce, so exclusion is purely a
    // base-fee decision, not a nonce gap).
    let (gasless_tx, gasless_hash) =
        build_gasless_store_probe_tx(/* nonce */ 0, /* slot */ 1);
    let (sentinel_tx, sentinel_hash) = build_underpriced_sentinel(/* nonce */ 1);
    let (normal_tx, normal_hash) =
        build_normal_store_probe_tx(/* nonce */ 1, /* slot */ 2);

    let block = build_isolation_block(
        &driver,
        vec![gasless_tx.into(), sentinel_tx.into(), normal_tx.into()],
    )
    .await?;
    let header_base_fee =
        block.header.base_fee_per_gas.expect("EIP-1559 block must have a base fee");
    // `min_base_fee` floors the *next* block's base fee (Jovian encodes it in this block's header),
    // so block 1's own base fee decays from the genesis base fee by the EIP-1559 step and is not
    // itself floored at `ISOLATION_BASE_FEE`. The isolation proof only requires the header base fee
    // to stay above the underpriced sentinel's `max_fee`, so the sentinel is base-fee-rejected when
    // validation is enabled.
    assert!(
        u128::from(header_base_fee) > UNDERPRICED_MAX_FEE,
        "header base fee ({header_base_fee}) must exceed the underpriced sentinel max_fee \
         ({UNDERPRICED_MAX_FEE}) so the sentinel is base-fee-rejected"
    );

    assert!(block.includes(&gasless_hash), "gasless BASEFEE-probe tx must be included");
    assert!(
        !block.includes(&sentinel_hash),
        "underpriced sentinel must be excluded — proves base-fee validation was restored after the \
         gasless tx"
    );
    assert!(block.includes(&normal_hash), "valid normal BASEFEE-probe tx must be included");

    let gasless_receipt = provider
        .get_transaction_receipt(gasless_hash)
        .await?
        .expect("gasless tx should have a receipt");
    assert!(gasless_receipt.status(), "gasless BASEFEE-probe tx must succeed");
    let normal_receipt = provider
        .get_transaction_receipt(normal_hash)
        .await?
        .expect("normal tx should have a receipt");
    assert!(normal_receipt.status(), "normal BASEFEE-probe tx must succeed");

    // Both the gasless and the normal probe must observe the real header base fee.
    assert_probe_recorded_base_fee(&provider, 1, header_base_fee).await?;
    assert_probe_recorded_base_fee(&provider, 2, header_base_fee).await?;

    Ok(())
}

/// Same-block REVERT-path isolation. Ordered in one block: (1) a gasless BASEFEE-then-REVERT
/// tx, (2) an underpriced sentinel, (3) a valid normal BASEFEE-probe tx. Asserts the revert tx is
/// included with a failed receipt and consumes its nonce (so the normal tx uses the incremented
/// nonce), the sentinel is excluded, the normal tx is included, and the normal probe recorded the
/// real header base fee.
#[rb_test(
    args = gasless_args(),
    config = gasless_isolation_node_config()
)]
async fn gasless_same_block_revert_isolation(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let provider = driver.provider().clone();

    // nonce 0: gasless REVERT probe → included with failed receipt, consumes nonce. nonce 1:
    // sentinel (underpriced) → excluded. nonce 1: normal probe → included (uses the incremented
    // nonce, proving the revert tx consumed nonce 0).
    let (revert_tx, revert_hash) = sign_funded_call(
        /* nonce */ 0,
        /* max_fee_per_gas */ 0,
        PROBE_CALL_GAS_LIMIT,
        BASEFEE_REVERT_PROBE,
        Bytes::new(),
    );
    let (sentinel_tx, sentinel_hash) = build_underpriced_sentinel(/* nonce */ 1);
    let (normal_tx, normal_hash) =
        build_normal_store_probe_tx(/* nonce */ 1, /* slot */ 3);

    let block = build_isolation_block(
        &driver,
        vec![revert_tx.into(), sentinel_tx.into(), normal_tx.into()],
    )
    .await?;
    let header_base_fee =
        block.header.base_fee_per_gas.expect("EIP-1559 block must have a base fee");

    assert!(block.includes(&revert_hash), "gasless REVERT tx must be included");
    let revert_receipt = provider
        .get_transaction_receipt(revert_hash)
        .await?
        .expect("reverted gasless tx should still have a receipt");
    assert!(!revert_receipt.status(), "gasless REVERT tx receipt must have failed status");

    assert!(!block.includes(&sentinel_hash), "underpriced sentinel must be excluded");
    assert!(
        block.includes(&normal_hash),
        "valid normal tx (using the nonce incremented by the reverted tx) must be included"
    );

    let normal_receipt = provider
        .get_transaction_receipt(normal_hash)
        .await?
        .expect("normal tx should have a receipt");
    assert!(normal_receipt.status(), "normal BASEFEE-probe tx must succeed");
    assert_probe_recorded_base_fee(&provider, 3, header_base_fee).await?;

    Ok(())
}

/// Same-block validation-failure isolation. Ordered in one block: (1) a gasless-recognized tx
/// that fails validation on insufficient intrinsic gas (failure occurs *after* the gasless cfg
/// override is enabled), (2) an underpriced sentinel with the same nonce, (3) a valid normal
/// BASEFEE-probe tx with the same nonce. Asserts the failed gasless tx is excluded with no receipt
/// and does NOT consume its nonce (so the normal tx reuses nonce 0), the sentinel is excluded, the
/// normal tx is included, and the normal probe recorded the real header base fee.
#[rb_test(
    args = gasless_args(),
    config = gasless_isolation_node_config()
)]
async fn gasless_same_block_validation_failure_isolation(
    rbuilder: LocalInstance,
) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let provider = driver.provider().clone();

    // All three use nonce 0: the failed gasless tx must not consume the nonce, so the sentinel and
    // the normal tx are offered at the same nonce. Inclusion of the normal tx at nonce 0 proves the
    // failed tx consumed nothing.
    let (failed_tx, failed_hash) = build_gasless_store_probe_tx_with_gas(
        /* nonce */ 0,
        /* slot */ 4,
        INSUFFICIENT_GAS_LIMIT,
    );
    let (sentinel_tx, sentinel_hash) = build_underpriced_sentinel(/* nonce */ 0);
    let (normal_tx, normal_hash) =
        build_normal_store_probe_tx(/* nonce */ 0, /* slot */ 4);

    let block = build_isolation_block(
        &driver,
        vec![failed_tx.into(), sentinel_tx.into(), normal_tx.into()],
    )
    .await?;
    let header_base_fee =
        block.header.base_fee_per_gas.expect("EIP-1559 block must have a base fee");

    assert!(
        !block.includes(&failed_hash),
        "gasless tx failing intrinsic-gas validation must not be included"
    );
    assert!(
        provider.get_transaction_receipt(failed_hash).await?.is_none(),
        "validation-failed gasless tx must have no receipt"
    );
    assert!(!block.includes(&sentinel_hash), "underpriced sentinel must be excluded");
    assert!(
        block.includes(&normal_hash),
        "valid normal tx at the same nonce must be included — proves the failed gasless tx did not \
         consume the nonce"
    );

    let normal_receipt = provider
        .get_transaction_receipt(normal_hash)
        .await?
        .expect("normal tx should have a receipt");
    assert!(normal_receipt.status(), "normal BASEFEE-probe tx must succeed");
    assert_probe_recorded_base_fee(&provider, 4, header_base_fee).await?;

    Ok(())
}

/// Gasless call to [`BASEFEE_STORE_PROBE`] with an explicit `gas_limit`, for the intrinsic-gas
/// validation-failure case.
fn build_gasless_store_probe_tx_with_gas(nonce: u64, slot: u64, gas_limit: u64) -> (Vec<u8>, B256) {
    sign_funded_call(nonce, 0, gas_limit, BASEFEE_STORE_PROBE, probe_slot_calldata(slot))
}
