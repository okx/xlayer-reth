//! Core execution handler for EIP-8130 AA transactions.
//!
//! Implements the AA transaction execution pipeline:
//!
//! 1. Check lock state (reject locked accounts' config changes)
//! 2. Deduct gas from payer (balance -= max_cost)
//! 3. Increment nonce (NonceManager storage write)
//! 4. Auto-delegate bare EOA to `DEFAULT_ACCOUNT_ADDRESS`
//! 5. Process `account_changes` (CREATE2, config changes, delegation)
//! 6. Populate TxContext precompile data (thread-local)
//! 7. Execute phased `calls` (each phase is atomic)
//! 8. Refund unused gas to payer
//! 9. Generate execution result for receipt
//!
//! This module provides the orchestration logic that the node builder's
//! block execution pipeline calls for each AA transaction. It delegates
//! to the consensus crate for storage slot computation, gas calculation,
//! and validation.

use alloy_primitives::{Address, Bytes, U256};
use xlayer_eip8130_consensus::{auto_delegation_code, check_lock_state, read_nonce, PhaseResult};

use crate::{
    eip8130_parts::{Eip8130ExecutionResult, Eip8130GasAccounting, Eip8130Parts},
    tx_context::{clear_eip8130_tx_context, set_eip8130_tx_context},
};

/// Errors from the AA execution pipeline.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Eip8130ExecutionError {
    /// Payer has insufficient balance to cover the max gas cost.
    #[error("payer {payer} insufficient balance: need {required}, have {available}")]
    InsufficientPayerBalance {
        /// The payer address.
        payer: Address,
        /// Required balance (max_cost).
        required: U256,
        /// Available balance.
        available: U256,
    },
    /// Nonce validation failed.
    #[error("nonce mismatch: {0}")]
    NonceMismatch(String),
    /// Account is locked and cannot process config changes.
    #[error("account {0} is locked")]
    AccountLocked(Address),
    /// Database error during execution.
    #[error("database error: {0}")]
    Database(String),
    /// EVM execution error.
    #[error("evm error: {0}")]
    EvmError(String),
    /// Gas limit exceeded.
    #[error("out of gas: consumed {consumed}, limit {limit}")]
    OutOfGas {
        /// Gas consumed.
        consumed: u64,
        /// Gas limit.
        limit: u64,
    },
}

/// RAII guard that clears the thread-local TxContext on drop.
///
/// Ensures the TxContext is always cleaned up, even on early returns or panics.
struct TxContextGuard;

impl Drop for TxContextGuard {
    fn drop(&mut self) {
        clear_eip8130_tx_context();
    }
}

/// The AA execution pipeline orchestrator.
///
/// Stateless — all mutable state is passed through the `State` / `Evm`
/// parameters. The handler is constructed once and reused across
/// transactions within a block.
#[derive(Debug, Clone, Default)]
pub struct Eip8130Handler;

impl Eip8130Handler {
    /// Creates a new handler instance.
    pub fn new() -> Self {
        Self
    }

    /// Executes the full AA transaction pipeline.
    ///
    /// This is the main entry point called by the block execution engine.
    /// It coordinates all 9 steps of the AA execution flow.
    ///
    /// # Arguments
    /// - `db` — Mutable database for state reads/writes.
    /// - `parts` — Pre-computed execution data from [`Eip8130Parts::new`].
    /// - `block_timestamp` — Current block timestamp (for lock check).
    /// - `effective_gas_price` — Effective gas price for this transaction.
    /// - `execute_call` — Closure that executes a single call via the EVM.
    ///   Takes `(caller, to, data, gas_limit)` → `Result<(success, gas_used, output), error>`.
    ///
    /// # Returns
    /// The execution result for receipt generation.
    pub fn execute<DB, F>(
        &self,
        db: &mut DB,
        parts: &mut Eip8130Parts,
        block_timestamp: u64,
        effective_gas_price: u128,
        execute_call: F,
    ) -> Result<Eip8130ExecutionResult, Eip8130ExecutionError>
    where
        DB: revm::Database,
        DB::Error: core::fmt::Display,
        F: Fn(&mut DB, Address, Address, Bytes, u64) -> Result<(bool, u64, Bytes), String>,
    {
        let mut gas = Eip8130GasAccounting::new(parts);

        // Step 1: Check lock state for config changes.
        self.check_lock(db, parts, block_timestamp)?;

        // Step 2: Deduct gas from payer.
        self.deduct_gas(db, parts)?;

        // Step 3: Increment nonce.
        self.increment_nonce(db, parts)?;

        // Step 4: Auto-delegate bare EOA.
        self.auto_delegate(db, parts)?;

        // Step 5: Process account changes.
        self.process_account_changes(db, parts)?;

        // Step 6: Populate TxContext precompile.
        // Use a drop guard to ensure cleanup on any exit path (error or panic).
        set_eip8130_tx_context(parts.to_eip8130_tx_context());
        let _tx_ctx_guard = TxContextGuard;

        // Step 7: Execute phased calls.
        let phase_results = self.execute_phases(db, parts, &mut gas, &execute_call)?;

        // TxContext is cleared when `_tx_ctx_guard` drops (at function exit).

        // Step 8: Refund unused gas to payer.
        let refund = self.refund_gas(db, parts, gas.consumed(), effective_gas_price)?;

        // Step 9: Build execution result.
        let all_succeeded = phase_results.iter().all(|p| p.success);
        Ok(Eip8130ExecutionResult {
            success: all_succeeded,
            gas_used: gas.consumed(),
            phase_results,
            effective_gas_price,
            gas_refund: refund,
        })
    }

