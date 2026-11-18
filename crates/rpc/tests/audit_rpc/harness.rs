//! Shared fixtures for the audit-RPC integration tests: the observed proxy and its two
//! implementations, the normative rule shapes and the production batch driver.

use alloy_evm::EvmEnv;
use alloy_primitives::{keccak256, Address, B256, U256};
use op_revm::OpSpecId;
use rcs_filter::callstack::SnapshotStamp;
use rcs_filter::rules::{load_rules, RawRule};
use revm::context::{BlockEnv, CfgEnv};
use revm::database::InMemoryDB;
use revm::state::{AccountInfo, Bytecode};
use revm::Database as _;
use xlayer_rpc::audit::deposit::DepositTxRequest;
use xlayer_rpc::audit::rpc::test_seams::{
    execute_audit_batch, execute_audit_batch_with_capture_stamp,
};
use xlayer_rpc::audit::rpc::AuditTransactionsRequest;
use xlayer_rpc::audit::verdict::AuditResult;

/// The proxy a rule observes. Its own code performs a first-level `DELEGATECALL`.
pub(crate) const PROXY: Address = Address::repeat_byte(0xAA);
pub(crate) const APPROVED_IMPL: Address = Address::repeat_byte(0xB1);
pub(crate) const UNAPPROVED_IMPL: Address = Address::repeat_byte(0xB2);
pub(crate) const DEPOSITOR: Address = Address::repeat_byte(0xC1);

/// `DELEGATECALL(gas, target, 0,0,0,0); POP` repeated `times`, then `STOP`.
pub(crate) fn delegating_proxy_code(target: Address, times: usize) -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 0..times {
        code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73]);
        code.extend_from_slice(target.as_slice());
        code.extend_from_slice(&[0x5a, 0xf4, 0x50]);
    }
    code.push(0x00);
    code
}

/// `SSTORE(0, marker); STOP`. Under a `DELEGATECALL` this writes the PROXY's storage, which
/// makes "the state was committed" a direct observation rather than an inference. The marker
/// also gives the two implementations distinct code, hence distinct code hashes.
pub(crate) fn impl_code(marker: u8) -> Vec<u8> {
    vec![0x60, marker, 0x60, 0x00, 0x55, 0x00]
}

pub(crate) fn approved_hash() -> B256 {
    keccak256(impl_code(0x01))
}

/// The normative rule pattern: declare one alias on the proxy address, then deny when the
/// code it delegated into is NOT in the approved set. No guard variable is needed or
/// available: an alias that produced no observation suppresses the whole rule, and only a
/// direct delegatecall is ever a candidate, so the bound hash is always a real value.
pub(crate) fn proxy_impl_allowlist_rule() -> RawRule {
    proxy_impl_allowlist_rule_approving(&[approved_hash()])
}

/// The same rule as raw JSON, for a test that has to build a whole request body.
pub(crate) fn proxy_impl_allowlist_rule_json() -> serde_json::Value {
    proxy_impl_allowlist_rule_json_approving(&[approved_hash()])
}

/// [`proxy_impl_allowlist_rule_json`] over an explicit approved set.
pub(crate) fn proxy_impl_allowlist_rule_json_approving(approved: &[B256]) -> serde_json::Value {
    let approved: Vec<String> = approved.iter().map(|h| format!("{h:#x}")).collect();
    serde_json::json!({
        "id": "proxy-impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "in": [ { "var": "entry.delegateCodeHash" }, approved ] } },
        "action": "deny"
    })
}

/// The same pattern over an explicit approved set, for a fixture whose implementation is not
/// the default one. The shape is identical — only the allowlist contents differ.
pub(crate) fn proxy_impl_allowlist_rule_approving(approved: &[B256]) -> RawRule {
    let approved: Vec<String> = approved.iter().map(|h| format!("{h:#x}")).collect();
    serde_json::from_value(serde_json::json!({
        "id": "proxy-impl-allowlist",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "in": [ { "var": "entry.delegateCodeHash" }, approved ] } },
        "action": "deny"
    }))
    .expect("the normative sample must deserialize")
}

/// The origin the request's rules are narrowed to in the skip tests: deliberately NOT the
/// depositor, so every rule is filtered out with zero decoding before any observation is
/// consulted.
pub(crate) const OTHER_ORIGIN: Address = Address::repeat_byte(0x99);

