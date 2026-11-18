//! The candidate loop: block-pinned rule snapshots, per-candidate snapshot binding across hot
//! reloads, and the builder's own (independent) observation budget.

use std::sync::Arc;

use alloy_consensus::TxEip1559;
use alloy_eips::Encodable2718;
use alloy_primitives::{Address, TxKind, U256};
use op_alloy_consensus::OpTypedTransaction;
use rcs_filter::capture::CallstackCaptureConfig;
use rcs_filter::rules::{load_rules, RawRule};
use rcs_filter::test_support::golden;
use rcs_filter::{BufferStatus, FilterConfig, FilterHandle, Screen, SystemClock};
use reth_optimism_txpool::OpPooledTransaction;
use reth_revm::State;
use reth_transaction_pool::test_utils::{testing_pool, MockTransaction};
use reth_transaction_pool::{TransactionOrigin, TransactionPool};
use revm::database::{CacheDB, EmptyDB};
use revm::state::{AccountInfo, Bytecode};
use serde_json::json;
use xlayer_builder::flashblocks::test_seams::{
    clear_last_candidate_observations, clear_last_screen_decision, last_candidate_observations,
    last_screen_decision, ExecutionInfo, LoopSeams, Signer, TransactionLimits,
};

use crate::fixtures::*;

// ------------------------------------------------------------ snapshot pinning --------

fn selector_rule(id: &str, address: Address) -> RawRule {
    serde_json::from_value(json!({
        "id": id,
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{address:#x}") } },
        "condition": { "!=": [ { "var": "entry.delegateTo" }, null ] },
        "action": "deny"
    }))
    .expect("valid rule")
}

fn handle_with(address: Address, content_version: u64) -> Arc<FilterHandle> {
    let rules = load_rules(
        rcs_filter::config::MIN_PROTOCOL_VERSION,
        content_version,
        vec![selector_rule("guard", address)],
    );
    assert!(rules.rejected.is_empty(), "{:?}", rules.rejected);
    Arc::new(FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock)))
}

/// A context carrying the snapshot pinned for its block, exactly as the payload job builds it.
fn pin(filter: &Arc<FilterHandle>) -> Option<Arc<rcs_filter::rules::RuleSet>> {
    Some(filter.rule_snapshot())
}

#[test]
fn the_capture_config_comes_from_the_pinned_snapshot_not_the_live_one() {
    let first = Address::repeat_byte(0x11);
    let second = Address::repeat_byte(0x22);
    let filter = handle_with(first, 1);

    // The block's payload job pins the snapshot once, up front.
    let pinned = pin(&filter).expect("screening is enabled");
    let pinned_stamp = pinned.stamp();
    assert_eq!(pinned.callstack_selectors(), [first]);

    // A hot reload lands mid-block. Rule installation is an immediate swap, so this is the
    // moment that would leak new rules into the block already being built.
    filter.install_rules(load_rules(
        rcs_filter::config::MIN_PROTOCOL_VERSION,
        2,
        vec![selector_rule("guard", second)],
    ));

    // Every flashblock of this block builds a fresh inspector from the SAME pinned snapshot,
    // so neither the selector set nor the stamp moves part-way through the block.
    assert_eq!(pinned.callstack_selectors(), [first]);
    assert_eq!(pinned.stamp(), pinned_stamp);

    // The next block's payload job pins the reloaded snapshot, which is where it takes effect.
    let next_block = pin(&filter).expect("screening is enabled");
    assert_eq!(next_block.callstack_selectors(), [second]);
    assert_ne!(next_block.stamp(), pinned_stamp);
}

#[test]
fn a_context_without_screening_pins_no_snapshot_and_captures_nothing() {
    // With the master switch off there is no snapshot to pin, and the capture configuration
    // is inert, so the observation hooks cost nothing.
    let config = CallstackCaptureConfig::default();
    assert!(config.is_inert());
}

#[test]
fn a_snapshot_with_no_selectors_yields_an_inert_capture_config() {
    // Screening enabled but no active rule asking about call frames: the whole observation
    // path stays inert rather than observing every frame "just in case".
    let rules = load_rules(rcs_filter::config::MIN_PROTOCOL_VERSION, 1, vec![]);
    assert!(!rules.has_callstack_rules());
    let config = CallstackCaptureConfig::new(rules.stamp(), rules.callstack_selectors().to_vec());
    assert!(config.is_inert());
}

#[test]
fn a_snapshot_with_selectors_yields_an_active_capture_config_carrying_its_stamp() {
    let address = Address::repeat_byte(0x33);
    let filter = handle_with(address, 7);
    let pinned = filter.rule_snapshot();
    let config = CallstackCaptureConfig::new(pinned.stamp(), pinned.callstack_selectors().to_vec());
    assert!(!config.is_inert());
    // The stamp travels with the selector set, so an inspector cannot be configured from one
    // snapshot and stamped with another.
    assert_eq!(config.stamp_for_test(), Some(pinned.stamp()));
}

// ------------------------------------------------------- snapshot binding --------

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
            code: Some(Bytecode::new_raw(delegating_code(callstack_implementation()).into())),
            ..Default::default()
        },
    );
    cache.insert_account_info(
        callstack_implementation(),
        AccountInfo {
            code: Some(Bytecode::new_raw(erc20_transfer_emitter_code().into())),
            ..Default::default()
        },
    );
    State::builder().with_database(cache).with_bundle_update().build()
}

