use super::*;
use crate::flashblocks::rcs_capture::{FORCE_CREATE_ADDRESS_MISMATCH, FORCE_REALLOG_MISMATCH};
use alloy_consensus::TxEip1559;
use alloy_genesis::Genesis;
use alloy_primitives::{keccak256, Address, TxKind};
use alloy_rpc_types_engine::PayloadAttributes;
use op_alloy_consensus::OpTypedTransaction;
use op_alloy_rpc_types_engine::OpPayloadAttributes;
use rcs_filter::rules::{load_rules, RawRule};
use rcs_filter::test_support::golden;
use rcs_filter::{BufferStatus, FilterConfig, SystemClock};
use reth_optimism_txpool::OpPooledTransaction;
use reth_payload_util::PayloadTransactions;
use reth_transaction_pool::test_utils::{testing_pool, MockTransaction};
use reth_transaction_pool::TransactionOrigin;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::{CacheDB, EmptyDB};
use revm::state::{AccountInfo, Bytecode};
// Bring revm's `Database` trait into scope (anonymously, to avoid shadowing the `alloy_evm::Database`
// already in scope) so `state.basic(..)` resolves for the P1.3 sender nonce/balance reads.
use revm::Database as _;

/// Panic-safe RAII guard for the `FORCE_REALLOG_MISMATCH` test seam (spec §5.10.3 P2): arms the
/// flag on construction and RESETS it on `Drop` — on success AND on a panic/unwind — so it can
/// never leak into another test running later on the same thread.
struct ForceMismatchGuard;
impl ForceMismatchGuard {
    fn arm() -> Self {
        FORCE_REALLOG_MISMATCH.with(|f| f.set(true));
        Self
    }
}
impl Drop for ForceMismatchGuard {
    fn drop(&mut self) {
        FORCE_REALLOG_MISMATCH.with(|f| f.set(false));
    }
}

/// A one-shot [`PayloadTransactions`] that yields a single candidate and RECORDS every
/// `mark_invalid(sender, nonce)` call. `PayloadTransactionsFixed::mark_invalid` is a silent
/// no-op; this wrapper makes the loop's post-decision `best_txs.mark_invalid` step observable as
/// SUPPORTING evidence (the Deny-vs-Drop decision itself is asserted via [`LAST_SCREEN_DECISION`]).
struct RecordingTxs {
    tx: Option<OpPooledTransaction>,
    marked: Vec<(Address, u64)>,
}
impl PayloadTransactions for RecordingTxs {
    type Transaction = OpPooledTransaction;
    fn next(&mut self, _ctx: ()) -> Option<OpPooledTransaction> {
        self.tx.take()
    }
    fn mark_invalid(&mut self, sender: Address, nonce: u64) {
        self.marked.push((sender, nonce));
    }
}

/// Runtime bytecode that emits ONE real ERC20
/// `Transfer(from = BRIDGE_ERC20, to = RECIPIENT, value = ONE_TOKEN)` log then `STOP`s. Deployed at
/// `TOKEN_X`, so `log.address == TOKEN_X` and the log matches `RULE_SCENARIO_A`
/// (`transfer.address == TOKEN_X && transfer.from == BRIDGE_ERC20`). `LOG3` pops
/// `offset, size, topic0, topic1, topic2` (offset on top), so the topics are pushed in reverse.
fn erc20_transfer_emitter_code() -> Vec<u8> {
    let transfer_sig = keccak256("Transfer(address,address,uint256)"); // topic0
    let from_word = golden::bridge_erc20().into_word(); // topic1 (from)
    let to_word = golden::recipient().into_word(); // topic2 (to)
    let value_bytes = golden::one_token().to_be_bytes::<32>();
    let mut code = Vec::new();
    code.push(0x7f); // PUSH32 value
    code.extend_from_slice(&value_bytes);
    code.extend_from_slice(&[0x60, 0x00, 0x52]); // PUSH1 0x00; MSTORE  → mem[0..32] = value
    code.push(0x7f); // PUSH32 topic2 (to)
    code.extend_from_slice(to_word.as_slice());
    code.push(0x7f); // PUSH32 topic1 (from)
    code.extend_from_slice(from_word.as_slice());
    code.push(0x7f); // PUSH32 topic0 (Transfer signature)
    code.extend_from_slice(transfer_sig.as_slice());
    code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0xa3, 0x00]); // PUSH1 32; PUSH1 0; LOG3; STOP
    code
}