/// The same normative pattern, additionally narrowed to one `origin`. Stage one of matching
/// skips such a rule on an origin mismatch before stage two resolves any call frame, so no
/// per-rule observation check can ever be reached for it.
pub(crate) fn proxy_impl_allowlist_rule_from_origin(origin: Address) -> RawRule {
    let approved = vec![format!("{:#x}", approved_hash())];
    serde_json::from_value(serde_json::json!({
        "id": "proxy-impl-allowlist-origin-narrowed",
        "origin": format!("{origin:#x}"),
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{PROXY:#x}") } },
        "condition": { "!": { "in": [ { "var": "entry.delegateCodeHash" }, approved ] } },
        "action": "deny"
    }))
    .expect("the origin-narrowed sample must deserialize")
}

/// An event rule declaring no `callstack` section at all.
pub(crate) fn event_only_rule() -> RawRule {
    serde_json::from_str(
        r#"{"id":"evt","event_abis":{"transfer":{"type":"event","name":"Transfer",
               "inputs":[{"name":"from","type":"address","indexed":true},
                         {"name":"to","type":"address","indexed":true},
                         {"name":"value","type":"uint256","indexed":false}],
               "anonymous":false}},"condition":true,"action":"deny"}"#,
    )
    .expect("valid event-only rule")
}

pub(crate) fn deposit_request(source_hash: &str, to: Address) -> DepositTxRequest {
    DepositTxRequest {
        source_hash: source_hash.to_string(),
        from: format!("{DEPOSITOR:#x}"),
        to: Some(format!("{to:#x}")),
        mint: "0".to_string(),
        value: "0".to_string(),
        gas_limit: 1_000_000,
        is_system_transaction: false,
        data: "0x".to_string(),
    }
}

pub(crate) fn seeded_db(proxy_target: Address, delegatecalls: usize) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    db.insert_account_info(
        PROXY,
        AccountInfo {
            code: Some(Bytecode::new_raw(
                delegating_proxy_code(proxy_target, delegatecalls).into(),
            )),
            ..Default::default()
        },
    );
    for (address, marker) in [(APPROVED_IMPL, 0x01u8), (UNAPPROVED_IMPL, 0x02u8)] {
        db.insert_account_info(
            address,
            AccountInfo {
                code: Some(Bytecode::new_raw(impl_code(marker).into())),
                ..Default::default()
            },
        );
    }
    db
}

/// The production EVM environment: a JOVIAN spec with a large block gas limit.
pub(crate) fn audit_env() -> EvmEnv<OpSpecId> {
    EvmEnv::new(
        CfgEnv::new_with_spec(OpSpecId::JOVIAN),
        BlockEnv { gas_limit: 30_000_000, ..Default::default() },
    )
}

/// One batch outcome: the per-transaction results plus the PROXY storage slot the
/// implementation writes, which is how "state committed" is observed directly. The depositor's
/// nonce counts committed transactions for fixtures whose code writes no storage.
pub(crate) struct BatchRun {
    pub(crate) results: Vec<AuditResult>,
    pub(crate) proxy_slot0: U256,
    pub(crate) depositor_nonce: u64,
}

/// The production request a batch test hands to the seam.
pub(crate) fn audit_request(
    reqs: &[DepositTxRequest],
    raw_rules: Vec<RawRule>,
    known_allowed: &[&str],
) -> AuditTransactionsRequest {
    AuditTransactionsRequest {
        rules: raw_rules,
        txs: reqs.to_vec(),
        known_allowed: known_allowed.iter().map(|hash| (*hash).to_string()).collect(),
    }
}

/// Runs the complete production batch through the `test-utils` seam, exactly as the handler
/// does after its state anchoring: validation, compilation, deposit construction, the
/// production inspector choice with the production budget, the loop and result assembly.
pub(crate) fn run_batch(
    reqs: &[DepositTxRequest],
    raw_rules: Vec<RawRule>,
    known_allowed: &[&str],
    db: InMemoryDB,
) -> BatchRun {
    run_batch_stamped(reqs, raw_rules, known_allowed, db, None)
}

