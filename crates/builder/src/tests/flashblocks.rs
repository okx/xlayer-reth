use alloy_consensus::Transaction;
use alloy_eips::Decodable2718;
use alloy_primitives::{Address, TxHash, U256};
use alloy_provider::Provider;
use macros::rb_test;
use op_alloy_consensus::OpTxEnvelope;
use std::time::Duration;

use crate::{
    args::{BuilderArgs, FlashblocksArgs},
    tests::{
        count_txs_to, flashblocks_number_contract::FlashblocksNumber, BlockTransactionsExt,
        ChainDriver, LocalInstance, TransactionBuilderExt, FLASHBLOCKS_NUMBER_ADDRESS,
    },
};

#[rb_test(args = BuilderArgs {
    chain_block_time: 1000,
    flashblocks: FlashblocksArgs {
        enabled: true,
        flashblocks_port: 1239,
        flashblocks_addr: "127.0.0.1".into(),
        flashblocks_block_time: 200,
        flashblocks_disable_state_root: true,
        ..Default::default()
    },
    ..Default::default()
})]
async fn test_flashblocks_no_state_root_calculation(rbuilder: LocalInstance) -> eyre::Result<()> {
    use alloy_primitives::B256;

    let driver = rbuilder.driver().await?;

    // Send a transaction to ensure block has some activity
    let _tx = driver.create_transaction().random_valid_transfer().send().await?;

    // Build a block with current timestamp (not historical) and disable_state_root: true
    let block = driver.build_new_block_with_current_timestamp(None).await?;

    // Verify that flashblocks are still produced (block should have transactions)
    assert!(block.transactions.len() > 2, "Block should contain transactions"); // deposit + builder tx + user tx

    // Verify that state root is not calculated (should be zero)
    assert_eq!(
        block.header.state_root,
        B256::ZERO,
        "State root should be zero when disable_state_root is true"
    );

    Ok(())
}

#[rb_test(args = BuilderArgs {
    chain_block_time: 1000,
    flashblocks: FlashblocksArgs {
        flashblocks_number_contract_address: Some(FLASHBLOCKS_NUMBER_ADDRESS),
        ..Default::default()
    },
    ..Default::default()
})]
async fn test_flashblocks_number_contract_builder_tx(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let flashblocks_listener = rbuilder.spawn_flashblocks_listener();
    let provider = rbuilder.provider().await?;

    // Deploy flashblocks number contract which will be in flashblocks 1
    let deploy_tx = driver.create_transaction().deploy_flashblock_number_contract().send().await?;

    // Create valid transactions
    let user_transactions = create_flashblock_transactions(&driver, 2..5).await?;

    // Build block with deploy tx in first flashblock, and a random valid transfer in every other flashblock
    let block = driver.build_new_block_with_current_timestamp(None).await?;

    // Verify contract deployment
    let receipt = provider
        .get_transaction_receipt(*deploy_tx.tx_hash())
        .await?
        .expect("flashblock number contract deployment not mined");
    let contract_address = receipt
        .inner
        .contract_address
        .expect("contract receipt does not contain flashblock number contract address");
    assert_eq!(
        contract_address, FLASHBLOCKS_NUMBER_ADDRESS,
        "Flashblocks number contract address mismatch"
    );

    // Verify first block structure
    assert_eq!(block.transactions.len(), 10);
    let txs = block.transactions.as_transactions().expect("transactions not in block");
    // Verify builder txs (should be regular since builder tx is not registered yet)
    assert_eq!(count_txs_to(txs, Address::ZERO), 5, "Should have 5 regular builder txs");
    // Verify deploy tx
    assert!(block.includes(deploy_tx.tx_hash()));

    // Verify user transactions
    assert!(block.includes(&user_transactions));

    // Initialize contract
    let init_tx = driver
        .create_transaction()
        .init_flashblock_number_contract(true)
        .with_to(FLASHBLOCKS_NUMBER_ADDRESS)
        .send()
        .await?;

    // Mine initialization
    driver.build_new_block_with_current_timestamp(None).await?;
    provider.get_transaction_receipt(*init_tx.tx_hash()).await?.expect("init tx not mined");

    // Create user transactions
    let user_transactions = create_flashblock_transactions(&driver, 1..5).await?;

    // Build second block after initialization which will call the flashblock number contract
    // with builder registered
    let block = driver.build_new_block_with_current_timestamp(None).await?;
    assert_eq!(block.transactions.len(), 10);
    assert!(block.includes(&user_transactions));
    let txs = block.transactions.as_transactions().expect("transactions not in block");
    // Fallback block should have regular builder tx after deposit tx
    assert_eq!(txs[1].to(), Some(Address::ZERO), "Fallback block should have regular builder tx");

    // Other builder txs should call the contract
    // Verify builder txs
    assert_eq!(count_txs_to(txs, contract_address), 4, "Should have 4 contract builder txs");

    // Verify flashblock number incremented correctly
    let contract = FlashblocksNumber::new(contract_address, provider.clone());
    let current_number = contract.getFlashblockNumber().call().await?;
    assert_eq!(current_number, U256::from(7), "Flashblock number not incremented correctly");

    // Verify flashblocks
    let flashblocks = flashblocks_listener.get_flashblocks();
    assert_eq!(flashblocks.len(), 15);

    // Verify builder tx in each flashblock
    for (i, flashblock) in flashblocks.iter().enumerate() {
        // In fallback blocks, builder tx is the 2nd tx (index 1)
        // In regular flashblocks, builder tx is the 1st tx (index 0)
        let is_fallback = i % 5 == 0;
        let tx_index = if is_fallback { 1 } else { 0 };

        let tx_bytes = flashblock
            .diff
            .transactions
            .get(tx_index)
            .unwrap_or_else(|| panic!("Flashblock {} should have tx at index {}", i, tx_index));
        let tx = OpTxEnvelope::decode_2718(&mut tx_bytes.as_ref())
            .expect("failed to decode transaction");

        let expected_to =
            if i < 7 || i == 10 { Some(Address::ZERO) } else { Some(contract_address) };

        assert_eq!(
            tx.to(),
            expected_to,
            "Flashblock {} builder tx (at index {}) should have to = {:?}",
            i,
            tx_index,
            expected_to
        );
    }

    flashblocks_listener.stop().await?;
    Ok(())
}

