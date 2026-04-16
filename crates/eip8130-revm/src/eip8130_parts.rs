//! Pre-computed execution data for EIP-8130 AA transactions.
//!
//! [`Eip8130Parts`] aggregates all the data the handler needs during EVM
//! execution. It is built **once** from a validated [`TxEip8130`] before the
//! execution pipeline starts, so the handler can focus on orchestration
//! instead of re-computing gas breakdowns or re-parsing account changes on
//! every step.

use alloy_primitives::{Address, Bytes, B256, U256};
use xlayer_eip8130_consensus::{
    build_execution_calls, gas_refund, intrinsic_gas, max_execution_gas_cost,
    nonce_increment_write, CodePlacement, ExecutionCall, PhaseResult, StorageWrite,
    TxContextValues, TxEip8130,
};

use crate::tx_context::Eip8130TxContext;

/// Pre-computed data extracted from a validated [`TxEip8130`] before
/// EVM execution begins.
///
/// The handler constructs this once and references it throughout the
/// execution pipeline (gas deduction → nonce increment → calls → refund).
#[derive(Debug, Clone)]
pub struct Eip8130Parts {
    // ── Identity ──────────────────────────────────────────────────────
    /// Resolved sender address.
    pub sender: Address,
    /// Resolved payer address (= sender for self-pay).
    pub payer: Address,
    /// Authenticated owner ID (`bytes32`) for the sender.
    pub sender_owner_id: B256,
    /// Authenticated owner ID for the payer (same as sender_owner_id
    /// when self-pay).
    pub payer_owner_id: B256,

    // ── Nonce ─────────────────────────────────────────────────────────
    /// The 2D nonce key.
    pub nonce_key: U256,
    /// The expected nonce sequence.
    pub nonce_sequence: u64,
    /// Whether the nonce key is warm (already seen in this block).
    pub nonce_key_is_warm: bool,

    // ── Gas ───────────────────────────────────────────────────────────
    /// Intrinsic gas for this transaction (calldata + base + verification).
    pub intrinsic_gas: u64,
    /// The `gas_limit` field from the transaction (execution gas only).
    pub execution_gas_limit: u64,
    /// `max_fee_per_gas` from the transaction.
    pub max_fee_per_gas: u128,
    /// `max_priority_fee_per_gas` from the transaction.
    pub max_priority_fee_per_gas: u128,
    /// Maximum cost the payer must cover: `(gas_limit + intrinsic) * max_fee_per_gas`.
    pub max_payer_cost: U256,

    // ── Execution calls ───────────────────────────────────────────────
    /// Phased execution calls built from `tx.calls`.
    pub execution_calls: Vec<Vec<ExecutionCall>>,

    // ── Account changes ───────────────────────────────────────────────
    /// Pre-computed storage writes for account changes (nonce, owners,
    /// config). Populated lazily by the handler when processing
    /// `account_changes`.
    pub nonce_write: StorageWrite,
    /// Whether the sender needs auto-delegation (bare EOA without code).
    pub needs_auto_delegate: bool,
    /// Code placements for CREATE2 deployments.
    pub code_placements: Vec<CodePlacement>,
    /// Config-change storage writes.
    pub config_writes: Vec<StorageWrite>,
    /// Config-change sequence updates: `(slot, is_multichain, new_value)`.
    pub sequence_updates: Vec<(U256, bool, u64)>,

    // ── Transaction expiry ────────────────────────────────────────────
    /// The expiry timestamp (0 = no expiry).
    pub expiry: u64,
    /// The chain ID.
    pub chain_id: u64,

    // ── Phase results (populated during execution) ────────────────────
    /// Phase execution results, filled in by the handler.
    pub phase_results: Vec<PhaseResult>,
}