/// Builds a real [`FlashblocksBuilderCtx`] with the RCS Filter enabled by `filter`: a JOVIAN EVM
/// env at base fee 0 (hand-built, so no parent-header/fork coordination is needed) over a chain
/// spec from the shared test genesis template pinned to chain id 1.
fn build_ctx(filter: Arc<FilterHandle>) -> FlashblocksBuilderCtx {
    let mut genesis: Genesis =
        serde_json::from_str(include_str!("framework/artifacts/genesis.json.tmpl"))
            .expect("valid genesis template JSON");
    genesis.config.chain_id = 1;
    genesis.base_fee_per_gas = Some(0u64.into());
    let chain_spec = Arc::new(OpChainSpec::from_genesis(genesis));
    let evm_config = OpEvmConfig::optimism(chain_spec.clone());
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.chain_id = 1;
    let evm_env = EvmEnv::new(cfg, BlockEnv { gas_limit: 30_000_000, ..Default::default() });
    let payload_id = PayloadId::new([0x35; 8]);
    let rpc_attrs = OpPayloadAttributes {
        payload_attributes: PayloadAttributes { timestamp: 1_751_000_002, ..Default::default() },
        gas_limit: Some(30_000_000),
        ..Default::default()
    };
    let attributes = OpPayloadBuilderAttributes::from_rpc_attrs(B256::ZERO, payload_id, rpc_attrs)
        .expect("attrs decode");
    let config = PayloadConfig {
        parent_header: Arc::new(SealedHeader::seal_slow(alloy_consensus::Header::default())),
        parent_block_info: None,
        payload_id,
        attributes,
    };
    let block_env_attributes = OpNextBlockEnvAttributes {
        timestamp: 1_751_000_002,
        suggested_fee_recipient: Address::ZERO,
        prev_randao: B256::ZERO,
        gas_limit: 30_000_000,
        parent_beacon_block_root: None,
        extra_data: Bytes::new(),
    };
    FlashblocksBuilderCtx {
        evm_config,
        da_config: OpDAConfig::default(),
        gas_limit_config: OpGasLimitConfig::default(),
        chain_spec,
        config,
        evm_env,
        block_env_attributes,
        cancel: CancellationToken::new(),
        builder_signer: None,
        metrics: Arc::new(BuilderMetrics::default()),
        max_gas_per_txn: None,
        bridge_intercept_config: Default::default(),
        gasless_contract: None,
        gasless_block_gas_limit: None,
        filter: Some(Arc::clone(&filter)),
        // The test context pins the snapshot exactly as the real block payload job does.
        rcs_rule_snapshot: Some(filter.rule_snapshot()),
    }
}

