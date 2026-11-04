//! Multi-round Force Tx replay in the caller's own shape, against the production audit batch.
//!
//! The driver below mirrors RCS's replay loop (`internal/forcetx/replay_state.go` and
//! `ProcessOneRound` in `internal/forcetx/orchestrate.go` @
//! `8944b24f00c058cb5e65090a5c210335d1f186ed`): a stable synthetic-ban prefix followed by the
//! immutable originals, prefix verdicts skipped, a deny for an original making it terminal (and
//! removing it from `known_allowed`) and ending the round, a newly denied original turning into a
//! synthetic ban built exactly as RCS builds it, and termination once a round neither saw an
//! `unknown` nor grew the prefix. RCS's quota adjudication is replaced by a fixed per-deposit
//! table. Like RCS, the driver sends **no** label map unless a scenario says otherwise.
//!
//! Each round is a fresh request through the production decode, `AuditBatchPlan::prepare`, the
//! production deposit builder, inspector construction, `run_audit_batch` and `assemble_results`,
//! on a fresh copy of that round's base state, because each RPC call simulates on the current tip
//! and discards its overlay. The chain is the devnet chain id, with a blacklist fixture at the
//! devnet blacklist address, so the EVM's own native interceptor turns a blacklisted deposit into
//! a failed deposit exactly as on chain.
//!
//! This is an in-process mirror of the caller. It does not run RCS's code and does not replace a
//! joint run against a reachable node.

use super::*;
use crate::audit::synthetic_ban::{
    BATCH_ADD_TO_BLACKLIST_SIGNATURE, MAX_SYNTHETIC_BAN_BATCH, SYNTHETIC_BAN_SOURCE_HASH_DOMAIN,
};
use alloy_primitives::{address, hex, Keccak256};
use op_revm::OpHaltReason;
use revm::context_interface::result::ExecutionResult;
use std::collections::HashMap;

/// The devnet chain id: the only chain whose blacklist address the interceptor resolves and that
/// a test can deploy a fixture at.
const DEVNET_CHAIN_ID: u64 = 195;
/// The devnet blacklist proxy address the EVM's native interceptor queries.
const TX_BLACKLIST: Address = address!("0xb1ac000000000000000000000000000000000001");
/// The blacklist fixture's implementation, behind [`TX_BLACKLIST`].
const TX_BLACKLIST_IMPL: Address = Address::repeat_byte(0xBB);
/// The operator RCS sends synthetic bans from.
const OPERATOR: Address = Address::repeat_byte(0x0E);
/// Two upgradeable proxies, each read by its own call-frame rule.
const P1: Address = Address::repeat_byte(0xA1);
const P2: Address = Address::repeat_byte(0xA2);
/// Implementations a proxy can point at. Distinct markers, hence distinct code hashes.
const GOOD_IMPL: Address = Address::repeat_byte(0xD1);
const BAD_IMPL: Address = Address::repeat_byte(0xD2);
/// A deposit target with no code: its deposit is clean on every dimension.
const PLAIN: Address = Address::repeat_byte(0xE1);

// ------------------------------------------------------------------------- fixtures -------

