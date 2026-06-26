//! In-process builder tests.

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
use std::{sync::Arc, time::Duration};

// ############################################################################
// # EIP-2929 warm/cold access-list leak across a dropped tx
// ############################################################################
//
// The Base's bug under test: while building a block the flashblocks builder executes a transaction's body
// (warming storage slots / accounts via EIP-2929), then DROPS it without resetting the EVM journal
// / access-warmness. The next transaction that touches the same slot is then charged a *warm*
// (cheaper) SLOAD on the sequencer, while a validator replaying the sealed block (which never saw
// the dropped tx) charges a *cold* SLOAD — divergent gas, a State-Mismatch-Block.
//
// Reproduction shape, all in one block in order A → B → C:
// - **A** is executed (its body SLOADs `PROBE` slot 0, warming it) but then dropped because its
//   `gas_used` exceeds `max_gas_per_txn` (its calldata is inflated so the intrinsic cost blows the
//   per-tx cap). `max_gas_per_txn` (context.rs:718) is the one "execute-then-drop" path with no
//   skip-without-simulate guard, so the next tx still runs on the same EVM.
// - **B** reads `PROBE` slot 0 right after the dropped A — warm if the journal leaked, else cold.
// - **C** reads `PROBE` slot 0 after B committed (its own fresh access list) — the cold baseline.
//
// With the journal correctly reset on drop, B and C cost the same. If the warmth leaks, B is
// charged a warm SLOAD and comes out ~2000 gas cheaper than C.

// Root cause is that they use inspect_tx instead of transact, which leave dirty journal states for failed tx.

/// Address the `PROBE` contract is deployed at in genesis.
const PROBE: Address = address!("00000000000000000000000000000000000c0de0");

/// `PROBE` runtime bytecode: `PUSH1 0x00; SLOAD; STOP`. A call warms `(PROBE, slot 0)` via the
/// SLOAD and otherwise does nothing — the minimal observable warm/cold target.
const PROBE_BYTECODE: [u8; 4] = [0x60, 0x00, 0x54, 0x00];

/// Per-tx gas cap. Picked so the setup/B/C txs fit comfortably under it while A (with inflated
/// calldata) blows past it and is executed-then-dropped.
const MAX_GAS_PER_TXN: u64 = 100_000;

/// `A`'s calldata size. At 16 gas/byte (non-zero) the intrinsic cost alone (~128k) exceeds
/// [`MAX_GAS_PER_TXN`], so A is dropped after its body runs (and warms `PROBE` slot 0).
const A_CALLDATA_LEN: usize = 8_000;

/// Sender of the failing tx A. A *separate* account from B/C: A is dropped, which would nonce-gap
/// B/C if it shared their account.
static A_SIGNER: std::sync::LazyLock<crate::signer::Signer> =
    std::sync::LazyLock::new(crate::signer::Signer::random);

/// Node config: template genesis with `PROBE` deployed and [`A_SIGNER`] funded.
fn probe_node_config() -> NodeConfig<OpChainSpec> {
    let genesis_json = include_str!("./framework/artifacts/genesis.json.tmpl");
    let mut genesis: Genesis =
        serde_json::from_str(genesis_json).expect("invalid genesis template JSON");

    genesis.alloc.insert(
        PROBE,
        GenesisAccount {
            code: Some(Bytes::copy_from_slice(&PROBE_BYTECODE)),
            ..Default::default()
        },
    );
    genesis.alloc.insert(
        A_SIGNER.address,
        GenesisAccount { balance: U256::from(1_000_000_000_000_000_000u128), ..Default::default() },
    );

    let chain_spec = OpChainSpec::from_genesis(genesis);
    default_node_config().with_chain(Arc::new(chain_spec))
}

/// Builder args with the per-tx gas cap that triggers A's execute-then-drop.
fn probe_args() -> BuilderArgs {
    BuilderArgs {
        builder_signer: Some(builder_signer()),
        max_gas_per_txn: Some(MAX_GAS_PER_TXN),
        ..Default::default()
    }
}

/// Chain id of the template genesis.
const CHAIN_ID: u64 = 901;