#[tokio::test]
async fn loop_level_real_reallogmismatch_denies_no_buffer_no_commit_no_receipt() {
    // Non-empty RuleSet: RULE_SCENARIO_A audits any tx whose captured stream contains an ERC20
    // Transfer with `log.address == TOKEN_X` and `from == BRIDGE_ERC20` (origin/tx_to are null in
    // the rule → not constrained).
    let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()]);
    let filter =
        Arc::new(FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock)));
    let ctx = build_ctx(filter.clone());

    // (P1.1, option a) The candidate CALLS TOKEN_X, whose installed code emits a REAL ERC20
    // Transfer log matching RULE_SCENARIO_A — so a normal (no-mismatch) run screens to
    // AuditPending and buffers (proven by the CONTROL run at the end). This is what makes the
    // fail-closed buffer-absence meaningful: it shows `screen_tx` was SKIPPED, not that the
    // candidate could never buffer.
    let signer = crate::tests::funded_signer();
    let candidate = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 200_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(golden::token_x()),
        value: U256::ZERO,
        ..Default::default()
    });
    let recovered = signer.sign_tx(candidate).expect("sign candidate");
    let candidate_hash: B256 = recovered.tx_hash();
    let encoded_len = recovered.encode_2718_len();
    let pooled = OpPooledTransaction::new(recovered, encoded_len);

    // Funded sender + the ERC20 Transfer-emitting contract installed at TOKEN_X.
    let mut cache = CacheDB::new(EmptyDB::default());
    cache.insert_account_info(
        signer.address,
        AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
    );
    cache.insert_account_info(
        golden::token_x(),
        AccountInfo {
            code: Some(Bytecode::new_raw(Bytes::from(erc20_transfer_emitter_code()))),
            ..Default::default()
        },
    );
    let mut state = State::builder().with_database(cache).with_bundle_update().build();

    // The txpool holds the candidate so the Deny arm's `tx_pool.remove_transaction` is a real,
    // observable pool-state change (SUPPORTING evidence — not the Deny signal; see P1.2).
    let pool = testing_pool();
    let mock = MockTransaction::legacy()
        .with_sender(signer.address)
        .with_nonce(0)
        .with_gas_price(100)
        .with_hash(candidate_hash);
    pool.add_transaction(TransactionOrigin::External, mock).await.unwrap();
    assert!(pool.get(&candidate_hash).is_some(), "candidate is in the txpool before the loop");

    let limits =
        TransactionLimits { block_gas: 30_000_000, block_da: None, block_da_footprint: None };

    // (P1.3) sender nonce/balance BEFORE the fail-closed loop.
    let before = state.basic(signer.address).expect("db read").expect("sender funded");
    let (nonce_before, balance_before) = (before.nonce, before.balance);

    // ===== FAIL-CLOSED RUN =====
    // (P2) arm the seam through a panic-safe RAII guard so a panic mid-run cannot leak the flag
    // into another test on this thread; the guard's `Drop` resets it at end of scope.
    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs = RecordingTxs { tx: Some(pooled.clone()), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let result = {
        let _guard = ForceMismatchGuard::arm();
        let r = ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
        // Checked INSIDE the guarded scope, BEFORE the guard's Drop also clears the flag: the loop's
        // own `finish_capture` consumed the one-shot seam (`replace(false)`), so it is already false
        // here. This is falsifiable — if the candidate never reached `finish_capture` (e.g. skipped
        // before the screen decision) the flag would still be set. (The guard additionally guarantees
        // the flag is reset on any panic/unwind, so it cannot leak to another test on this thread.)
        assert!(
            !FORCE_REALLOG_MISMATCH.with(|f| f.get()),
            "the loop's finish_capture must have consumed the one-shot mismatch seam"
        );
        r
    };

    assert!(matches!(result, Ok(None)), "loop completes without cancellation/error: {result:?}");

    // (P1.2) Screen::Deny observed DIRECTLY via the decision seam — a Deny-only signal. FAILS
    // under M2: a loop returning Screen::Drop would be captured here as Drop. Deny is NEVER
    // inferred from mark_invalid/txpool removal, which Screen::Drop shares.
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::Deny),
        "the real RealLogMismatch must fail closed to Screen::Deny (not Drop) on the candidate loop"
    );

    // Supporting eviction evidence (NOT the Deny signal — Screen::Drop shares these).
    assert!(
        best_txs.marked.contains(&(signer.address, 0)),
        "the fail-closed candidate is mark_invalid'd by the loop"
    );
    assert!(
        pool.get(&candidate_hash).is_none(),
        "the fail-closed candidate is removed from the txpool"
    );

    // (retained) no execution / no receipt / zero gas / no virtual native event on this path: the
    // `continue` fires before `info.executed_transactions`/`info.receipts`/`evm.db_mut().commit`.
    assert!(info.executed_transactions.is_empty(), "fail-closed: no tx appended to the block");
    assert!(
        info.receipts.is_empty(),
        "fail-closed: no receipt pushed (commit skipped) — no virtual native event on any output"
    );
    assert_eq!(info.cumulative_gas_used, 0, "fail-closed: no gas accounted (never committed)");

    // (P1.3) no state commit observed DIRECTLY: sender nonce/balance unchanged across the loop.
    // FAILS under M3: a `commit` moved before the `continue` would bump the nonce and spend gas.
    let after = state.basic(signer.address).expect("db read").expect("sender present");
    assert_eq!(
        after.nonce, nonce_before,
        "fail-closed: sender nonce must be unchanged (no commit)"
    );
    assert_eq!(
        after.balance, balance_before,
        "fail-closed: sender balance must be unchanged (no commit)"
    );

    // (P1.1) the tx never enters the RCS pending buffer: fail-closed SKIPS `screen_tx` (the only
    // AuditPending -> BufferPool insert path), so the buffer is not increased and the candidate
    // has no entry. Meaningful because the CONTROL below proves this very candidate DOES buffer.
    assert_eq!(
        filter.buffered_len(),
        0,
        "fail-closed must not add a BufferPool entry (screen_tx skipped)"
    );
    assert!(
        filter.buffer_status(&candidate_hash).is_none(),
        "no BufferEntry may exist for the failed-closed candidate"
    );

    // ===== CONTROL RUN (no forced mismatch) — proves the SAME candidate reaches AuditPending
    // (P1.1 / M1). With no guard, the loop's finish_capture returns Complete([Transfer]) and
    // screen_tx matches RULE_SCENARIO_A → AuditPending → a BufferEntry appears. If the fail-closed
    // path had wrongly reached screen_tx (M1), the buffer-absence assertion above would have failed.
    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs_ctrl = RecordingTxs { tx: Some(pooled), marked: Vec::new() };
    let mut info_ctrl = ExecutionInfo::with_capacity(1);
    let control = ctx.execute_best_transactions(
        &mut info_ctrl,
        &mut state,
        &mut best_txs_ctrl,
        &pool,
        limits,
    );
    assert!(matches!(control, Ok(None)), "control loop completes: {control:?}");
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::AuditPending),
        "control: the SAME candidate, run without a forced mismatch, screens to AuditPending"
    );
    assert_eq!(
        filter.buffer_status(&candidate_hash),
        Some(BufferStatus::NotSubmitted),
        "control: the candidate's normal run inserts a BufferPool entry"
    );
    assert_eq!(filter.buffered_len(), 1, "control: the candidate now occupies the pending buffer");
}