/// A tiny assembler for the fixtures below: one-byte `PUSH1` jump targets, patched once every
/// label is placed.
#[derive(Default)]
struct Asm {
    code: Vec<u8>,
    labels: HashMap<&'static str, u8>,
    fixups: Vec<(usize, &'static str)>,
}

impl Asm {
    fn op(&mut self, bytes: &[u8]) -> &mut Self {
        self.code.extend_from_slice(bytes);
        self
    }
    fn push_label(&mut self, label: &'static str) -> &mut Self {
        self.code.push(0x60);
        self.fixups.push((self.code.len(), label));
        self.code.push(0);
        self
    }
    fn label(&mut self, label: &'static str) -> &mut Self {
        let at = u8::try_from(self.code.len()).expect("fixture code fits one-byte jumps");
        self.labels.insert(label, at);
        self.code.push(0x5b); // JUMPDEST
        self
    }
    fn finish(&mut self) -> Vec<u8> {
        for (at, label) in &self.fixups {
            self.code[*at] = self.labels[label];
        }
        std::mem::take(&mut self.code)
    }
}

fn selector(signature: &str) -> [u8; 4] {
    keccak256(signature.as_bytes())[..4].try_into().expect("four bytes")
}

/// The blacklist implementation, in runtime bytecode, over a Solidity-layout mapping at slot 0
/// (`keccak256(sourceHash ‖ 0)`):
/// - `isBlacklisted(bytes32) returns (bool)` reads it;
/// - `batchAddToBlacklist(bytes32[])` sets every listed hash, reading the canonical encoding;
/// - anything else reverts.
fn tx_blacklist_impl_code() -> Vec<u8> {
    let mut asm = Asm::default();
    asm.op(&[0x60, 0x00, 0x35, 0x60, 0xe0, 0x1c]) // selector = CALLDATALOAD(0) >> 224
        .op(&[0x80, 0x63])
        .op(&selector("isBlacklisted(bytes32)"))
        .op(&[0x14])
        .push_label("is")
        .op(&[0x57]) // DUP1 PUSH4 EQ JUMPI
        .op(&[0x63])
        .op(&selector(BATCH_ADD_TO_BLACKLIST_SIGNATURE))
        .op(&[0x14])
        .push_label("batch")
        .op(&[0x57]) // PUSH4 EQ JUMPI
        .op(&[0x60, 0x00, 0x60, 0x00, 0xfd]) // REVERT(0, 0)
        // isBlacklisted
        .label("is")
        .op(&[0x50]) // POP selector
        .op(&[0x60, 0x04, 0x35, 0x60, 0x00, 0x52]) // MSTORE(0, CALLDATALOAD(4))
        .op(&[0x60, 0x00, 0x60, 0x20, 0x52]) // MSTORE(32, 0)
        .op(&[0x60, 0x40, 0x60, 0x00, 0x20, 0x54]) // SLOAD(KECCAK256(0, 64))
        .op(&[0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3]) // RETURN the word
        // batchAddToBlacklist
        .label("batch")
        .op(&[0x60, 0x24, 0x35, 0x60, 0x00]) // n = CALLDATALOAD(36); i = 0
        .label("loop")
        .op(&[0x81, 0x81, 0x10, 0x15]) // !(i < n)
        .push_label("end")
        .op(&[0x57])
        .op(&[0x80, 0x60, 0x20, 0x02, 0x60, 0x44, 0x01, 0x35]) // CALLDATALOAD(68 + 32 i)
        .op(&[0x60, 0x00, 0x52, 0x60, 0x00, 0x60, 0x20, 0x52]) // MSTORE(0, e); MSTORE(32, 0)
        .op(&[0x60, 0x01, 0x60, 0x40, 0x60, 0x00, 0x20, 0x55]) // SSTORE(KECCAK256(0, 64), 1)
        .op(&[0x60, 0x01, 0x01]) // i += 1
        .push_label("loop")
        .op(&[0x56]) // JUMP
        .label("end")
        .op(&[0x00]);
    asm.finish()
}

/// A delegating proxy with a fixed target that forwards calldata and return data, as the real
/// blacklist proxy does.
fn forwarding_proxy_code(target: Address) -> Vec<u8> {
    let mut asm = Asm::default();
    asm.op(&[0x36, 0x60, 0x00, 0x60, 0x00, 0x37]) // CALLDATACOPY(0, 0, size)
        .op(&[0x60, 0x00, 0x60, 0x00, 0x36, 0x60, 0x00, 0x73])
        .op(target.as_slice())
        .op(&[0x5a, 0xf4]) // DELEGATECALL(gas, target, 0, size, 0, 0)
        .op(&[0x3d, 0x60, 0x00, 0x60, 0x00, 0x3e]) // RETURNDATACOPY(0, 0, size)
        .push_label("ok")
        .op(&[0x57, 0x3d, 0x60, 0x00, 0xfd]) // REVERT(0, size)
        .label("ok")
        .op(&[0x3d, 0x60, 0x00, 0xf3]); // RETURN(0, size)
    asm.finish()
}

/// An upgradeable proxy: delegatecalls whatever address slot 0 holds. The test upgrades it by
/// writing that slot between rounds — the chain tip moving under the replay.
fn upgradeable_proxy_code() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x54, 0x5a, 0xf4, 0x50, 0x00]
}

/// Implementation code: writes `marker` to the proxy's slot 1 and emits a real ERC20
/// `Transfer(from = the proxy, to = DEPOSITOR, value = 1)`, which the quota rule asks to audit.
fn emitting_impl_code(marker: u8) -> Vec<u8> {
    let mut code = vec![0x60, marker, 0x60, 0x01, 0x55]; // SSTORE(1, marker)
    code.extend_from_slice(&[0x60, 0x01, 0x60, 0x00, 0x52]); // MSTORE(0, 1)
    code.push(0x7f);
    code.extend_from_slice(DEPOSITOR.into_word().as_slice()); // topic2
    code.push(0x30); // ADDRESS: topic1 is the proxy
    code.push(0x7f);
    code.extend_from_slice(keccak256("Transfer(address,address,uint256)").as_slice()); // topic0
    code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0xa3, 0x00]); // LOG3; STOP
    code
}

