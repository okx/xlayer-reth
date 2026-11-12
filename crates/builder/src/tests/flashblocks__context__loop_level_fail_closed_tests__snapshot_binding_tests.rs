//! Rule-snapshot binding across hot reloads, driven through `execute_best_transactions`: an
//! approved candidate re-simulated in a later block keeps its saved snapshot, and every
//! flashblock of one block keeps the snapshot pinned for that block.

use super::*;

/// A callstack-gated audit rule: the decoded event supplies the action payload while the
/// observed call frame acts as a condition, which is the only shape §6.8 permits for `audit`.
/// Only the `content_version` differs between the two snapshots the test installs, so the
/// selector set, the rule text and the executed code are all identical across the reload.
fn callstack_gated_audit_rule() -> RawRule {
    serde_json::from_value(serde_json::json!({
        "id": "gated-quota-audit",
        "event_abis": { "transfer": {
            "type": "event", "name": "Transfer",
            "inputs": [
                { "name": "from", "type": "address", "indexed": true },
                { "name": "to", "type": "address", "indexed": true },
                { "name": "value", "type": "uint256", "indexed": false }
            ],
            "anonymous": false
        }},
        "audit_types": ["quota"],
        "callstack": { "direct_delegatecalls": {
            "entry": format!("{:#x}", golden::token_x())
        } },
        "condition": { "and": [
            { "!=": [ { "var": "entry.delegateTo" }, null ] },
            { "==": [ { "var": "transfer.address" }, format!("{:#x}", golden::token_x()) ] }
        ]},
        "action": "audit",
        "audit_timeout_action": "allow"
    }))
    .expect("valid callstack-gated audit rule")
}

fn callstack_rules_at(content_version: u64) -> rcs_filter::rules::RuleSet {
    let rules = load_rules(
        rcs_filter::config::MIN_PROTOCOL_VERSION,
        content_version,
        vec![callstack_gated_audit_rule()],
    );
    assert!(rules.rejected.is_empty(), "rule must load: {:?}", rules.rejected);
    rules
}

/// Installs the sender and the ERC20 `Transfer`-emitting contract the candidate calls.
/// The implementation `TOKEN_X` delegates into. Its code is what actually emits the log, so
/// under the `DELEGATECALL` the log's address is still `TOKEN_X`.
fn callstack_implementation() -> Address {
    Address::repeat_byte(0xd1)
}

/// `DELEGATECALL(gas, implementation, 0, 0, 0, 0); POP; STOP` — the one frame shape a rule
/// can observe under this contract.
fn delegating_code(target: Address) -> Vec<u8> {
    let mut code = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73];
    code.extend_from_slice(target.as_slice());
    code.extend_from_slice(&[0x5a, 0xf4, 0x50, 0x00]);
    code
}

fn callstack_state(sender: Address) -> State<CacheDB<EmptyDB>> {
    let mut cache = CacheDB::new(EmptyDB::default());
    cache.insert_account_info(
        sender,
        AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
    );
    // `TOKEN_X` performs a first-level `DELEGATECALL`, which is what makes it observable:
    // only a direct delegatecall of a declared address is ever a candidate.
    cache.insert_account_info(
        golden::token_x(),
        AccountInfo {
            code: Some(Bytecode::new_raw(Bytes::from(delegating_code(callstack_implementation())))),
            ..Default::default()
        },
    );
    cache.insert_account_info(
        callstack_implementation(),
        AccountInfo {
            code: Some(Bytecode::new_raw(Bytes::from(erc20_transfer_emitter_code()))),
            ..Default::default()
        },
    );
    State::builder().with_database(cache).with_bundle_update().build()
}

/// Signs one candidate that calls `TOKEN_X`, whose code delegatecalls its implementation — so
/// the real capture observes a DIRECT DELEGATECALL of the declared address AND the log the
/// implementation emits (under the delegatecall, at `TOKEN_X`) matches the rule's event.
fn callstack_candidate() -> (Signer, OpPooledTransaction, B256) {
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
    let hash: B256 = recovered.tx_hash();
    let encoded_len = recovered.encode_2718_len();
    (signer, OpPooledTransaction::new(recovered, encoded_len), hash)
}