/// Signs an EIP-1559 call to [`PROBE`] from `signer` with the given `nonce`, `priority`, `gas_limit`
/// and `input`, returning `(encoded, tx_hash)`. `max_fee_per_gas` is set high enough to clear any
/// base fee (the builder zeroes nothing here — these are ordinary paid txs).
fn signed_probe_call(
    signer: &crate::signer::Signer,
    nonce: u64,
    priority: u128,
    gas_limit: u64,
    input: Bytes,
) -> (Vec<u8>, B256) {
    let tx = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: CHAIN_ID,
        nonce,
        gas_limit,
        max_fee_per_gas: priority + 1_000_000_000,
        max_priority_fee_per_gas: priority,
        to: TxKind::Call(PROBE),
        input,
        ..Default::default()
    });
    let signed = signer.sign_tx(tx).expect("failed to sign probe call");
    let tx_hash = B256::from_slice(signed.tx_hash().as_ref());
    (signed.encoded_2718(), tx_hash)
}

/// Builds a single block from the pool, one second after the latest block.
async fn build_block_from_pool(
    driver: &crate::tests::ChainDriver,
) -> eyre::Result<alloy_rpc_types_eth::Block<op_alloy_rpc_types::Transaction>> {
    let latest = driver.get_block(Latest).await?.expect("latest block must exist");
    let block_timestamp = Duration::from_secs(latest.header.timestamp) + Duration::from_secs(1);
    driver
        .build_new_block_with_txs_timestamp(vec![], None, Some(block_timestamp), None, Some(0))
        .await
}

/// Regression: a tx (A) that is executed then dropped (over `max_gas_per_txn`) must not leak its
/// EIP-2929 warmth into the next executed tx (B). B and the cold baseline C must cost the same.
#[rb_test(args = probe_args(), config = probe_node_config())]
async fn warm_cold_leak_across_dropped_tx(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let provider = driver.provider().clone();

    let funded = funded_signer();
    let b_nonce =
        provider.get_transaction_count(funded.address).pending().await.unwrap_or_default();

    // A: to PROBE with inflated calldata → body runs (SLOADs PROBE slot 0, warming it) but gas_used
    // exceeds MAX_GAS_PER_TXN → executed-then-dropped. Higher priority so it is pulled (and dropped)
    // right before B.
    let (a_encoded, a_hash) = signed_probe_call(
        &A_SIGNER,
        0,
        2_000_000,
        1_000_000,
        Bytes::from(vec![0x11u8; A_CALLDATA_LEN]),
    );
    // B: reads PROBE slot 0 immediately after the dropped A.
    let (b_encoded, b_hash) =
        signed_probe_call(&funded, b_nonce, 1_000_000, MAX_GAS_PER_TXN, Bytes::new());
    // C: reads PROBE slot 0 after B commits — the in-block cold baseline.
    let (c_encoded, c_hash) =
        signed_probe_call(&funded, b_nonce + 1, 1_000_000, MAX_GAS_PER_TXN, Bytes::new());

    // Submit in order A, B, C (equal-priority txs pop FIFO; B before C by nonce).
    let _ = provider.send_raw_transaction(a_encoded.as_slice()).await?;
    let _ = provider.send_raw_transaction(b_encoded.as_slice()).await?;
    let _ = provider.send_raw_transaction(c_encoded.as_slice()).await?;

    let block = build_block_from_pool(&driver).await?;
    assert!(!block.includes(&a_hash), "A must be dropped (exceeds max_gas_per_txn), not included");
    assert!(block.includes(&b_hash), "B must be included");
    assert!(block.includes(&c_hash), "C must be included");

    let b_gas = provider
        .get_transaction_receipt(b_hash)
        .await?
        .expect("B should have a receipt")
        .gas_used();
    let c_gas = provider
        .get_transaction_receipt(c_hash)
        .await?
        .expect("C should have a receipt")
        .gas_used();

    println!("warm/cold: B(after dropped A) gas_used={b_gas}, C(cold baseline) gas_used={c_gas}");
    println!("           B - C = {}", b_gas as i64 - c_gas as i64);

    assert_eq!(
        b_gas, c_gas,
        "B (right after the dropped A) and the cold baseline C must use the same gas; \
         B < C means A's warm SLOAD of PROBE slot 0 leaked across the drop into B"
    );

    Ok(())
}