fn code_account(code: Vec<u8>) -> AccountInfo {
    AccountInfo { code: Some(Bytecode::new_raw(code.into())), ..Default::default() }
}

/// The base chain state the first round runs on.
fn base_state() -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        DEPOSITOR,
        AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
    );
    db.insert_account_info(TX_BLACKLIST, code_account(forwarding_proxy_code(TX_BLACKLIST_IMPL)));
    db.insert_account_info(TX_BLACKLIST_IMPL, code_account(tx_blacklist_impl_code()));
    for proxy in [P1, P2] {
        db.insert_account_info(proxy, code_account(upgradeable_proxy_code()));
        point(&mut db, proxy, GOOD_IMPL);
    }
    db.insert_account_info(GOOD_IMPL, code_account(emitting_impl_code(0x01)));
    db.insert_account_info(BAD_IMPL, code_account(emitting_impl_code(0x02)));
    db
}

/// Points `proxy` at `implementation`.
fn point(db: &mut InMemoryDB, proxy: Address, implementation: Address) {
    db.insert_account_storage(proxy, U256::ZERO, implementation.into_word().into())
        .expect("the fixture proxy's slot is writable");
}

fn good_hash() -> B256 {
    keccak256(emitting_impl_code(0x01))
}

/// One call-frame rule per proxy: deny when the code it delegated into is not approved.
fn proxy_guard(id: &str, proxy: Address) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{proxy:#x}") } },
        "condition": { "!": { "in": [
            { "var": "entry.delegateCodeHash" }, [format!("{:#x}", good_hash())]
        ] } },
        "action": "deny"
    })
}

/// The quota rule: every `Transfer` a proxy emits needs adjudication.
fn quota_rule() -> serde_json::Value {
    serde_json::json!({
        "id": "quota-on-proxy-transfer",
        "event_abis": { "transfer": {
            "type": "event", "name": "Transfer",
            "inputs": [
                { "name": "from", "type": "address", "indexed": true },
                { "name": "to", "type": "address", "indexed": true },
                { "name": "value", "type": "uint256", "indexed": false }
            ],
            "anonymous": false
        } },
        "condition": { "in": [
            { "var": "transfer.address" }, [format!("{P1:#x}"), format!("{P2:#x}")]
        ] },
        "action": "audit"
    })
}

/// A call-frame rule on the blacklist proxy itself that objects to its implementation: the
/// synthetic bans' own execution is then objected to.
fn blacklist_guard() -> serde_json::Value {
    serde_json::json!({
        "id": "blacklist-guard",
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "bl": format!("{TX_BLACKLIST:#x}") } },
        "condition": { "==": [
            { "var": "bl.delegateCodeHash" }, format!("{:#x}", keccak256(tx_blacklist_impl_code()))
        ] },
        "action": "deny"
    })
}

fn default_rules() -> Vec<serde_json::Value> {
    vec![proxy_guard("p1-guard", P1), proxy_guard("p2-guard", P2), quota_rule()]
}

/// The `source_hash` RCS derives for a user deposit from the L1 log it decoded
/// (`decode.go`, `userDepositSourceHash`).
fn l1_user_deposit_source_hash(l1_block_hash: B256, log_index: u64) -> B256 {
    let mut deposit_id = [0u8; 64];
    deposit_id[..32].copy_from_slice(l1_block_hash.as_slice());
    deposit_id[56..].copy_from_slice(&log_index.to_be_bytes());
    let mut domain = [0u8; 64];
    domain[32..].copy_from_slice(keccak256(deposit_id).as_slice());
    keccak256(domain)
}

fn original(log_index: u64, to: Address) -> DepositTxRequest {
    DepositTxRequest {
        source_hash: format!(
            "{:#x}",
            l1_user_deposit_source_hash(keccak256("replay-l1-block"), log_index)
        ),
        from: format!("{DEPOSITOR:#x}"),
        to: Some(format!("{to:#x}")),
        mint: "0".to_string(),
        value: "0".to_string(),
        gas_limit: 1_000_000,
        is_system_transaction: false,
        data: "0x".to_string(),
    }
}