// Helper to create transactions for flashblocks
async fn create_flashblock_transactions(
    driver: &ChainDriver,
    range: std::ops::Range<u64>,
) -> eyre::Result<Vec<TxHash>> {
    let mut txs = Vec::new();
    for _ in range {
        let tx = driver.create_transaction().random_valid_transfer().send().await?;
        txs.push(*tx.tx_hash());
    }
    Ok(txs)
}

/// Smoke test for flashblocks with end buffer.
#[rb_test(args = BuilderArgs {
    chain_block_time: 1000,
    flashblocks: FlashblocksArgs {
        enabled: true,
        flashblocks_port: 1239,
        flashblocks_addr: "127.0.0.1".into(),
        flashblocks_block_time: 250,
        flashblocks_end_buffer_ms: 50,
        ..Default::default()
    },
    ..Default::default()
})]
async fn smoke_basic(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let flashblocks_listener = rbuilder.spawn_flashblocks_listener();

    for _ in 0..5 {
        for _ in 0..3 {
            let _ = driver.create_transaction().random_valid_transfer().send().await?;
        }
        let block = driver.build_new_block_with_current_timestamp(None).await?;
        assert_eq!(block.transactions.len(), 6); // 3 normal txn + deposit + 2 builder txn
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    let flashblocks = flashblocks_listener.get_flashblocks();
    // Expected: ~5 flashblocks per block (1000ms / 250ms interval, with end_buffer_ms applied).
    // Allow ±1 per block to tolerate timing variance on CI runners.
    let count = flashblocks.len();
    assert!((20..=30).contains(&count), "Expected ~25 flashblocks (20–30), got {count}");

    flashblocks_listener.stop().await
}

/// Smoke test with send_offset_ms
#[rb_test(args = BuilderArgs {
    chain_block_time: 1000,
    flashblocks: FlashblocksArgs {
        enabled: true,
        flashblocks_port: 1239,
        flashblocks_addr: "127.0.0.1".into(),
        flashblocks_block_time: 250,
        flashblocks_end_buffer_ms: 50,
        flashblocks_send_offset_ms: -25, // Send 25ms earlier
        ..Default::default()
    },
    ..Default::default()
})]
async fn smoke_with_offset(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver: ChainDriver = rbuilder.driver().await?;
    let flashblocks_listener = rbuilder.spawn_flashblocks_listener();

    for _ in 0..5 {
        for _ in 0..3 {
            let _ = driver.create_transaction().random_valid_transfer().send().await?;
        }
        let block = driver.build_new_block_with_current_timestamp(None).await?;
        assert_eq!(block.transactions.len(), 6);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    let flashblocks = flashblocks_listener.get_flashblocks();
    // Offset should still produce expected flashblock count
    assert_eq!(25, flashblocks.len(), "Expected 25 flashblocks, got {:#?}", flashblocks.len());

    flashblocks_listener.stop().await
}

/// Test significant FCU delay (700ms into 1000ms block)
/// Should produce fewer flashblocks due to less remaining time
#[rb_test(args = BuilderArgs {
    chain_block_time: 1000,
    flashblocks: FlashblocksArgs {
        enabled: true,
        flashblocks_port: 1239,
        flashblocks_addr: "127.0.0.1".into(),
        flashblocks_block_time: 200,
        flashblocks_end_buffer_ms: 50,
        ..Default::default()
    },
    ..Default::default()
})]
async fn late_fcu_reduces_flashblocks(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let flashblocks_listener = rbuilder.spawn_flashblocks_listener();

    // Send a transaction
    let _ = driver.create_transaction().random_valid_transfer().send().await?;

    // Build block with 700ms delay - only 300ms remaining for a 200ms interval
    // Should produce only 2 flashblocks
    let block =
        driver.build_new_block_with_current_timestamp(Some(Duration::from_millis(700))).await?;

    assert!(block.transactions.len() >= 2); // deposit + at least builder tx

    let flashblocks = flashblocks_listener.get_flashblocks();

    // With only ~300ms remaining and 200ms interval, we should get 2 + 1 (fallback) flashblocks
    assert_eq!(
        3,
        flashblocks.len(),
        "Expected 3 flashblocks with 700ms FCU delay, got {:#?}",
        flashblocks.len()
    );

    flashblocks_listener.stop().await
}