/// Panic-safe RAII guard for the `FORCE_CREATE_ADDRESS_MISMATCH` test seam: arms the flag on
/// construction and RESETS it on `Drop` — on success AND on a panic/unwind — so it cannot leak
/// into another test running later on this thread.
struct ForceCreateAddressMismatchGuard;
impl ForceCreateAddressMismatchGuard {
    fn arm() -> Self {
        FORCE_CREATE_ADDRESS_MISMATCH.with(|f| f.set(true));
        Self
    }
}
impl Drop for ForceCreateAddressMismatchGuard {
    fn drop(&mut self) {
        FORCE_CREATE_ADDRESS_MISMATCH.with(|f| f.set(false));
    }
}

/// Runtime bytecode that performs one endowment-bearing `CREATE` and then emits a real ERC20
/// `Transfer` log matching `RULE_SCENARIO_A`. Installed at `TOKEN_X`, so the emitted log carries
/// `log.address == TOKEN_X` and the CONTROL run of the candidate genuinely buffers.
fn create_then_erc20_transfer_emitter_code() -> Vec<u8> {
    // CREATE(value=1, offset=0, size=0): PUSH1 0 (size), PUSH1 0 (offset), PUSH1 1 (value), CREATE, POP
    let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0xf0, 0x50];
    code.extend_from_slice(&erc20_transfer_emitter_code());
    code
}