/// RCS's `BuildBlacklistSimulation`: one unsigned deposit per batch of at most 500 hashes, with
/// the canonical `batchAddToBlacklist(bytes32[])` calldata and the domain-separated
/// `source_hash` over exactly those hashes.
fn build_blacklist_simulation(source_hashes: &[String]) -> Vec<DepositTxRequest> {
    source_hashes
        .chunks(MAX_SYNTHETIC_BAN_BATCH)
        .map(|batch| {
            let hashes: Vec<B256> =
                batch.iter().map(|h| h.parse().expect("a denied source hash")).collect();
            let mut data = selector(BATCH_ADD_TO_BLACKLIST_SIGNATURE).to_vec();
            data.extend_from_slice(&U256::from(32u64).to_be_bytes::<32>());
            data.extend_from_slice(&U256::from(hashes.len()).to_be_bytes::<32>());
            let mut material = Keccak256::new();
            material.update(SYNTHETIC_BAN_SOURCE_HASH_DOMAIN);
            for hash in &hashes {
                data.extend_from_slice(hash.as_slice());
                material.update(hash.as_slice());
            }
            DepositTxRequest {
                source_hash: format!("{:#x}", material.finalize()),
                from: format!("{OPERATOR:#x}"),
                to: Some(format!("{TX_BLACKLIST:#x}")),
                mint: "0".to_string(),
                value: "0".to_string(),
                gas_limit: 1_000_000,
                is_system_transaction: false,
                data: format!("0x{}", hex::encode(data)),
            }
        })
        .collect()
}

fn request_json(req: &DepositTxRequest) -> serde_json::Value {
    serde_json::json!({
        "source_hash": req.source_hash,
        "from": req.from,
        "to": req.to,
        "mint": req.mint,
        "value": req.value,
        "gas_limit": req.gas_limit,
        "is_system_transaction": req.is_system_transaction,
        "data": req.data,
    })
}

fn devnet_env() -> EvmEnv<OpSpecId> {
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::JOVIAN);
    cfg.chain_id = DEVNET_CHAIN_ID;
    EvmEnv::new(cfg, BlockEnv { gas_limit: 30_000_000, ..Default::default() })
}

// --------------------------------------------------------------------------- driver -------

/// How the driver labels its request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Labels {
    /// No `known_allowed_kinds` at all — what RCS sends.
    None,
    /// A complete, correct map.
    Correct,
    /// A complete map that labels every synthetic ban `original`.
    BansAsOriginal,
}

/// One production audit call: the real decode, plan, deposit build, inspector choice, batch body
/// and result assembly, on `base`.
fn audit_call(
    rules: &[serde_json::Value],
    txs: &[DepositTxRequest],
    known_allowed: &[String],
    kinds: Option<BTreeMap<String, KnownAllowedKind>>,
    base: &InMemoryDB,
) -> Vec<AuditResult> {
    let mut body = serde_json::json!({
        "rules": rules,
        "txs": txs.iter().map(request_json).collect::<Vec<_>>(),
        "known_allowed": known_allowed,
    });
    if let Some(kinds) = kinds {
        let map: serde_json::Map<String, serde_json::Value> = kinds
            .into_iter()
            .map(|(hash, kind)| {
                let label = match kind {
                    KnownAllowedKind::Original => "original",
                    KnownAllowedKind::SyntheticBan => "synthetic_ban",
                };
                (hash, label.into())
            })
            .collect();
        body["known_allowed_kinds"] = map.into();
    }
    let req: AuditTransactionsRequest = serde_json::from_value(body).expect("the request decodes");
    let plan = AuditBatchPlan::prepare(&req).expect("the request passes every check");

    let mut ordered_txs = Vec::new();
    let mut ordered_reqs = Vec::new();
    let mut built = Vec::new();
    for tx_req in &req.txs {
        match build_deposit_tx(tx_req) {
            Ok(tx) => {
                built.push(Some(ordered_txs.len()));
                ordered_reqs.push(tx_req.clone());
                ordered_txs.push(tx);
            }
            Err(_) => built.push(None),
        }
    }
    let mut executed: Vec<Option<AuditResult>> = (0..ordered_txs.len()).map(|_| None).collect();
    let batch = plan.batch(&ordered_txs, &ordered_reqs);
    let capture = AuditCaptureConfig { max_callstack_observations_per_tx: 10_000 };
    if let Some(inspector) = audit_capture_inspector(plan.rules(), capture) {
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
            base.clone(),
            devnet_env(),
            inspector,
        );
        run_audit_batch(&mut evm, &batch, &mut executed).expect("the batch executes");
    } else {
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm(base.clone(), devnet_env());
        run_audit_batch(&mut evm, &batch, &mut executed).expect("the batch executes");
    }
    assemble_results(&req.txs, &built, &mut executed)
}

/// `replay_state.go`, line for line.
struct ReplayState {
    originals: Vec<DepositTxRequest>,
    simulation_prefix: Vec<DepositTxRequest>,
    known_allowed: Vec<String>,
    terminal_denied: HashSet<String>,
    queued_denied: Vec<String>,
    request: Vec<DepositTxRequest>,
    body_start: usize,
    newly_denied: Vec<String>,
}