/// Test progressive FCU delays across multiple blocks.
/// With 1000ms block time, 200ms flashblock interval, and 50ms end buffer:
/// - Available time = 1000 - lag - 50 = 950 - lag
/// - Flashblocks per block = ceil((available_time) / 200) + 1 (base flashblock)
#[rb_test(args = BuilderArgs {
      chain_block_time: 1000,
      flashblocks: FlashblocksArgs {
          enabled: true,
          flashblocks_port: 1239,
          flashblocks_addr: "127.0.0.1".into(),
          flashblocks_block_time: 200,
          flashblocks_end_buffer_ms: 50,
          ..Default::default()
      },
      ..Default::default()
  })]
async fn progressive_lag_reduces_flashblocks(rbuilder: LocalInstance) -> eyre::Result<()> {
    let driver = rbuilder.driver().await?;
    let flashblocks_listener = rbuilder.spawn_flashblocks_listener();

    // Test 9 blocks with increasing FCU delays (0ms, 100ms, ..., 800ms)
    for i in 0..9 {
        for _ in 0..5 {
            let _ = driver.create_transaction().random_valid_transfer().send().await?;
        }
        let block = driver
            .build_new_block_with_current_timestamp(Some(Duration::from_millis(i * 100)))
            .await?;
        assert_eq!(block.transactions.len(), 8, "Got: {:#?}", block.transactions); // 5 normal txn + deposit + 2 builder txn

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    let flashblocks = flashblocks_listener.get_flashblocks();

    // Count flashblocks for each block
    // Expected flashblocks per block based on lag:
    // lag=0ms:   ceil((1000-50)/200) + 1 = 6
    // lag=100ms: ceil((900-50)/200) + 1 = 6
    // lag=200ms: ceil((800-50)/200) + 1 = 5
    // lag=300ms: ceil((700-50)/200) + 1 = 5
    // lag=400ms: ceil((600-50)/200) + 1 = 4
    // lag=500ms: ceil((500-50)/200) + 1 = 4
    // lag=600ms: ceil((400-50)/200) + 1 = 3
    // lag=700ms: ceil((300-50)/200) + 1 = 3
    // lag=800ms: ceil((200-50)/200) + 1 = 2
    let expected_flashblocks_per_block = [6, 6, 5, 5, 4, 4, 3, 3, 2];
    for i in 0..9 {
        let block_number = i + 1; // Block numbers start from 1
        let flashblocks_for_block =
            flashblocks.iter().filter(|fb| fb.block_number() == block_number).count();
        assert_eq!(
            flashblocks_for_block,
            expected_flashblocks_per_block[i as usize],
            "Block {} (lag {}ms): expected {} flashblocks, got {}",
            i,
            i * 100,
            expected_flashblocks_per_block[i as usize],
            flashblocks_for_block
        );
    }

    // Total: 6+6+5+5+4+4+3+3+2 = 38
    assert_eq!(38, flashblocks.len());

    flashblocks_listener.stop().await
}

/// Validates the EIP-7928 BAL produced via the upstream `State::with_bal_builder()`
/// path is wire-correct end-to-end. Combines two invariants in one e2e run:
///
/// * **EIP-7928 index assignment**: pre-exec records at `block_access_index = 0`,
///   tx K records at `K + 1`. The base flashblock (index=0) carries the pre-exec
///   entry at `bal_index=0` and the L1-info system tx at `bal_index=1`.
/// * **Per-flashblock incrementality**: subsequent flashblocks (index>=1) carry
///   ONLY their own tx range — no pre-exec leak from FB 0 (caught by the
///   `take_built_alloy_bal()` + `bal_builder = Some(Bal::new())` re-arm pair),
///   and no overlap of `bal_index` across FBs (i.e., FB N max < FB N+1 min).
#[rb_test(args = BuilderArgs {
    chain_block_time: 1000,
    flashblocks: FlashblocksArgs {
        enabled: true,
        flashblocks_port: 1242,
        flashblocks_addr: "127.0.0.1".into(),
        flashblocks_block_time: 200,
        flashblocks_disable_state_root: true,
        ..Default::default()
    },
    ..Default::default()
})]
async fn test_bal_eip7928_per_flashblock_invariants(rbuilder: LocalInstance) -> eyre::Result<()> {
    use alloy_eip7928::BlockAccessList;
    use alloy_rlp::Decodable;
    use std::collections::BTreeSet;

    let driver = rbuilder.driver().await?;
    let flashblocks_listener = rbuilder.spawn_flashblocks_listener();

    // Send enough txs that the block produces multiple flashblocks (need
    // ≥2 FBs to assert non-trivial incrementality).
    for _ in 0..6 {
        let _ = driver.create_transaction().random_valid_transfer().send().await?;
    }
    let _block = driver.build_new_block_with_current_timestamp(None).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let flashblocks = flashblocks_listener.get_flashblocks();
    assert!(flashblocks.len() >= 2, "expected at least 2 flashblocks; got {}", flashblocks.len());

    // For each flashblock, decode its wire BAL and collect every
    // `block_access_index` referenced.
    fn collect_indices(bal: &BlockAccessList) -> BTreeSet<u64> {
        let mut set = BTreeSet::new();
        for account in bal {
            for c in &account.balance_changes {
                set.insert(c.block_access_index);
            }
            for c in &account.nonce_changes {
                set.insert(c.block_access_index);
            }
            for c in &account.code_changes {
                set.insert(c.block_access_index);
            }
            for sc in &account.storage_changes {
                for c in &sc.changes {
                    set.insert(c.block_access_index);
                }
            }
        }
        set
    }

    let mut per_fb: Vec<(u64, BTreeSet<u64>)> = Vec::new();
    for fb in &flashblocks {
        let Some(raw_bal) = fb.metadata.access_list.as_ref() else { continue };
        let mut buf: &[u8] = raw_bal.as_ref();
        let bal = BlockAccessList::decode(&mut buf).expect("BAL must decode");
        per_fb.push((fb.index, collect_indices(&bal)));
    }
    assert!(!per_fb.is_empty(), "expected at least one flashblock to carry a BAL");

    // EIP-7928 invariant: base FB carries pre-exec at bal_index=0 AND tx 0
    // (L1-info attribute deposit) at bal_index=1.
    let fb0 = per_fb
        .iter()
        .find(|(idx, _)| *idx == 0)
        .map(|(_, set)| set)
        .expect("base flashblock must carry a BAL");
    assert!(fb0.contains(&0), "FB 0 must carry pre-exec entry at bal_index=0; got {fb0:?}");
    assert!(fb0.contains(&1), "FB 0 must carry the L1-info system tx at bal_index=1; got {fb0:?}",);

    // Per-FB incrementality: only the base FB may carry bal_index=0. If a
    // subsequent FB does, `take_built_alloy_bal() + re-arm` leaked.
    for (fb_idx, indices) in &per_fb {
        if *fb_idx == 0 {
            continue;
        }
        assert!(
            !indices.contains(&0),
            "FB {fb_idx} leaked pre-exec entries at bal_index=0: {indices:?}",
        );
    }

    // Global monotonicity: across FBs that carry indices, FB N's max is
    // strictly less than FB N+1's min. Catches any per-FB cumulative leak
    // beyond just the pre-exec slot.
    let mut nonempty: Vec<&(u64, BTreeSet<u64>)> =
        per_fb.iter().filter(|(_, s)| !s.is_empty()).collect();
    nonempty.sort_by_key(|(idx, _)| *idx);
    for w in nonempty.windows(2) {
        let (a_fb, a_set) = w[0];
        let (b_fb, b_set) = w[1];
        let a_max = *a_set.iter().next_back().expect("non-empty");
        let b_min = *b_set.iter().next().expect("non-empty");
        assert!(
            a_max < b_min,
            "FB {a_fb} (max bal_index={a_max}) and FB {b_fb} (min bal_index={b_min}) overlap",
        );
    }

    flashblocks_listener.stop().await
}