/// Snapshot-binding regression (design §6.5.1 rule 3): an `Approved` candidate re-simulated in
/// a LATER L2 block, after a hot reload that changed nothing but `content_version`, must still
/// reach `Screen::AuditApproved`.
///
/// Driven entirely through `execute_best_transactions`, so the observation stamp is produced by
/// the real capture chain rather than hand-built: the first block buffers the candidate under
/// version 41, version 42 is then installed, the candidate is approved, and the second block
/// pins version 42. If capture were configured from the block-pinned snapshot while the
/// consistency recompute evaluated the SAVED one, the stamps would differ and the candidate
/// would be dropped — which is exactly the defect this test pins.
#[tokio::test]
async fn approved_resimulation_after_a_cross_block_reload_uses_the_saved_snapshot() {
    let filter = Arc::new(FilterHandle::for_test(
        FilterConfig::default(),
        callstack_rules_at(41),
        Arc::new(SystemClock),
    ));
    let (signer, pooled, candidate_hash) = callstack_candidate();
    let mut state = callstack_state(signer.address);
    let pool = testing_pool();
    let mock = MockTransaction::legacy()
        .with_sender(signer.address)
        .with_nonce(0)
        .with_gas_price(100)
        .with_hash(candidate_hash);
    pool.add_transaction(TransactionOrigin::External, mock).await.unwrap();
    let limits =
        TransactionLimits { block_gas: 30_000_000, block_da: None, block_da_footprint: None };

    // ===== BLOCK N: the candidate is screened under version 41 and buffers. =====
    let ctx_v41 = build_ctx(filter.clone());
    assert_eq!(ctx_v41.rcs_rule_snapshot.as_ref().expect("snapshot pinned").content_version, 41);
    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs = RecordingTxs { tx: Some(pooled.clone()), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let first =
        ctx_v41.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(first, Ok(None)), "block N loop completes: {first:?}");
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::AuditPending),
        "the candidate's first screening buffers it for adjudication"
    );
    assert_eq!(filter.buffer_status(&candidate_hash), Some(BufferStatus::NotSubmitted));

    // A hot reload lands while the candidate is buffered. Nothing changes but the version.
    filter.install_rules(callstack_rules_at(42));
    filter.advance_to_approved_for_test(&candidate_hash);
    assert_eq!(filter.buffer_status(&candidate_hash), Some(BufferStatus::Approved));

    // ===== BLOCK N+1: a new payload job pins version 42 and re-simulates the candidate. =====
    let ctx_v42 = build_ctx(filter.clone());
    assert_eq!(
        ctx_v42.rcs_rule_snapshot.as_ref().expect("snapshot pinned").content_version,
        42,
        "the next block pins the reloaded snapshot"
    );
    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs = RecordingTxs { tx: Some(pooled), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let second =
        ctx_v42.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(second, Ok(None)), "block N+1 loop completes: {second:?}");
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::AuditApproved),
        "the Approved re-simulation must configure capture from the SAVED snapshot, so a \
             version-only reload cannot turn it into a stamp mismatch and a Drop"
    );
    assert_eq!(
        info.executed_transactions.len(),
        1,
        "the approved candidate is packaged into the block"
    );
}