/// The same production path, with the capture stamp the inspector is configured with made
/// injectable.
///
/// `None` is exactly [`run_batch`]: the inspector is the production one, so it stamps with the
/// evaluating snapshot's own identity and no test can accidentally pass by configuring capture
/// differently from the handler. `Some` builds the SAME real inspector over the SAME selector
/// set but under a FOREIGN snapshot identity, which is the only way to reach the stamp-mismatch
/// branch through production code: the request compiles its own rules, so a handler-driven test
/// can never make capture and screening disagree on the stamp.
pub(crate) fn run_batch_stamped(
    reqs: &[DepositTxRequest],
    raw_rules: Vec<RawRule>,
    known_allowed: &[&str],
    db: InMemoryDB,
    capture_stamp: Option<SnapshotStamp>,
) -> BatchRun {
    let req = audit_request(reqs, raw_rules, known_allowed);
    let mut db = db;
    let results = match capture_stamp {
        None => execute_audit_batch(&req, &mut db, audit_env()),
        Some(stamp) => execute_audit_batch_with_capture_stamp(&req, &mut db, audit_env(), stamp),
    }
    .expect("the batch executes");
    let proxy_slot0 = db.storage(PROXY, U256::ZERO).expect("storage read");
    let depositor_nonce = db.basic(DEPOSITOR).expect("account read").map_or(0, |a| a.nonce);
    BatchRun { results, proxy_slot0, depositor_nonce }
}

pub(crate) fn only(run: &BatchRun) -> &AuditResult {
    run.results.first().expect("the single transaction was classified")
}

/// A snapshot identity that is NOT the evaluating request's: the same selector set under a
/// different `content_version`, which is exactly the shape a stale or crossed capture
/// configuration produces. Capture still COMPLETES, so the observation set is `Available` —
/// the unavailable dimension never fires and only the stamp separates the two.
pub(crate) fn foreign_stamp(raw_rules: &[RawRule]) -> SnapshotStamp {
    let evaluating = load_rules(1, 0, raw_rules.to_vec());
    let foreign = load_rules(1, 7, raw_rules.to_vec());
    assert_eq!(
        foreign.callstack_selectors(),
        evaluating.callstack_selectors(),
        "the fixture must differ ONLY in snapshot identity, never in the observed set"
    );
    assert_ne!(foreign.stamp(), evaluating.stamp(), "the fixture must be a foreign stamp");
    foreign.stamp()
}

/// The implementation used by the repeated-adjudication fixture. Distinct from the two
/// implementations above because its code emits a log, hence a distinct code hash.
pub(crate) const LOGGING_IMPL: Address = Address::repeat_byte(0xB3);

/// Implementation code that emits one real ERC20
/// `Transfer(from = PROXY, to = DEPOSITOR, value = 1)` then `STOP`s. Executed under the
/// proxy's `DELEGATECALL`, so `log.address` is the PROXY.
///
/// `LOG3` pops `offset, size, topic0, topic1, topic2` with the offset on top, so the topics
/// are pushed in reverse.
pub(crate) fn logging_impl_code() -> Vec<u8> {
    let signature = keccak256("Transfer(address,address,uint256)");
    let mut code = vec![0x60, 0x01, 0x60, 0x00, 0x52]; // PUSH1 1; PUSH1 0; MSTORE
    code.push(0x7f); // PUSH32 topic2 (to)
    code.extend_from_slice(DEPOSITOR.into_word().as_slice());
    code.push(0x7f); // PUSH32 topic1 (from)
    code.extend_from_slice(PROXY.into_word().as_slice());
    code.push(0x7f); // PUSH32 topic0 (the event signature)
    code.extend_from_slice(signature.as_slice());
    code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0xa3, 0x00]); // PUSH1 32; PUSH1 0; LOG3; STOP
    code
}

/// A log-dimension rule asking for adjudication on the `Transfer` the fixture really emits.
/// This is what makes the convergence claim falsifiable: the very same input genuinely does
/// reach adjudication while it is not yet approved.
pub(crate) fn audit_on_proxy_transfer_rule() -> RawRule {
    serde_json::from_value(serde_json::json!({
        "id": "audit-proxy-transfer",
        "event_abis": { "transfer": {
            "type": "event", "name": "Transfer",
            "inputs": [
                { "name": "from", "type": "address", "indexed": true },
                { "name": "to", "type": "address", "indexed": true },
                { "name": "value", "type": "uint256", "indexed": false }
            ],
            "anonymous": false
        } },
        "condition": { "==": [ { "var": "transfer.address" }, format!("{PROXY:#x}") ] },
        "action": "audit"
    }))
    .expect("valid log-dimension rule")
}

/// The proxy delegatecalls [`LOGGING_IMPL`], so one transaction carries BOTH a call-frame
/// observation and a matching log.
pub(crate) fn logging_db() -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    db.insert_account_info(
        PROXY,
        AccountInfo {
            code: Some(Bytecode::new_raw(delegating_proxy_code(LOGGING_IMPL, 1).into())),
            ..Default::default()
        },
    );
    db.insert_account_info(
        LOGGING_IMPL,
        AccountInfo {
            code: Some(Bytecode::new_raw(logging_impl_code().into())),
            ..Default::default()
        },
    );
    db
}