impl ReplayState {
    fn new(originals: Vec<DepositTxRequest>) -> Self {
        Self {
            originals,
            simulation_prefix: Vec::new(),
            known_allowed: Vec::new(),
            terminal_denied: HashSet::new(),
            queued_denied: Vec::new(),
            request: Vec::new(),
            body_start: 0,
            newly_denied: Vec::new(),
        }
    }

    fn begin_round(&mut self) -> (Vec<DepositTxRequest>, Vec<String>) {
        self.request = self.simulation_prefix.clone();
        self.body_start = self.request.len();
        self.request.extend(self.originals.iter().cloned());
        self.newly_denied.clear();
        (self.request.clone(), self.known_allowed.clone())
    }

    fn is_simulation(&self, index: usize) -> bool {
        index < self.body_start
    }

    fn queue_denied(&mut self, index: usize) -> bool {
        let source_hash = self.request[index].source_hash.clone();
        if !self.transition_to_denied(&source_hash) {
            return false;
        }
        self.queued_denied.push(source_hash.clone());
        self.newly_denied.push(source_hash);
        true
    }

    fn transition_to_denied(&mut self, source_hash: &str) -> bool {
        if !self.terminal_denied.insert(source_hash.to_string()) {
            return false;
        }
        self.known_allowed.retain(|allowed| allowed != source_hash);
        true
    }

    fn transition_to_allowed(&mut self, source_hash: &str) {
        if self.terminal_denied.contains(source_hash)
            || self.known_allowed.iter().any(|allowed| allowed == source_hash)
        {
            return;
        }
        self.known_allowed.push(source_hash.to_string());
    }

    fn append_simulation_prefix(&mut self, simulations: Vec<DepositTxRequest>) {
        for simulation in &simulations {
            self.transition_to_allowed(&simulation.source_hash);
        }
        self.simulation_prefix.extend(simulations);
    }
}

/// RCS's quota adjudication, replaced by a fixed decision per deposit.
#[derive(Clone, Copy)]
enum Quota {
    Approved,
    Denied,
}

/// One round as the driver saw it.
#[derive(Debug)]
struct Round {
    request: Vec<String>,
    known_allowed: Vec<String>,
    verdicts: Vec<Verdict>,
}

struct ReplayOutcome {
    rounds: Vec<Round>,
    terminal_denied: HashSet<String>,
    /// What RCS would submit with `SubmitBlacklist` after the loop.
    blacklist: Vec<String>,
    /// The last round's request and the base state it ran on.
    final_request: Vec<DepositTxRequest>,
    final_base: InMemoryDB,
}

/// Runs the replay loop to termination. `before_round(k, base)` may move the chain tip before
/// round `k` (0-based).
fn replay(
    rules: &[serde_json::Value],
    originals: Vec<DepositTxRequest>,
    quota: &HashMap<String, Quota>,
    labels: Labels,
    mut before_round: impl FnMut(usize, &mut InMemoryDB),
) -> ReplayOutcome {
    let mut state = ReplayState::new(originals);
    let mut base = base_state();
    let mut rounds = Vec::new();
    loop {
        assert!(rounds.len() < 10, "the replay must converge: {rounds:?}");
        before_round(rounds.len(), &mut base);
        let (request, known_allowed) = state.begin_round();
        let kinds = match labels {
            Labels::None => None,
            Labels::Correct | Labels::BansAsOriginal => Some(
                known_allowed
                    .iter()
                    .map(|hash| {
                        let is_ban = state.simulation_prefix.iter().any(|s| &s.source_hash == hash);
                        let kind = if is_ban && labels == Labels::Correct {
                            KnownAllowedKind::SyntheticBan
                        } else {
                            KnownAllowedKind::Original
                        };
                        (hash.clone(), kind)
                    })
                    .collect(),
            ),
        };
        let results = audit_call(rules, &request, &known_allowed, kinds, &base);

        // Correlated by position and checked by source hash, as RCS does.
        assert_eq!(results.len(), request.len(), "one result per request deposit");
        for (result, deposit) in results.iter().zip(&request) {
            assert!(
                result.source_hash.eq_ignore_ascii_case(&deposit.source_hash),
                "result/request source hash mismatch"
            );
        }
        rounds.push(Round {
            request: request.iter().map(|d| d.source_hash.clone()).collect(),
            known_allowed: known_allowed.clone(),
            verdicts: results.iter().map(|r| r.verdict).collect(),
        });

        let mut saw_unknown = false;
        for (index, result) in results.iter().enumerate() {
            if state.is_simulation(index) {
                continue;
            }
            let source_hash = request[index].source_hash.clone();
            match result.verdict {
                Verdict::Allow | Verdict::Malformed => {}
                Verdict::Deny => {
                    if state.queue_denied(index) {
                        break;
                    }
                }
                Verdict::Audit => match quota[&source_hash] {
                    Quota::Approved => state.transition_to_allowed(&source_hash),
                    Quota::Denied => {
                        if state.queue_denied(index) {
                            break;
                        }
                    }
                },
                Verdict::Unknown => saw_unknown = true,
            }
        }
        let newly_denied = state.newly_denied.clone();
        let prefix_grew = !newly_denied.is_empty();
        if prefix_grew {
            state.append_simulation_prefix(build_blacklist_simulation(&newly_denied));
        }
        if !saw_unknown && !prefix_grew {
            break;
        }
    }
    ReplayOutcome {
        rounds,
        terminal_denied: state.terminal_denied,
        blacklist: state.queued_denied,
        final_request: state.request,
        final_base: base,
    }
}