    // ── Step 1: Lock check ────────────────────────────────────────────

    fn check_lock<DB>(
        &self,
        db: &mut DB,
        parts: &Eip8130Parts,
        block_timestamp: u64,
    ) -> Result<(), Eip8130ExecutionError>
    where
        DB: revm::Database,
        DB::Error: core::fmt::Display,
    {
        // Only ConfigChange entries require lock check, not Create entries.
        // sequence_updates are only populated for ConfigChange entries.
        let has_config_changes = !parts.sequence_updates.is_empty();

        if !has_config_changes {
            return Ok(());
        }

        check_lock_state(db, parts.sender, block_timestamp)
            .map_err(|_| Eip8130ExecutionError::AccountLocked(parts.sender))
    }

    // ── Step 2: Gas deduction ─────────────────────────────────────────

    fn deduct_gas<DB>(&self, db: &mut DB, parts: &Eip8130Parts) -> Result<(), Eip8130ExecutionError>
    where
        DB: revm::Database,
        DB::Error: core::fmt::Display,
    {
        let payer_info = db
            .basic(parts.payer)
            .map_err(|e| Eip8130ExecutionError::Database(e.to_string()))?
            .unwrap_or_default();

        if payer_info.balance < parts.max_payer_cost {
            return Err(Eip8130ExecutionError::InsufficientPayerBalance {
                payer: parts.payer,
                required: parts.max_payer_cost,
                available: payer_info.balance,
            });
        }

        // Deduction is handled by the EVM execution context — the block
        // builder decrements the payer's balance before transaction execution.
        // We only validate here; actual balance modification happens in the
        // state transition.
        Ok(())
    }

    // ── Step 3: Nonce increment ───────────────────────────────────────

    fn increment_nonce<DB>(
        &self,
        db: &mut DB,
        parts: &Eip8130Parts,
    ) -> Result<(), Eip8130ExecutionError>
    where
        DB: revm::Database,
        DB::Error: core::fmt::Display,
    {
        // Read current nonce from NonceManager storage.
        let current = read_nonce(db, parts.sender, parts.nonce_key)
            .map_err(|e| Eip8130ExecutionError::Database(e.to_string()))?;

        if current != parts.nonce_sequence {
            return Err(Eip8130ExecutionError::NonceMismatch(format!(
                "expected {}, got {}",
                parts.nonce_sequence, current
            )));
        }

        // The actual nonce write is applied by the caller via
        // `collect_storage_writes()`, since revm's `Database` trait is
        // read-only. The handler validates here; the block builder
        // commits `parts.nonce_write` to state.
        Ok(())
    }

    // ── Step 4: Auto-delegate ─────────────────────────────────────────