/// The `CreateAddressMismatch` fail-closed chain, driven through the ACTUAL candidate loop
/// rather than a hand-built `CaptureOutcome`: the loop's own `RcsInspector::create_end` sees a
/// genuinely successful CREATE whose deployed address differs from the pre-derived recipient,
/// `finish_capture` surfaces `InvariantViolation { CreateAddressMismatch }`, and
/// `fail_closed_screen` must turn that into a Deny WITHOUT reaching `screen_tx`.
///
/// Falsifiable in both directions: removing the `CreateAddressMismatch` arm from
/// `fail_closed_screen` fails to compile (the match is exhaustive), and routing the outcome to
/// any non-Deny decision makes the `LAST_SCREEN_DECISION` assertion below fail.
#[tokio::test]
async fn loop_level_create_address_mismatch_denies_no_buffer_no_commit_no_receipt() {
    let rules = load_rules(1, 1, vec![serde_json::from_str(golden::RULE_SCENARIO_A).unwrap()]);
    let filter =
        Arc::new(FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock)));
    let ctx = build_ctx(filter.clone());

    let signer = crate::tests::funded_signer();
    let candidate = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 200_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(golden::token_x()),
        value: U256::ZERO,
        ..Default::default()
    });
    let recovered = signer.sign_tx(candidate).expect("sign candidate");
    let candidate_hash: B256 = recovered.tx_hash();
    let encoded_len = recovered.encode_2718_len();
    let pooled = OpPooledTransaction::new(recovered, encoded_len);

    // Funded sender + a factory at TOKEN_X that endows a CREATE, then emits the scenario log.
    // The factory must hold at least the 1 wei it endows.
    let mut cache = CacheDB::new(EmptyDB::default());
    cache.insert_account_info(
        signer.address,
        AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
    );
    cache.insert_account_info(
        golden::token_x(),
        AccountInfo {
            balance: U256::from(1_000u64),
            code: Some(Bytecode::new_raw(Bytes::from(create_then_erc20_transfer_emitter_code()))),
            ..Default::default()
        },
    );
    let mut state = State::builder().with_database(cache).with_bundle_update().build();

    let pool = testing_pool();
    let mock = MockTransaction::legacy()
        .with_sender(signer.address)
        .with_nonce(0)
        .with_gas_price(100)
        .with_hash(candidate_hash);
    pool.add_transaction(TransactionOrigin::External, mock).await.unwrap();
    assert!(pool.get(&candidate_hash).is_some(), "candidate is in the txpool before the loop");

    let limits =
        TransactionLimits { block_gas: 30_000_000, block_da: None, block_da_footprint: None };

    let before = state.basic(signer.address).expect("db read").expect("sender funded");
    let (nonce_before, balance_before) = (before.nonce, before.balance);

    // ===== FAIL-CLOSED RUN =====
    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs = RecordingTxs { tx: Some(pooled.clone()), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let result = {
        let _guard = ForceCreateAddressMismatchGuard::arm();
        let r = ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
        // Checked INSIDE the guarded scope, BEFORE the guard's Drop clears the flag: the loop's
        // own `create` hook consumed the one-shot seam, so it is already false here. If the
        // candidate never reached the create hook this would still be set.
        assert!(
            !FORCE_CREATE_ADDRESS_MISMATCH.with(|f| f.get()),
            "the loop's CREATE hook must have consumed the one-shot mismatch seam"
        );
        r
    };
    assert!(matches!(result, Ok(None)), "loop completes without cancellation: {result:?}");

    // The fail-closed chain ends in a DENY decision, observed directly through the decision
    // seam (never inferred from mark_invalid / txpool removal, which Screen::Drop shares).
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::Deny),
        "the real CreateAddressMismatch must fail closed to Screen::Deny on the candidate loop"
    );

    // Supporting eviction evidence.
    assert!(
        best_txs.marked.contains(&(signer.address, 0)),
        "the fail-closed candidate is mark_invalid'd by the loop"
    );
    assert!(
        pool.get(&candidate_hash).is_none(),
        "the fail-closed candidate is removed from the txpool"
    );

    // The `continue` fires before execution accounting: no block entry, no receipt, no gas.
    assert!(info.executed_transactions.is_empty(), "fail-closed: no tx appended to the block");
    assert!(info.receipts.is_empty(), "fail-closed: no receipt pushed (commit skipped)");
    assert_eq!(info.cumulative_gas_used, 0, "fail-closed: no gas accounted (never committed)");

    let after = state.basic(signer.address).expect("db read").expect("sender present");
    assert_eq!(after.nonce, nonce_before, "fail-closed: sender nonce unchanged (no commit)");
    assert_eq!(after.balance, balance_before, "fail-closed: sender balance unchanged");

    // `screen_tx` was SKIPPED: the buffer gained nothing and the candidate has no entry.
    assert_eq!(
        filter.buffered_len(),
        0,
        "fail-closed must not add a BufferPool entry (screen_tx skipped)"
    );
    assert!(
        filter.buffer_status(&candidate_hash).is_none(),
        "no BufferEntry may exist for the failed-closed candidate"
    );

    // ===== CONTROL RUN (no forced mismatch) — the SAME candidate DOES reach screen_tx =====
    // Without the seam the CREATE completes consistently, the captured stream carries the real
    // ERC20 Transfer, and RULE_SCENARIO_A matches → AuditPending → a BufferEntry appears. This
    // is what makes the buffer-absence assertion above evidence that screen_tx was skipped.
    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs_ctrl = RecordingTxs { tx: Some(pooled), marked: Vec::new() };
    let mut info_ctrl = ExecutionInfo::with_capacity(1);
    let control = ctx.execute_best_transactions(
        &mut info_ctrl,
        &mut state,
        &mut best_txs_ctrl,
        &pool,
        limits,
    );
    assert!(matches!(control, Ok(None)), "control loop completes: {control:?}");
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::AuditPending),
        "control: the SAME candidate, run without a forced mismatch, screens to AuditPending"
    );
    assert_eq!(
        filter.buffer_status(&candidate_hash),
        Some(BufferStatus::NotSubmitted),
        "control: the candidate's normal run inserts a BufferPool entry"
    );
    assert_eq!(filter.buffered_len(), 1, "control: the candidate now occupies the pending buffer");
}