/// How one deposit of the final round executes on chain: the same ordered sequence, every
/// transaction committed, on the plain devnet EVM.
#[derive(Debug, PartialEq, Eq)]
enum Status {
    Success { logs: usize },
    FailedDeposit,
    Other,
}

fn final_statuses(outcome: &ReplayOutcome) -> Vec<Status> {
    let mut evm =
        OpEvmFactory::<OpTx>::default().create_evm(outcome.final_base.clone(), devnet_env());
    outcome
        .final_request
        .iter()
        .map(|req| {
            let tx = build_deposit_tx(req).expect("the request builds");
            let ResultAndState { result, state } = evm.transact(&tx).expect("the deposit executes");
            evm.db_mut().commit(state);
            match result {
                ExecutionResult::Success { logs, .. } => Status::Success { logs: logs.len() },
                ExecutionResult::Halt { reason: OpHaltReason::FailedDeposit, .. } => {
                    Status::FailedDeposit
                }
                _ => Status::Other,
            }
        })
        .collect()
}

fn hash_of(req: &DepositTxRequest) -> String {
    req.source_hash.clone()
}

use Verdict::{Allow, Audit, Deny, Unknown};

// ---------------------------------------------------------------------------- tests -------

#[test]
fn the_driver_builds_synthetic_bans_exactly_like_the_golden_vectors() {
    for ban in crate::audit::synthetic_ban::tests::golden_bans() {
        let elements: Vec<String> = ban.elements.iter().map(|e| format!("{e:#x}")).collect();
        let built = build_blacklist_simulation(&elements);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].data, ban.data, "n = {}", ban.n);
        assert_eq!(built[0].source_hash, ban.source_hash, "n = {}", ban.n);
    }
    let doc: serde_json::Value =
        serde_json::from_str(crate::audit::synthetic_ban::tests::GOLDEN).expect("JSON");
    let l1 = &doc["l1_user_deposit"];
    assert_eq!(
        format!(
            "{:#x}",
            l1_user_deposit_source_hash(
                l1["l1_block_hash"].as_str().unwrap().parse().unwrap(),
                l1["log_index"].as_u64().unwrap()
            )
        ),
        l1["source_hash"].as_str().unwrap(),
        "the driver derives original source hashes exactly as RCS does"
    );
}

#[test]
fn the_blacklist_fixture_is_what_the_native_interceptor_reads() {
    // A deposit whose hash a committed ban listed is a failed deposit; the same deposit before
    // the ban executes normally. Without this, every replay assertion below could pass on a
    // fixture the interceptor never consults.
    let a = original(1, P1);
    let ban = build_blacklist_simulation(&[hash_of(&a)]).remove(0);
    let run = |txs: Vec<DepositTxRequest>| {
        final_statuses(&ReplayOutcome {
            rounds: Vec::new(),
            terminal_denied: HashSet::new(),
            blacklist: Vec::new(),
            final_request: txs,
            final_base: base_state(),
        })
    };
    assert_eq!(run(vec![a.clone()]), vec![Status::Success { logs: 1 }]);
    assert_eq!(
        run(vec![ban, a]),
        vec![Status::Success { logs: 0 }, Status::FailedDeposit],
        "the committed ban makes the native interceptor fail the deposit"
    );
}