    fn auto_delegate<DB>(
        &self,
        db: &mut DB,
        parts: &mut Eip8130Parts,
    ) -> Result<(), Eip8130ExecutionError>
    where
        DB: revm::Database,
        DB::Error: core::fmt::Display,
    {
        let account = db
            .basic(parts.sender)
            .map_err(|e| Eip8130ExecutionError::Database(e.to_string()))?
            .unwrap_or_default();

        // A bare EOA has no code. Auto-delegate to DEFAULT_ACCOUNT_ADDRESS
        // so the account has a code-based identity for the AA system.
        if account.code_hash == revm::primitives::KECCAK_EMPTY
            || account.code_hash == revm::primitives::B256::ZERO
        {
            parts.needs_auto_delegate = true;
            // The actual EIP-7702 delegation is applied by the block builder.
            // We just flag it here so the caller knows to set the code.
        }

        Ok(())
    }

    // ── Step 5: Account changes ───────────────────────────────────────

    fn process_account_changes<DB>(
        &self,
        db: &mut DB,
        parts: &Eip8130Parts,
    ) -> Result<(), Eip8130ExecutionError>
    where
        DB: revm::Database,
        DB::Error: core::fmt::Display,
    {
        // Code placements and config writes are pre-computed in Eip8130Parts.
        // The handler only needs to validate sequence numbers here.
        for &(slot, is_multichain, new_value) in &parts.sequence_updates {
            // Read the current account state to get the current sequence.
            let current_packed = db
                .storage(xlayer_eip8130_consensus::ACCOUNT_CONFIG_ADDRESS, slot)
                .map_err(|e| Eip8130ExecutionError::Database(e.to_string()))?;

            let current_seq =
                xlayer_eip8130_consensus::read_sequence(current_packed, is_multichain);

            if new_value != current_seq + 1 {
                return Err(Eip8130ExecutionError::NonceMismatch(format!(
                    "config change sequence: expected {}, got {}",
                    current_seq + 1,
                    new_value,
                )));
            }
        }

        Ok(())
    }

    // ── Step 7: Phased execution ──────────────────────────────────────

    fn execute_phases<DB, F>(
        &self,
        db: &mut DB,
        parts: &Eip8130Parts,
        gas: &mut Eip8130GasAccounting,
        execute_call: &F,
    ) -> Result<Vec<PhaseResult>, Eip8130ExecutionError>
    where
        DB: revm::Database,
        DB::Error: core::fmt::Display,
        F: Fn(&mut DB, Address, Address, Bytes, u64) -> Result<(bool, u64, Bytes), String>,
    {
        let mut results = Vec::with_capacity(parts.execution_calls.len());

        for phase_calls in &parts.execution_calls {
            let phase_gas_start = gas.consumed();
            let mut phase_success = true;

            for call in phase_calls {
                let remaining = gas.remaining();
                if remaining == 0 {
                    phase_success = false;
                    break;
                }

                match execute_call(db, call.caller, call.to, call.data.clone(), remaining) {
                    Ok((success, gas_used, _output)) => {
                        gas.record_phase(gas_used);
                        if !success {
                            phase_success = false;
                            break;
                        }
                    }
                    Err(_) => {
                        // EVM-level error (not a revert) — phase fails.
                        phase_success = false;
                        break;
                    }
                }
            }

            let phase_gas_used = gas.consumed() - phase_gas_start;
            results.push(PhaseResult { success: phase_success, gas_used: phase_gas_used });

            // If a phase fails, subsequent phases still execute independently
            // (each phase is atomic, but phases are independent of each other).
        }

        Ok(results)
    }

    // ── Step 8: Gas refund ────────────────────────────────────────────

    fn refund_gas<DB>(
        &self,
        _db: &mut DB,
        parts: &Eip8130Parts,
        gas_used: u64,
        effective_gas_price: u128,
    ) -> Result<U256, Eip8130ExecutionError>
    where
        DB: revm::Database,
        DB::Error: core::fmt::Display,
    {
        let refund = parts.compute_refund(gas_used, effective_gas_price);
        // Actual balance credit is applied by the block builder's state
        // commit layer. We return the refund amount for the caller.
        Ok(refund)
    }
}

/// Returns the auto-delegation code bytes for bare EOAs.
///
/// The delegation code is `0xef0100 || DEFAULT_ACCOUNT_ADDRESS`,
/// which is the EIP-7702 delegation designator.
pub fn get_auto_delegation_code() -> Bytes {
    auto_delegation_code()
}