/// A callstack-only deny rule on `address`: any transaction whose direct delegatecall of it
/// is observed is rejected. `audit` would need decoded events; `deny` is the shape a pure
/// callstack rule takes, so it isolates the snapshot question from the event pipeline.
fn callstack_deny_rule(address: Address) -> RawRule {
    serde_json::from_value(serde_json::json!({
        "id": "deny-observed-frame",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{address:#x}") } },
        "condition": { "!=": [ { "var": "entry.delegateTo" }, null ] },
        "action": "deny"
    }))
    .expect("valid callstack deny rule")
}

fn deny_rules_at(address: Address, content_version: u64) -> rcs_filter::rules::RuleSet {
    let rules = load_rules(
        rcs_filter::config::MIN_PROTOCOL_VERSION,
        content_version,
        vec![callstack_deny_rule(address)],
    );
    assert!(rules.rejected.is_empty(), "rule must load: {:?}", rules.rejected);
    rules
}

/// Snapshot-pinning regression (design §6.5.1.1): every flashblock of ONE L2 block keeps the
/// same pinned snapshot, for the capture selector set as well as for screening.
///
/// `execute_best_transactions` runs once per flashblock, so the test calls it twice on the SAME
/// context with a hot reload in between. Because the reload replaces the selected address, a
/// per-flashblock (or per-candidate-fresh) re-read of the live rules would stop denying the
/// candidate in the second flashblock; the block-pinned snapshot must keep denying it. The
/// third run, on a NEW context, is where the reload is allowed to take effect.
#[tokio::test]
async fn every_flashblock_of_one_block_keeps_the_pinned_snapshot() {
    let elsewhere = Address::repeat_byte(0x7e);
    let filter = Arc::new(FilterHandle::for_test(
        FilterConfig::default(),
        deny_rules_at(golden::token_x(), 1),
        Arc::new(SystemClock),
    ));
    let (signer, pooled, candidate_hash) = callstack_candidate();
    let mut state = callstack_state(signer.address);
    let pool = testing_pool();
    let mock = MockTransaction::legacy()
        .with_sender(signer.address)
        .with_nonce(0)
        .with_gas_price(100)
        .with_hash(candidate_hash);
    pool.add_transaction(TransactionOrigin::External, mock).await.unwrap();
    let limits =
        TransactionLimits { block_gas: 30_000_000, block_da: None, block_da_footprint: None };

    // One context = one L2 block payload job, pinned once.
    let ctx = build_ctx(filter.clone());
    let pinned_version = ctx.rcs_rule_snapshot.as_ref().expect("snapshot pinned").content_version;
    assert_eq!(pinned_version, 1);

    // ===== FLASHBLOCK 1 =====
    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs = RecordingTxs { tx: Some(pooled.clone()), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let first = ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(first, Ok(None)), "flashblock 1 loop completes: {first:?}");
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::Deny),
        "flashblock 1: the observed frame at the selected address denies the candidate"
    );

    // A hot reload lands mid-block, moving the selector to an address the candidate never
    // touches. Rule installation is an immediate swap, so this is the moment that could leak
    // new rules into the block already being built.
    filter.install_rules(deny_rules_at(elsewhere, 2));

    // ===== FLASHBLOCK 2 of the SAME block =====
    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs = RecordingTxs { tx: Some(pooled.clone()), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let second = ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(second, Ok(None)), "flashblock 2 loop completes: {second:?}");
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::Deny),
        "flashblock 2 of the same block must still capture and screen under the pinned \
             snapshot, so the mid-block reload cannot change the verdict"
    );
    assert_eq!(
        ctx.rcs_rule_snapshot.as_ref().expect("snapshot pinned").content_version,
        pinned_version,
        "the context's pinned snapshot never moves during its block"
    );

    // ===== NEXT L2 BLOCK — a new payload job, where the reload finally takes effect. =====
    let next_ctx = build_ctx(filter.clone());
    assert_eq!(next_ctx.rcs_rule_snapshot.as_ref().expect("snapshot pinned").content_version, 2);
    LAST_SCREEN_DECISION.with(|d| d.set(None));
    let mut best_txs = RecordingTxs { tx: Some(pooled), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let third =
        next_ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(third, Ok(None)), "next block loop completes: {third:?}");
    assert_eq!(
        LAST_SCREEN_DECISION.with(|d| d.get()),
        Some(Screen::Allow),
        "the next block pins the reloaded snapshot, whose selector the candidate never touches"
    );
}