/// An approved original is re-denied when its proxy is upgraded between rounds, and ends as
/// a failed deposit on the blacklist.
#[test]
fn an_original_approved_earlier_is_re_denied_after_an_upgrade_and_ends_as_a_failed_deposit() {
    let a = original(1, P1);
    let w = original(2, PLAIN);
    let quota = HashMap::from([(hash_of(&a), Quota::Approved)]);
    let outcome =
        replay(&default_rules(), vec![a.clone(), w.clone()], &quota, Labels::None, |k, base| {
            if k == 1 {
                point(base, P1, BAD_IMPL);
            }
        });
    let ban_a = build_blacklist_simulation(&[hash_of(&a)]).remove(0);

    assert_eq!(outcome.rounds.len(), 3, "{:?}", outcome.rounds);
    // Round 1: `a` needs quota adjudication and stops the batch.
    assert_eq!(outcome.rounds[0].known_allowed, Vec::<String>::new());
    assert_eq!(outcome.rounds[0].verdicts, vec![Audit, Unknown]);
    // Round 2: `a` is known-allowed and unlabelled, and is now re-judged and denied.
    assert_eq!(outcome.rounds[1].known_allowed, vec![hash_of(&a)]);
    assert_eq!(outcome.rounds[1].verdicts, vec![Deny, Allow]);
    // Round 3: the ban prefix commits; `a` is a failed deposit and therefore clean.
    assert_eq!(outcome.rounds[2].request, vec![hash_of(&ban_a), hash_of(&a), hash_of(&w)]);
    assert_eq!(outcome.rounds[2].known_allowed, vec![hash_of(&ban_a)]);
    assert_eq!(outcome.rounds[2].verdicts, vec![Allow, Allow, Allow]);

    assert_eq!(outcome.terminal_denied, HashSet::from([hash_of(&a)]));
    assert_eq!(outcome.blacklist, vec![hash_of(&a)], "the would-be submitted blacklist");
    assert_eq!(
        final_statuses(&outcome),
        vec![Status::Success { logs: 0 }, Status::FailedDeposit, Status::Success { logs: 0 }]
    );
}

/// Of two approved originals only the one whose proxy moved is denied.
#[test]
fn only_the_original_whose_proxy_was_upgraded_is_denied() {
    let a1 = original(1, P1);
    let a2 = original(2, P2);
    let w = original(3, PLAIN);
    let quota = HashMap::from([(hash_of(&a1), Quota::Approved), (hash_of(&a2), Quota::Approved)]);
    let outcome = replay(
        &default_rules(),
        vec![a1.clone(), a2.clone(), w.clone()],
        &quota,
        Labels::None,
        |k, base| {
            if k == 2 {
                point(base, P2, BAD_IMPL);
            }
        },
    );
    let ban_a2 = build_blacklist_simulation(&[hash_of(&a2)]).remove(0);

    assert_eq!(outcome.rounds.len(), 4, "{:?}", outcome.rounds);
    assert_eq!(outcome.rounds[0].verdicts, vec![Audit, Unknown, Unknown]);
    assert_eq!(outcome.rounds[1].verdicts, vec![Allow, Audit, Unknown]);
    assert_eq!(outcome.rounds[2].known_allowed, vec![hash_of(&a1), hash_of(&a2)]);
    assert_eq!(outcome.rounds[2].verdicts, vec![Allow, Deny, Allow]);
    assert_eq!(outcome.rounds[3].request[0], hash_of(&ban_a2));
    assert_eq!(outcome.rounds[3].known_allowed, vec![hash_of(&a1), hash_of(&ban_a2)]);
    assert_eq!(outcome.rounds[3].verdicts, vec![Allow, Allow, Allow, Allow]);

    assert_eq!(outcome.blacklist, vec![hash_of(&a2)]);
    assert_eq!(
        final_statuses(&outcome),
        vec![
            Status::Success { logs: 0 },
            Status::Success { logs: 1 },
            Status::FailedDeposit,
            Status::Success { logs: 0 },
        ],
        "a1 still executes; a2 is a failed deposit"
    );
}

/// A ban prefix whose own execution a rule objects to still commits and stays `allow`.
#[test]
fn a_ban_prefix_commits_even_when_its_execution_is_objected() {
    let a = original(1, P1);
    let w = original(2, PLAIN);
    let quota = HashMap::from([(hash_of(&a), Quota::Approved)]);
    let mut rules = default_rules();
    rules.push(blacklist_guard());
    let outcome = replay(&rules, vec![a.clone(), w.clone()], &quota, Labels::None, |k, base| {
        if k == 1 {
            point(base, P1, BAD_IMPL);
        }
    });

    assert_eq!(outcome.rounds.len(), 3, "{:?}", outcome.rounds);
    assert_eq!(outcome.rounds[0].verdicts, vec![Audit, Unknown]);
    assert_eq!(outcome.rounds[1].verdicts, vec![Deny, Allow]);
    assert_eq!(outcome.rounds[2].verdicts, vec![Allow, Allow, Allow]);
    assert_eq!(outcome.blacklist, vec![hash_of(&a)]);
    assert_eq!(final_statuses(&outcome)[1], Status::FailedDeposit);

    // The prefix really was objected to, and was still allowed and committed.
    let ban_a = build_blacklist_simulation(&[hash_of(&a)]).remove(0);
    let results =
        audit_call(&rules, std::slice::from_ref(&ban_a), &[hash_of(&ban_a)], None, &base_state());
    assert_eq!(results[0].verdict, Allow);
    assert_eq!(results[0].callstack_identification, Some(CallstackIdentification::Objected));
}