/// Returns the list of storage writes that the handler wants applied.
///
/// This is called by the block builder after `execute()` returns, to apply
/// the AA-specific storage writes (nonce increment, config changes, etc.)
/// to the state.
///
/// **Note:** Code placements (CREATE2 deployments) are NOT included here.
/// Use [`collect_code_placements`] separately to apply those.
pub fn collect_storage_writes(parts: &Eip8130Parts) -> Vec<xlayer_eip8130_consensus::StorageWrite> {
    let mut writes = Vec::with_capacity(1 + parts.config_writes.len());

    // Nonce increment.
    writes.push(parts.nonce_write.clone());

    // Config change writes.
    writes.extend(parts.config_writes.iter().cloned());

    writes
}

/// Returns the list of code placements (CREATE2 deployments) that the
/// handler wants applied.
///
/// The block builder must set the code at each returned address after
/// `execute()` returns, in addition to applying [`collect_storage_writes`].
pub fn collect_code_placements(parts: &Eip8130Parts) -> &[xlayer_eip8130_consensus::CodePlacement] {
    &parts.code_placements
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use alloy_primitives::{Address, B256, U256};
    use xlayer_eip8130_consensus::{TxEip8130, DEFAULT_ACCOUNT_ADDRESS, NONCE_MANAGER_ADDRESS};

    use super::*;
    use crate::eip8130_parts::Eip8130Parts;

    /// Minimal in-memory database for testing.
    #[derive(Default)]
    struct MockDb {
        accounts: std::collections::HashMap<Address, revm::state::AccountInfo>,
        storage: std::collections::HashMap<(Address, U256), U256>,
    }

    impl revm::Database for MockDb {
        type Error = Infallible;

        fn basic(
            &mut self,
            address: Address,
        ) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
            Ok(self.accounts.get(&address).cloned())
        }

        fn code_by_hash(
            &mut self,
            _code_hash: B256,
        ) -> Result<revm::bytecode::Bytecode, Self::Error> {
            Ok(revm::bytecode::Bytecode::default())
        }

        fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
            Ok(self.storage.get(&(address, index)).copied().unwrap_or(U256::ZERO))
        }

        fn block_hash(&mut self, _number: u64) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    fn make_simple_parts() -> Eip8130Parts {
        let tx = TxEip8130 {
            chain_id: 1,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            max_priority_fee_per_gas: 1,
            nonce_key: U256::ZERO,
            nonce_sequence: 0,
            ..Default::default()
        };
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);

        Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false)
    }

    /// Successful no-op call executor.
    fn noop_executor(
        _db: &mut MockDb,
        _caller: Address,
        _to: Address,
        _data: Bytes,
        _gas: u64,
    ) -> Result<(bool, u64, Bytes), String> {
        Ok((true, 1000, Bytes::new()))
    }

    // ── Step 3a tests ─────────────────────────────────────────────────

    #[test]
    fn simple_self_pay_executes() {
        let handler = Eip8130Handler::new();
        let mut parts = make_simple_parts();
        let mut db = MockDb::default();

        // Fund the payer.
        db.accounts.insert(
            parts.payer,
            revm::state::AccountInfo { balance: U256::from(10_000_000u64), ..Default::default() },
        );

        let result = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap();

        assert!(result.success);
        assert!(result.gas_used > 0);
    }

    #[test]
    fn insufficient_payer_balance_rejected() {
        let handler = Eip8130Handler::new();
        let mut parts = make_simple_parts();
        let mut db = MockDb::default();

        // Payer has zero balance.
        let err = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap_err();

        assert!(matches!(err, Eip8130ExecutionError::InsufficientPayerBalance { .. }));
    }

    #[test]
    fn nonce_mismatch_rejected() {
        let handler = Eip8130Handler::new();
        let tx = TxEip8130 {
            chain_id: 1,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            nonce_key: U256::ZERO,
            nonce_sequence: 5, // Expect sequence 5, but storage has 0.
            ..Default::default()
        };
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);
        let mut parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);
        let mut db = MockDb::default();

        db.accounts.insert(
            sender,
            revm::state::AccountInfo { balance: U256::from(10_000_000u64), ..Default::default() },
        );

        let err = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap_err();

        assert!(matches!(err, Eip8130ExecutionError::NonceMismatch(_)));
    }

    #[test]
    fn gas_used_includes_intrinsic() {
        let handler = Eip8130Handler::new();
        let mut parts = make_simple_parts();
        let mut db = MockDb::default();

        db.accounts.insert(
            parts.payer,
            revm::state::AccountInfo { balance: U256::from(10_000_000u64), ..Default::default() },
        );

        let result = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap();

        // Gas used must be at least the intrinsic gas.
        assert!(result.gas_used >= parts.intrinsic_gas);
    }

    #[test]
    fn auto_delegate_flags_bare_eoa() {
        let handler = Eip8130Handler::new();
        let mut parts = make_simple_parts();
        let mut db = MockDb::default();

        db.accounts.insert(
            parts.sender,
            revm::state::AccountInfo {
                balance: U256::from(10_000_000u64),
                // code_hash defaults to KECCAK_EMPTY → bare EOA.
                ..Default::default()
            },
        );

        let _result = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap();

        assert!(parts.needs_auto_delegate);
    }

    #[test]
    fn gas_refund_calculated() {
        let handler = Eip8130Handler::new();
        let mut parts = make_simple_parts();
        let mut db = MockDb::default();

        db.accounts.insert(
            parts.payer,
            revm::state::AccountInfo { balance: U256::from(10_000_000u64), ..Default::default() },
        );

        let result = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap();

        // Refund should be > 0 since we didn't use all gas.
        assert!(result.gas_refund > U256::ZERO);
    }

    // ── Step 3b tests: Payer sponsorship ──────────────────────────────

    #[test]
    fn external_payer_gas_deduction() {
        let handler = Eip8130Handler::new();
        let tx = TxEip8130 {
            chain_id: 1,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            payer: Some(Address::repeat_byte(0xBB)),
            payer_auth: Bytes::from(vec![0u8; 65]),
            ..Default::default()
        };
        let sender = Address::repeat_byte(0xAA);
        let payer = Address::repeat_byte(0xBB);
        let sender_oid = B256::repeat_byte(0x11);
        let payer_oid = B256::repeat_byte(0x22);
        let mut parts = Eip8130Parts::new(&tx, sender, payer, sender_oid, payer_oid, false);
        let mut db = MockDb::default();

        // Only fund the payer, not the sender.
        db.accounts.insert(
            payer,
            revm::state::AccountInfo { balance: U256::from(10_000_000u64), ..Default::default() },
        );

        let result = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap();

        assert!(result.success);
        assert!(!parts.is_self_pay());
    }

    #[test]
    fn external_payer_insufficient_balance() {
        let handler = Eip8130Handler::new();
        let tx = TxEip8130 {
            chain_id: 1,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            payer: Some(Address::repeat_byte(0xBB)),
            payer_auth: Bytes::from(vec![0u8; 65]),
            ..Default::default()
        };
        let sender = Address::repeat_byte(0xAA);
        let payer = Address::repeat_byte(0xBB);
        let sender_oid = B256::repeat_byte(0x11);
        let payer_oid = B256::repeat_byte(0x22);
        let mut parts = Eip8130Parts::new(&tx, sender, payer, sender_oid, payer_oid, false);
        let mut db = MockDb::default();

        // Payer has insufficient balance.
        db.accounts.insert(
            payer,
            revm::state::AccountInfo {
                balance: U256::from(1u64), // Way too low.
                ..Default::default()
            },
        );

        let err = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap_err();

        assert!(matches!(err, Eip8130ExecutionError::InsufficientPayerBalance { .. }));
    }

    // ── Step 3c tests: Multi-phase calls ──────────────────────────────

    #[test]
    fn multi_phase_all_succeed() {
        let handler = Eip8130Handler::new();
        let tx = TxEip8130 {
            chain_id: 1,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            calls: vec![
                vec![xlayer_eip8130_consensus::Call {
                    to: Address::repeat_byte(0x01),
                    data: Bytes::from(vec![0x01]),
                }],
                vec![xlayer_eip8130_consensus::Call {
                    to: Address::repeat_byte(0x02),
                    data: Bytes::from(vec![0x02]),
                }],
            ],
            ..Default::default()
        };
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);
        let mut parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);
        let mut db = MockDb::default();

        db.accounts.insert(
            sender,
            revm::state::AccountInfo { balance: U256::from(10_000_000u64), ..Default::default() },
        );

        let result = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap();

        assert!(result.success);
        assert_eq!(result.phase_results.len(), 2);
        assert!(result.phase_results[0].success);
        assert!(result.phase_results[1].success);
    }

    #[test]
    fn multi_phase_second_fails_first_committed() {
        let handler = Eip8130Handler::new();
        let tx = TxEip8130 {
            chain_id: 1,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            calls: vec![
                vec![xlayer_eip8130_consensus::Call {
                    to: Address::repeat_byte(0x01),
                    data: Bytes::from(vec![0x01]),
                }],
                vec![xlayer_eip8130_consensus::Call {
                    to: Address::repeat_byte(0x02),
                    data: Bytes::from(vec![0x02]),
                }],
            ],
            ..Default::default()
        };
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);
        let mut parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);
        let mut db = MockDb::default();

        db.accounts.insert(
            sender,
            revm::state::AccountInfo { balance: U256::from(10_000_000u64), ..Default::default() },
        );

        // Executor that fails the second call.
        let call_count = std::cell::RefCell::new(0usize);
        let failing_executor = |_db: &mut MockDb,
                                _caller: Address,
                                _to: Address,
                                _data: Bytes,
                                _gas: u64|
         -> Result<(bool, u64, Bytes), String> {
            let mut count = call_count.borrow_mut();
            *count += 1;
            if *count >= 2 {
                Ok((false, 500, Bytes::new())) // Revert on 2nd call
            } else {
                Ok((true, 1000, Bytes::new()))
            }
        };

        let result = handler.execute(&mut db, &mut parts, 0, 10, failing_executor).unwrap();

        // Overall not all phases succeeded.
        assert!(!result.success);
        // Phase 1 succeeded, phase 2 failed.
        assert_eq!(result.phase_results.len(), 2);
        assert!(result.phase_results[0].success);
        assert!(!result.phase_results[1].success);
    }

    #[test]
    fn phase_results_in_receipt() {
        let handler = Eip8130Handler::new();
        let tx = TxEip8130 {
            chain_id: 1,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            calls: vec![
                vec![xlayer_eip8130_consensus::Call {
                    to: Address::repeat_byte(0x01),
                    data: Bytes::from(vec![0x01]),
                }],
                vec![xlayer_eip8130_consensus::Call {
                    to: Address::repeat_byte(0x02),
                    data: Bytes::from(vec![0x02]),
                }],
                vec![xlayer_eip8130_consensus::Call {
                    to: Address::repeat_byte(0x03),
                    data: Bytes::from(vec![0x03]),
                }],
            ],
            ..Default::default()
        };
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);
        let mut parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);
        let mut db = MockDb::default();

        db.accounts.insert(
            sender,
            revm::state::AccountInfo { balance: U256::from(10_000_000u64), ..Default::default() },
        );

        let result = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap();

        assert_eq!(result.phase_results.len(), 3);
        for phase in &result.phase_results {
            assert!(phase.success);
            assert!(phase.gas_used > 0);
        }
    }

    // ── Step 3d tests: Account changes ────────────────────────────────

    #[test]
    fn auto_delegate_code_is_valid() {
        let code = get_auto_delegation_code();
        // Should be exactly 23 bytes: 3-byte prefix + 20-byte address.
        assert_eq!(code.len(), 23);
        assert_eq!(code[0], 0xef);
        assert_eq!(code[1], 0x01);
        assert_eq!(code[2], 0x00);
        assert_eq!(&code[3..], DEFAULT_ACCOUNT_ADDRESS.as_slice());
    }

    #[test]
    fn collect_storage_writes_includes_nonce() {
        let parts = make_simple_parts();
        let writes = collect_storage_writes(&parts);

        assert!(!writes.is_empty());
        // First write should be the nonce increment.
        assert_eq!(writes[0].address, NONCE_MANAGER_ADDRESS);
    }

    #[test]
    fn empty_calls_produces_empty_results() {
        let handler = Eip8130Handler::new();
        let tx = TxEip8130 {
            chain_id: 1,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            calls: vec![], // No calls.
            ..Default::default()
        };
        let sender = Address::repeat_byte(0xAA);
        let owner_id = B256::repeat_byte(0x11);
        let mut parts = Eip8130Parts::new(&tx, sender, sender, owner_id, owner_id, false);
        let mut db = MockDb::default();

        db.accounts.insert(
            sender,
            revm::state::AccountInfo { balance: U256::from(10_000_000u64), ..Default::default() },
        );

        let result = handler.execute(&mut db, &mut parts, 0, 10, noop_executor).unwrap();

        assert!(result.success);
        assert!(result.phase_results.is_empty());
    }
}