impl Eip8130Parts {
    /// Builds pre-computed parts from a validated transaction.
    ///
    /// # Arguments
    /// - `tx` — The validated EIP-8130 transaction.
    /// - `sender` — Resolved sender address.
    /// - `payer` — Resolved payer address.
    /// - `sender_owner_id` — Authenticated owner ID for sender.
    /// - `payer_owner_id` — Authenticated owner ID for payer.
    /// - `nonce_key_is_warm` — Whether the nonce key slot is already warm.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tx: &TxEip8130,
        sender: Address,
        payer: Address,
        sender_owner_id: B256,
        payer_owner_id: B256,
        nonce_key_is_warm: bool,
    ) -> Self {
        let intrinsic = intrinsic_gas(tx, nonce_key_is_warm, tx.chain_id);
        let max_payer_cost = max_execution_gas_cost(tx);
        let execution_calls = build_execution_calls(tx, sender);
        let nonce_write =
            nonce_increment_write(sender, tx.nonce_key, tx.nonce_sequence.wrapping_add(1));

        // Pre-compute account change artifacts
        let mut code_placements = Vec::new();
        let mut config_writes = Vec::new();
        let mut sequence_updates = Vec::new();

        for entry in &tx.account_changes {
            match entry {
                xlayer_eip8130_consensus::AccountChangeEntry::Create(create) => {
                    let salt = xlayer_eip8130_consensus::effective_salt(
                        create.user_salt,
                        &create.initial_owners,
                    );
                    let addr =
                        xlayer_eip8130_consensus::create2_address(sender, salt, &create.bytecode);
                    code_placements
                        .push(CodePlacement { address: addr, code: create.bytecode.clone() });
                    let writes = xlayer_eip8130_consensus::owner_registration_writes(addr, create);
                    config_writes.extend(writes);
                }
                xlayer_eip8130_consensus::AccountChangeEntry::ConfigChange(cc) => {
                    let writes = xlayer_eip8130_consensus::config_change_writes(sender, cc);
                    config_writes.extend(writes);
                    let seq_info = xlayer_eip8130_consensus::config_change_sequence(sender, cc);
                    sequence_updates.push((
                        seq_info.slot,
                        seq_info.is_multichain,
                        seq_info.new_value,
                    ));
                }
                xlayer_eip8130_consensus::AccountChangeEntry::Delegation(_) => {
                    // Delegation is handled directly by the handler via EIP-7702.
                }
            }
        }

        Self {
            sender,
            payer,
            sender_owner_id,
            payer_owner_id,
            nonce_key: tx.nonce_key,
            nonce_sequence: tx.nonce_sequence,
            nonce_key_is_warm,
            intrinsic_gas: intrinsic,
            execution_gas_limit: tx.gas_limit,
            max_fee_per_gas: tx.max_fee_per_gas,
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas,
            max_payer_cost,
            execution_calls,
            nonce_write,
            needs_auto_delegate: false, // Set by handler after checking code
            code_placements,
            config_writes,
            sequence_updates,
            expiry: tx.expiry,
            chain_id: tx.chain_id,
            phase_results: Vec::new(),
        }
    }

    /// Total gas limit (intrinsic + execution).
    pub fn total_gas_limit(&self) -> u64 {
        self.intrinsic_gas.saturating_add(self.execution_gas_limit)
    }

    /// Whether the transaction is self-pay (sender == payer).
    pub fn is_self_pay(&self) -> bool {
        self.sender == self.payer
    }

    /// Computes the gas refund for unused gas.
    pub fn compute_refund(&self, gas_used: u64, effective_gas_price: u128) -> U256 {
        gas_refund(self.total_gas_limit(), gas_used, effective_gas_price)
    }

    /// Builds a [`TxContextValues`] for the TxContext precompile.
    pub fn to_tx_context_values(&self) -> TxContextValues {
        let calls: Vec<Vec<(Address, Bytes)>> = self
            .execution_calls
            .iter()
            .map(|phase| phase.iter().map(|c| (c.to, c.data.clone())).collect())
            .collect();

        TxContextValues {
            sender: self.sender,
            payer: self.payer,
            owner_id: self.sender_owner_id,
            gas_limit: self.execution_gas_limit,
            max_cost: self.max_payer_cost,
            calls,
        }
    }

    /// Builds an [`Eip8130TxContext`] for the thread-local precompile store.
    pub fn to_eip8130_tx_context(&self) -> Eip8130TxContext {
        let call_phases: Vec<Vec<(Address, Bytes)>> = self
            .execution_calls
            .iter()
            .map(|phase| phase.iter().map(|c| (c.to, c.data.clone())).collect())
            .collect();

        Eip8130TxContext {
            sender: self.sender,
            payer: self.payer,
            owner_id: self.sender_owner_id,
            gas_limit: self.execution_gas_limit,
            max_cost: self.max_payer_cost,
            call_phases,
        }
    }
}

/// Result of an AA transaction execution, used to build the receipt.
#[derive(Debug, Clone)]
pub struct Eip8130ExecutionResult {
    /// Whether the overall transaction succeeded.
    pub success: bool,
    /// Total gas used (intrinsic + execution).
    pub gas_used: u64,
    /// Per-phase execution results.
    pub phase_results: Vec<PhaseResult>,
    /// Effective gas price used for refund calculation.
    pub effective_gas_price: u128,
    /// Gas refund amount returned to payer.
    pub gas_refund: U256,
}

/// Gas accounting helper for the execution pipeline.
#[derive(Debug, Clone)]
pub struct Eip8130GasAccounting {
    /// Total gas budget (intrinsic + execution).
    total_budget: u64,
    /// Gas consumed so far.
    consumed: u64,
}