/// With no call-frame rule in the request nothing changes: approved originals stay
/// approved whatever their proxy now runs, exactly as before.
#[test]
fn a_request_without_call_frame_rules_behaves_as_before() {
    let a = original(1, P1);
    let w = original(2, PLAIN);
    let quota = HashMap::from([(hash_of(&a), Quota::Approved)]);
    let outcome = replay(&[quota_rule()], vec![a.clone(), w], &quota, Labels::None, |k, base| {
        if k == 1 {
            point(base, P1, BAD_IMPL);
        }
    });
    // Recorded expectation of the previous behaviour for this sequence: round 1 audits `a`,
    // round 2 allows the approved `a` and the plain deposit, and the loop ends with nothing
    // denied.
    assert_eq!(outcome.rounds.len(), 2, "{:?}", outcome.rounds);
    assert_eq!(outcome.rounds[0].verdicts, vec![Audit, Unknown]);
    assert_eq!(outcome.rounds[1].verdicts, vec![Allow, Allow]);
    assert!(outcome.blacklist.is_empty());
    assert_eq!(
        final_statuses(&outcome),
        vec![Status::Success { logs: 1 }, Status::Success { logs: 0 }]
    );
}

/// A complete correct label map reaches the same result as inference does; a map that labels the ban
/// `original` shows the explicit-label contract: labels win, and an objected ban then loses its
/// commit.
#[test]
fn labels_win_over_inference() {
    let a = original(1, P1);
    let w = original(2, PLAIN);
    let quota = HashMap::from([(hash_of(&a), Quota::Approved)]);
    let upgrade = |k: usize, base: &mut InMemoryDB| {
        if k == 1 {
            point(base, P1, BAD_IMPL);
        }
    };

    let labelled =
        replay(&default_rules(), vec![a.clone(), w.clone()], &quota, Labels::Correct, upgrade);
    let inferred =
        replay(&default_rules(), vec![a.clone(), w.clone()], &quota, Labels::None, upgrade);
    assert_eq!(
        labelled.rounds.iter().map(|r| &r.verdicts).collect::<Vec<_>>(),
        inferred.rounds.iter().map(|r| &r.verdicts).collect::<Vec<_>>()
    );
    assert_eq!(labelled.blacklist, inferred.blacklist);
    assert_eq!(final_statuses(&labelled), final_statuses(&inferred));

    let mut rules = default_rules();
    rules.push(blacklist_guard());
    let mislabelled =
        replay(&rules, vec![a.clone(), w.clone()], &quota, Labels::BansAsOriginal, upgrade);
    let last = mislabelled.rounds.last().expect("a round ran");
    assert_eq!(last.verdicts[0], Deny, "a ban labelled original is adjudicated as one");
    assert_eq!(
        last.verdicts[1], Deny,
        "its commit was withheld, so `a` was not blacklisted in the simulation and ran its payload"
    );
    assert_eq!(mislabelled.blacklist, vec![hash_of(&a)], "RCS-side state is unaffected");
}

/// A deposit the quota adjudicator denies in the first round becomes a ban prefix and a failed
/// deposit, while an approved one in the same body keeps executing — the pre-existing quota path,
/// unchanged by the call-frame inference.
#[test]
fn a_quota_denied_original_is_blacklisted_and_an_approved_one_keeps_executing() {
    let denied = original(1, P1);
    let approved = original(2, P2);
    let quota =
        HashMap::from([(hash_of(&denied), Quota::Denied), (hash_of(&approved), Quota::Approved)]);
    let outcome = replay(
        &default_rules(),
        vec![denied.clone(), approved.clone()],
        &quota,
        Labels::None,
        |_, _| {},
    );
    assert_eq!(outcome.rounds.len(), 2, "{:?}", outcome.rounds);
    assert_eq!(outcome.rounds[0].verdicts, vec![Audit, Unknown]);
    // Round 2: the ban commits, the denied deposit is a failed deposit, and the other deposit
    // reaches its own first adjudication; nothing is unknown afterwards, so the loop ends.
    assert_eq!(outcome.rounds[1].verdicts, vec![Allow, Allow, Audit]);
    assert_eq!(outcome.blacklist, vec![hash_of(&denied)]);
    assert_eq!(
        final_statuses(&outcome),
        vec![Status::Success { logs: 0 }, Status::FailedDeposit, Status::Success { logs: 1 }]
    );
}