/// A rule matching ANY native-asset `Transfer` by address alone (not by which contract emitted
/// it), so a budget-boundary fixture can live at any factory address instead of needing to
/// collide with `golden::token_x()`.
const AUDIT_ANY_NATIVE_TRANSFER_RULE: &str = r#"{
        "id": "audit-any-native-transfer",
        "event_abis": {
            "transfer": {
                "type": "event", "name": "Transfer",
                "inputs": [
                    {"name": "from", "type": "address", "indexed": true},
                    {"name": "to",   "type": "address", "indexed": true},
                    {"name": "value","type": "uint256",  "indexed": false}
                ],
                "anonymous": false
            }
        },
        "condition": {"==":[{"var":"transfer.address"},"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"]},
        "audit_types": ["native_budget_test"],
        "action": "audit"
    }"#;

/// Runtime bytecode performing `recipients.len()` sequential 1-wei `CALL` transfers, each a
/// distinct internal native-transfer observation, then `STOP`.
fn native_transfers_code(recipients: &[Address]) -> Vec<u8> {
    let mut code = Vec::new();
    for dst in recipients {
        // PUSH1 0 (retSize) PUSH1 0 (retOffset) PUSH1 0 (argsSize) PUSH1 0 (argsOffset)
        // PUSH1 1 (value) PUSH20 dst GAS CALL POP
        code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x01, 0x73]);
        code.extend_from_slice(dst.as_slice());
        code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
    }
    code.push(0x00); // STOP
    code
}

/// Builds a funded sender + a factory at `factory` performing `recipients.len()` internal
/// native transfers, a signed candidate calling it, and the pool/state needed to drive one
/// real `execute_best_transactions` run.
fn native_limit_candidate_fixture(
    filter: Arc<FilterHandle>,
    signer: &crate::signer::Signer,
    factory: Address,
    recipients: &[Address],
) -> (FlashblocksBuilderCtx, State<CacheDB<EmptyDB>>, B256, OpPooledTransaction) {
    let ctx = build_ctx(filter);
    let candidate = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 200_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(factory),
        value: U256::ZERO,
        ..Default::default()
    });
    let recovered = signer.sign_tx(candidate).expect("sign candidate");
    let candidate_hash: B256 = recovered.tx_hash();
    let encoded_len = recovered.encode_2718_len();
    let pooled = OpPooledTransaction::new(recovered, encoded_len);

    let mut cache = CacheDB::new(EmptyDB::default());
    cache.insert_account_info(
        signer.address,
        AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
    );
    cache.insert_account_info(
        factory,
        AccountInfo {
            balance: U256::from(recipients.len() as u64), // funds the 1-wei internal transfers
            code: Some(Bytecode::new_raw(Bytes::from(native_transfers_code(recipients)))),
            ..Default::default()
        },
    );
    let state = State::builder().with_database(cache).with_bundle_update().build();
    (ctx, state, candidate_hash, pooled)
}