impl Eip8130GasAccounting {
    /// Creates a new gas accounting instance.
    pub fn new(parts: &Eip8130Parts) -> Self {
        Self { total_budget: parts.total_gas_limit(), consumed: parts.intrinsic_gas }
    }

    /// Records gas consumed by a phase.
    pub fn record_phase(&mut self, gas_used: u64) {
        self.consumed = self.consumed.saturating_add(gas_used);
    }

    /// Returns the remaining gas budget.
    pub fn remaining(&self) -> u64 {
        self.total_budget.saturating_sub(self.consumed)
    }

    /// Returns total gas consumed.
    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    /// Returns the total budget.
    pub fn total_budget(&self) -> u64 {
        self.total_budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xlayer_eip8130_consensus::TxEip8130;

    fn make_simple_tx() -> TxEip8130 {
        TxEip8130 {
            chain_id: 1,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            max_priority_fee_per_gas: 1,
            nonce_key: U256::from(0),
            nonce_sequence: 5,
            ..Default::default()
        }
    }

    #[test]
    fn parts_from_simple_tx() {
        let tx = make_simple_tx();
        let sender = Address::repeat_byte(0xAA);
        let payer = sender;
        let owner_id = B256::repeat_byte(0x11);

        let parts = Eip8130Parts::new(&tx, sender, payer, owner_id, owner_id, false);

        assert_eq!(parts.sender, sender);
        assert_eq!(parts.payer, payer);
        assert!(parts.is_self_pay());
        assert_eq!(parts.nonce_sequence, 5);
        assert_eq!(parts.execution_gas_limit, 100_000);
        assert!(parts.intrinsic_gas > 0);
        assert_eq!(parts.total_gas_limit(), parts.intrinsic_gas + 100_000);
    }

    #[test]
    fn parts_with_external_payer() {
        let tx = make_simple_tx();
        let sender = Address::repeat_byte(0xAA);
        let payer = Address::repeat_byte(0xBB);
        let sender_oid = B256::repeat_byte(0x11);
        let payer_oid = B256::repeat_byte(0x22);

        let parts = Eip8130Parts::new(&tx, sender, payer, sender_oid, payer_oid, false);

        assert!(!parts.is_self_pay());
        assert_eq!(parts.payer, payer);
        assert_eq!(parts.payer_owner_id, payer_oid);
    }

    #[test]
    fn to_tx_context_values_roundtrip() {
        let tx = make_simple_tx();
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);

        let parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);
        let values = parts.to_tx_context_values();

        assert_eq!(values.sender, sender);
        assert_eq!(values.payer, sender);
        assert_eq!(values.owner_id, owner_id);
        assert_eq!(values.gas_limit, 100_000);
    }

    #[test]
    fn to_eip8130_tx_context_roundtrip() {
        let tx = make_simple_tx();
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);

        let parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);
        let ctx = parts.to_eip8130_tx_context();

        assert_eq!(ctx.sender, sender);
        assert_eq!(ctx.payer, sender);
        assert_eq!(ctx.owner_id, owner_id);
        assert_eq!(ctx.gas_limit, 100_000);
    }

    #[test]
    fn gas_accounting_tracks_consumption() {
        let tx = make_simple_tx();
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);
        let parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);

        let mut accounting = Eip8130GasAccounting::new(&parts);

        // Starts with intrinsic gas consumed.
        assert_eq!(accounting.consumed(), parts.intrinsic_gas);
        let initial_remaining = accounting.remaining();

        // Record a phase.
        accounting.record_phase(50_000);
        assert_eq!(accounting.consumed(), parts.intrinsic_gas + 50_000);
        assert_eq!(accounting.remaining(), initial_remaining - 50_000);
    }

    #[test]
    fn gas_accounting_saturates() {
        let tx = make_simple_tx();
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);
        let parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);

        let mut accounting = Eip8130GasAccounting::new(&parts);
        // Record more gas than the budget.
        accounting.record_phase(u64::MAX);
        assert_eq!(accounting.remaining(), 0);
    }

    #[test]
    fn nonce_write_targets_correct_sequence() {
        let tx = make_simple_tx();
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);

        let parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);

        // The nonce write should target the NonceManager address.
        assert_eq!(parts.nonce_write.address, xlayer_eip8130_consensus::NONCE_MANAGER_ADDRESS);
    }

    #[test]
    fn compute_refund_for_partial_usage() {
        let tx = make_simple_tx();
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);
        let parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);

        let total = parts.total_gas_limit();
        let gas_used = total / 2;
        let gas_price = 10u128;

        let refund = parts.compute_refund(gas_used, gas_price);
        let expected = U256::from(total - gas_used) * U256::from(gas_price);
        assert_eq!(refund, expected);
    }
}