/// Signs one candidate that calls `TOKEN_X`, whose code delegatecalls its implementation — so
/// the real capture observes a DIRECT DELEGATECALL of the declared address AND the log the
/// implementation emits (under the delegatecall, at `TOKEN_X`) matches the rule's event.
fn callstack_candidate() -> (Signer, OpPooledTransaction, alloy_primitives::B256) {
    let signer = funded_signer();
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
    let hash: alloy_primitives::B256 = recovered.tx_hash();
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
    clear_last_screen_decision();
    let mut best_txs = RecordingTxs { tx: Some(pooled.clone()), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let first =
        ctx_v41.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(first, Ok(None)), "block N loop completes: {first:?}");
    assert_eq!(
        last_screen_decision(),
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
    clear_last_screen_decision();
    let mut best_txs = RecordingTxs { tx: Some(pooled), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let second =
        ctx_v42.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(second, Ok(None)), "block N+1 loop completes: {second:?}");
    assert_eq!(
        last_screen_decision(),
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
    clear_last_screen_decision();
    let mut best_txs = RecordingTxs { tx: Some(pooled.clone()), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let first = ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(first, Ok(None)), "flashblock 1 loop completes: {first:?}");
    assert_eq!(
        last_screen_decision(),
        Some(Screen::Deny),
        "flashblock 1: the observed frame at the selected address denies the candidate"
    );

    // A hot reload lands mid-block, moving the selector to an address the candidate never
    // touches. Rule installation is an immediate swap, so this is the moment that could leak
    // new rules into the block already being built.
    filter.install_rules(deny_rules_at(elsewhere, 2));

    // ===== FLASHBLOCK 2 of the SAME block =====
    clear_last_screen_decision();
    let mut best_txs = RecordingTxs { tx: Some(pooled.clone()), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let second = ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(second, Ok(None)), "flashblock 2 loop completes: {second:?}");
    assert_eq!(
        last_screen_decision(),
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
    clear_last_screen_decision();
    let mut best_txs = RecordingTxs { tx: Some(pooled), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    let third =
        next_ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits);
    assert!(matches!(third, Ok(None)), "next block loop completes: {third:?}");
    assert_eq!(
        last_screen_decision(),
        Some(Screen::Allow),
        "the next block pins the reloaded snapshot, whose selector the candidate never touches"
    );
}

// ---------------------------------------------------------- observation budget --------

fn candidate_state(sender: Address, proxy_code: Vec<u8>) -> State<CacheDB<EmptyDB>> {
    let mut cache = CacheDB::new(EmptyDB::default());
    cache.insert_account_info(
        sender,
        AccountInfo { balance: U256::from(10u128.pow(18)), nonce: 0, ..Default::default() },
    );
    cache.insert_account_info(
        PROXY,
        AccountInfo { code: Some(Bytecode::new_raw(proxy_code.into())), ..Default::default() },
    );
    cache.insert_account_info(
        APPROVED_IMPL,
        AccountInfo { code: Some(Bytecode::new_raw(impl_code(0x01).into())), ..Default::default() },
    );
    State::builder().with_database(cache).with_bundle_update().build()
}

/// The builder's observation budget is its own module constant (10 000) and is unrelated to the
/// audit RPC's 1 000: through the real candidate loop a candidate producing 1 001 observations is
/// captured in full and allowed, never truncated.
#[tokio::test]
async fn the_builder_budget_is_not_bounded_by_the_audit_default() {
    let approved = code_hash(&impl_code(0x01));
    let filter = filter_with(vec![proxy_impl_allowlist_rule(&[approved])], test_config());
    let ctx = build_ctx(filter.clone());

    let signer = funded_signer();
    let candidate = OpTypedTransaction::Eip1559(TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 2_000_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(PROXY),
        value: U256::ZERO,
        ..Default::default()
    });
    let recovered = signer.sign_tx(candidate).expect("sign the candidate");
    let candidate_hash = recovered.tx_hash();
    let encoded_len = recovered.encode_2718_len();
    let pooled = OpPooledTransaction::new(recovered, encoded_len);

    // The proxy's own frame plus one per first-level delegatecall: 1 001 observations.
    let mut state = candidate_state(signer.address, delegating_proxy_code(APPROVED_IMPL, 1_000));
    let pool = testing_pool();
    let mock = MockTransaction::legacy()
        .with_sender(signer.address)
        .with_nonce(0)
        .with_gas_price(100)
        .with_hash(candidate_hash);
    pool.add_transaction(TransactionOrigin::External, mock).await.unwrap();

    let limits =
        TransactionLimits { block_gas: 30_000_000, block_da: None, block_da_footprint: None };
    clear_last_candidate_observations();
    clear_last_screen_decision();
    let mut best_txs = RecordingTxs { tx: Some(pooled), marked: Vec::new() };
    let mut info = ExecutionInfo::with_capacity(1);
    ctx.execute_best_transactions(&mut info, &mut state, &mut best_txs, &pool, limits)
        .expect("the candidate loop runs");

    let observed = last_candidate_observations()
        .expect("the candidate's capture completed, so its observations are recorded");
    assert_eq!(
        observed.len(),
        1_001,
        "the builder's default budget captures every frame of the fixture"
    );
    assert_eq!(
        last_screen_decision(),
        Some(Screen::Allow),
        "an in-budget candidate under the builder's own default is allowed, not truncated"
    );
    assert_eq!(
        rcs_filter::capture::CANDIDATE_MAX_CALLSTACK_OBSERVATIONS_PER_TX,
        10_000,
        "the builder's fixed per-transaction budget"
    );
    assert_eq!(
        xlayer_rpc::audit::rpc::AUDIT_MAX_CALLSTACK_OBSERVATIONS_PER_TX,
        1_000,
        "the audit RPC's budget is unrelated and unchanged"
    );
}