/// The native-transfer budget fail-closed chain, driven through the ACTUAL candidate loop
/// rather than a helper's `CaptureOutcome`: the loop's own `RcsInspector` (constructed by
/// `execute_best_transactions` straight from `FilterConfig::max_native_transfers_per_tx`, so no
/// new production seam is needed) overflows at `limit + 1`, `finish_capture` returns
/// `LimitExceeded`, and `fail_closed_screen` must turn that into a Deny WITHOUT reaching
/// `screen_tx`.
///
/// Falsifiable: temporarily widening the `LimitExceeded` arm of `fail_closed_screen` to return
/// a non-`Deny` decision makes the `LAST_SCREEN_DECISION` assertion below fail.
#[tokio::test]
async fn loop_level_native_limit_exceeded_denies_no_buffer_no_commit_no_receipt() {
    let rules =
        load_rules(1, 1, vec![serde_json::from_str(AUDIT_ANY_NATIVE_TRANSFER_RULE).unwrap()]);
    let recipients = [Address::repeat_byte(0xA1), Address::repeat_byte(0xA2)];
    let factory = Address::repeat_byte(0xF9);

    // ===== FAIL-CLOSED RUN — budget 1, two observations overflow at the second =====
    let filter = Arc::new(FilterHandle::for_test(
        FilterConfig { max_native_transfers_per_tx: 1, ..Default::default() },
        rules.clone(),
        Arc::new(SystemClock),
    ));
    let signer = crate::tests::funded_signer();
    let (ctx, mut state, candidate_hash, pooled) =
        native_limit_candidate_fixture(filter.clone(), &signer, factory, &recipients);
    let pool = testing_pool();
    let mock = MockTransaction::legacy()
        .with_sender(signer.address)
        .with_nonce(0)
        .with_gas_price(100)
        .with_hash(candidate_hash);
    pool.add_transaction(TransactionOrigin::External, mock).await.unwrap();
    assert!(pool.get(&candidate_hash).is_some(), "candidate is in the txpool before the loop");

    let limits =
        TransactionLimits { block_gas: 30_000_000, block_da: None, block_da_footprint: None };
    let before = state.basic(signer.address).expect("db read").expect("sender funded");
    let (nonce_before, balance_before) = (before.nonce, before.balance);

    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs = RecordingTxs { tx: Some(pooled), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let result = ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(result, Ok(None)), "loop completes without cancellation: {result:?}");

    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::Deny),
        "the real native LimitExceeded must fail closed to Screen::Deny on the candidate loop"
    );
    assert!(
        best_txs.marked.contains(&(signer.address, 0)),
        "the fail-closed candidate is mark_invalid'd by the loop"
    );
    assert!(
        pool.get(&candidate_hash).is_none(),
        "the fail-closed candidate is removed from the txpool"
    );
    assert!(info.executed_transactions.is_empty(), "fail-closed: no tx appended to the block");
    assert!(info.receipts.is_empty(), "fail-closed: no receipt pushed (commit skipped)");
    assert_eq!(info.cumulative_gas_used, 0, "fail-closed: no gas accounted (never committed)");
    let after = state.basic(signer.address).expect("db read").expect("sender present");
    assert_eq!(after.nonce, nonce_before, "fail-closed: sender nonce unchanged (no commit)");
    assert_eq!(after.balance, balance_before, "fail-closed: sender balance unchanged");
    assert_eq!(
        filter.buffered_len(),
        0,
        "fail-closed must not add a BufferPool entry (screen_tx skipped)"
    );
    assert!(
        filter.buffer_status(&candidate_hash).is_none(),
        "no BufferEntry may exist for the fail-closed candidate"
    );

    // ===== AT-LIMIT CONTROL — budget 2, the SAME two observations do NOT deny. Guards against
    // an off-by-one: without this control a deny-at-N implementation would also pass above. =====
    let filter2 = Arc::new(FilterHandle::for_test(
        FilterConfig { max_native_transfers_per_tx: 2, ..Default::default() },
        rules,
        Arc::new(SystemClock),
    ));
    let (ctx2, mut state2, candidate_hash2, pooled2) =
        native_limit_candidate_fixture(filter2.clone(), &signer, factory, &recipients);
    let pool2 = testing_pool();
    let mock2 = MockTransaction::legacy()
        .with_sender(signer.address)
        .with_nonce(0)
        .with_gas_price(100)
        .with_hash(candidate_hash2);
    pool2.add_transaction(TransactionOrigin::External, mock2).await.unwrap();

    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs2 = RecordingTxs { tx: Some(pooled2), marked: Vec::new() };
    let mut info2 = ExecutionInfo::with_capacity(1);
    let control =
        ctx2.execute_best_transactions(&mut info2, &mut state2, &mut best_txs2, &pool2, limits);
    assert!(matches!(control, Ok(None)), "control loop completes: {control:?}");
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::AuditPending),
        "control: exactly N=2 observations under budget 2 must NOT deny"
    );
    assert_eq!(
        filter2.buffer_status(&candidate_hash2),
        Some(BufferStatus::NotSubmitted),
        "control: the at-limit candidate inserts a BufferPool entry"
    );
}

/// A one-shot-per-slot [`PayloadTransactions`] yielding candidates from a queue in order,
/// recording every `mark_invalid`. Used by the "loop survives a deny" test below, which needs
/// two DISTINCT candidates from the same `execute_best_transactions` call — `RecordingTxs`
/// only ever yields one.
struct QueuedTxs {
    queue: std::collections::VecDeque<OpPooledTransaction>,
    marked: Vec<(Address, u64)>,
}
impl PayloadTransactions for QueuedTxs {
    type Transaction = OpPooledTransaction;
    fn next(&mut self, _ctx: ()) -> Option<OpPooledTransaction> {
        self.queue.pop_front()
    }
    fn mark_invalid(&mut self, sender: Address, nonce: u64) {
        self.marked.push((sender, nonce));
    }
}

/// After a budget-overflow Deny the loop must keep going: the NEXT candidate in the same
/// `execute_best_transactions` call is screened normally and buffers, proving capture state was
/// reset and the deny is not sticky.
#[tokio::test]
async fn loop_level_native_limit_deny_does_not_poison_the_next_candidate() {
    let rules =
        load_rules(1, 1, vec![serde_json::from_str(AUDIT_ANY_NATIVE_TRANSFER_RULE).unwrap()]);
    let filter = Arc::new(FilterHandle::for_test(
        FilterConfig { max_native_transfers_per_tx: 1, ..Default::default() },
        rules,
        Arc::new(SystemClock),
    ));

    // Two DISTINCT senders AND two distinct factories, each at its own nonce 0: candidate #0's
    // deny path never commits state (the loop `continue`s before any nonce bump), so a
    // same-sender nonce 1 for #1 would be invalid against the real EVM/pool nonce sequencing.
    let ctx = build_ctx(filter.clone());
    let signer0 = crate::tests::funded_signer();
    let signer1 = crate::tests::builder_signer();
    let factory0 = Address::repeat_byte(0xF9); // #0: two transfers under budget 1 → overflow
    let factory1 = Address::repeat_byte(0xFA); // #1: one transfer, exactly at budget 1 → fits

    let candidate0 = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 200_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(factory0),
        value: U256::ZERO,
        ..Default::default()
    });
    let recovered0 = signer0.sign_tx(candidate0).expect("sign candidate 0");
    let hash0: B256 = recovered0.tx_hash();
    let len0 = recovered0.encode_2718_len();
    let pooled0 = OpPooledTransaction::new(recovered0, len0);

    let candidate1 = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 200_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(factory1),
        value: U256::ZERO,
        ..Default::default()
    });
    let recovered1 = signer1.sign_tx(candidate1).expect("sign candidate 1");
    let hash1: B256 = recovered1.tx_hash();
    let len1 = recovered1.encode_2718_len();
    let pooled1 = OpPooledTransaction::new(recovered1, len1);

    let mut cache = CacheDB::new(EmptyDB::default());
    cache.insert_account_info(
        signer0.address,
        AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
    );
    cache.insert_account_info(
        signer1.address,
        AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
    );
    cache.insert_account_info(
        factory0,
        AccountInfo {
            balance: U256::from(2u64),
            code: Some(Bytecode::new_raw(Bytes::from(native_transfers_code(&[
                Address::repeat_byte(0xA1),
                Address::repeat_byte(0xA2),
            ])))),
            ..Default::default()
        },
    );
    cache.insert_account_info(
        factory1,
        AccountInfo {
            balance: U256::from(1u64),
            code: Some(Bytecode::new_raw(Bytes::from(native_transfers_code(&[
                Address::repeat_byte(0xA3),
            ])))),
            ..Default::default()
        },
    );
    let mut state = State::builder().with_database(cache).with_bundle_update().build();

    let pool = testing_pool();
    for (sender, mock_hash) in [(signer0.address, hash0), (signer1.address, hash1)] {
        let mock = MockTransaction::legacy()
            .with_sender(sender)
            .with_nonce(0)
            .with_gas_price(100)
            .with_hash(mock_hash);
        pool.add_transaction(TransactionOrigin::External, mock).await.unwrap();
    }

    let limits =
        TransactionLimits { block_gas: 30_000_000, block_da: None, block_da_footprint: None };
    let mut best_txs = QueuedTxs {
        queue: std::collections::VecDeque::from([pooled0, pooled1]),
        marked: Vec::new(),
    };
    let mut info = ExecutionInfo::with_capacity(2);
    let result = ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(result, Ok(None)), "loop completes without cancellation: {result:?}");

    assert!(best_txs.marked.contains(&(signer0.address, 0)), "candidate #0 is mark_invalid'd");
    assert!(
        filter.buffer_status(&hash0).is_none(),
        "candidate #0 (budget-overflow) must not buffer"
    );
    assert_eq!(
        filter.buffer_status(&hash1),
        Some(BufferStatus::NotSubmitted),
        "candidate #1, screened AFTER #0's deny, buffers normally — capture state was reset \
             and the deny did not poison the loop"
    );
    assert_eq!(filter.buffered_len(), 1, "exactly #1 occupies the pending buffer");
}

#[path = "flashblocks__context__loop_level_fail_closed_tests__snapshot_binding_tests.rs"]
#[cfg(test)]
mod snapshot_binding_tests;
