//! XLayerAA (EIP-8130) handler.
//!
//! [`XLayerAAHandler`] wraps `op_revm::handler::OpHandler` and adds the
//! XLayerAA branch to each phase of the [`Handler`] lifecycle. For every
//! non-AA transaction (deposit + regular) the implementation delegates to
//! the upstream `OpHandler`, so this crate is a strict superset of op-revm
//! rather than a fork.
//!
//! The AA-specific logic lives in [`helpers`]. See
//! `docs/xlayer-aa.md` for the protocol reference.

pub mod helpers;

#[cfg(test)]
mod tests;

use std::boxed::Box;

use op_revm::{
    L1BlockInfo, OpHaltReason, OpSpecId,
    constants::{BASE_FEE_RECIPIENT, L1_FEE_RECIPIENT, OPERATOR_FEE_RECIPIENT},
    handler::IsTxError,
    transaction::{
        abstraction::OpTxTr, deposit::DEPOSIT_TRANSACTION_TYPE, error::OpTransactionError,
    },
};
use revm::{
    context::{
        LocalContextTr,
        journaled_state::account::JournaledAccountTr,
        result::InvalidTransaction,
    },
    context_interface::{
        Block, Cfg, ContextTr, JournalTr, Transaction,
        context::ContextError,
        result::{EVMError, ExecutionResult, FromStringError},
    },
    handler::{
        EthFrame, EvmTr, FrameResult, Handler, MainnetHandler,
        evm::FrameTr,
        handler::EvmTrError,
        post_execution,
        pre_execution::calculate_caller_fee,
    },
    inspector::{Inspector, InspectorEvmTr, InspectorHandler},
    interpreter::{
        CallOutcome, Gas, InstructionResult, InterpreterResult, SharedMemory,
        interpreter::EthInterpreter,
        interpreter_action::{CallInput, CallInputs, CallScheme, CallValue, FrameInit, FrameInput},
    },
    primitives::{Address, B256, U256, hardfork::SpecId, keccak256},
    state::EvmState,
};

use crate::{
    constants::{
        DELEGATE_VERIFIER_ADDRESS, MAX_ACCOUNT_CHANGES_PER_TX, MAX_CALLS_PER_TX, OWNER_SCOPE_PAYER,
        OWNER_SCOPE_SENDER, XLAYERAA_TX_TYPE,
    },
    precompiles::{NONCE_MANAGER_ADDRESS, TX_CONTEXT_ADDRESS},
    transaction::{
        XLayerAATxTr, config_log_to_system_log, encode_phase_statuses, phase_statuses_system_log,
    },
};

use helpers::{
    ACCOUNT_CONFIG_ADDRESS, EXPIRING_NONCE_SET_CAPACITY, EXPIRING_RING_PTR_SLOT,
    NONCE_COLD_WARM_DELTA, NONCE_FREE_MAX_EXPIRY_WINDOW, NONCE_KEY_MAX, aa_expiring_ring_slot,
    aa_expiring_seen_slot, aa_nonce_slot, check_account_lock, estimation_calldata_overhead,
    run_custom_verifier_staticcall, validate_authorizer_chain, validate_config_change_preconditions,
    validate_native_verifier_owner, validate_owner_config,
};

// ---------------------------------------------------------------------------
// Context bound
// ---------------------------------------------------------------------------

/// Context bound used throughout XLayerAA-aware handler code.
pub trait XLayerAAContextTr:
    ContextTr<
        Journal: JournalTr<State = EvmState>,
        Tx: XLayerAATxTr,
        Cfg: Cfg<Spec = OpSpecId>,
        Chain = L1BlockInfo,
    >
{
}

impl<T> XLayerAAContextTr for T where
    T: ContextTr<
            Journal: JournalTr<State = EvmState>,
            Tx: XLayerAATxTr,
            Cfg: Cfg<Spec = OpSpecId>,
            Chain = L1BlockInfo,
        >
{
}

// ---------------------------------------------------------------------------
// Handler struct
// ---------------------------------------------------------------------------

/// XLayerAA handler. Wraps an upstream [`op_revm::handler::OpHandler`] and
/// layers the AA lifecycle branches on top.
#[derive(Debug, Clone)]
pub struct XLayerAAHandler<EVM, ERROR, FRAME> {
    /// Upstream op-revm handler used for every non-AA transaction.
    pub op: op_revm::handler::OpHandler<EVM, ERROR, FRAME>,
}

impl<EVM, ERROR, FRAME> XLayerAAHandler<EVM, ERROR, FRAME> {
    /// Create a new handler.
    pub fn new() -> Self {
        Self { op: op_revm::handler::OpHandler::new() }
    }

    /// Shortcut to the inner mainnet handler used for `run_exec_loop` during
    /// AA verifier STATICCALLs and call-phase execution.
    pub fn mainnet_mut(&mut self) -> &mut MainnetHandler<EVM, ERROR, FRAME> {
        &mut self.op.mainnet
    }
}

impl<EVM, ERROR, FRAME> Default for XLayerAAHandler<EVM, ERROR, FRAME> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Handler trait impl
// ---------------------------------------------------------------------------

impl<EVM, ERROR, FRAME> Handler for XLayerAAHandler<EVM, ERROR, FRAME>
where
    EVM: EvmTr<Context: XLayerAAContextTr, Frame = FRAME>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError> + FromStringError + IsTxError,
    FRAME: FrameTr<FrameResult = FrameResult, FrameInit = FrameInit>,
{
    type Evm = EVM;
    type Error = ERROR;
    type HaltReason = OpHaltReason;

    fn validate_env(&self, evm: &mut Self::Evm) -> Result<(), Self::Error> {
        let tx_type = evm.ctx().tx().tx_type();
        if tx_type != XLAYERAA_TX_TYPE {
            return self.op.validate_env(evm);
        }

        // AA transactions require an enveloped tx (same as other non-deposit).
        if evm.ctx().tx().enveloped_tx().is_none() {
            return Err(OpTransactionError::MissingEnvelopedTx.into());
        }

        let ctx = evm.ctx();

        if !ctx.cfg().is_base_fee_check_disabled() {
            let basefee = ctx.block().basefee() as u128;
            let max_fee = ctx.tx().max_fee_per_gas();
            let max_priority = ctx.tx().max_priority_fee_per_gas().unwrap_or(0);

            if max_fee < basefee {
                return Err(OpTransactionError::Base(InvalidTransaction::Str(
                    "XLayerAA: max_fee_per_gas below base fee".into(),
                ))
                .into());
            }
            if max_priority > max_fee {
                return Err(OpTransactionError::Base(InvalidTransaction::Str(
                    "XLayerAA: max_priority_fee_per_gas exceeds max_fee_per_gas".into(),
                ))
                .into());
            }
        }

        let parts = ctx.tx().xlayeraa_parts();

        // EIP-8130: when `nonce_key == NONCE_KEY_MAX` (nonce-free mode), the
        // protocol does not read or increment nonce state; replay protection
        // relies on `expiry`. Spec MUSTs: `expiry != 0`, `nonce_sequence == 0`.
        if parts.nonce_key == NONCE_KEY_MAX {
            if parts.expiry == 0 {
                return Err(OpTransactionError::Base(InvalidTransaction::Str(
                    "XLayerAA: nonce-free tx requires non-zero expiry".into(),
                ))
                .into());
            }
            if ctx.tx().nonce() != 0 {
                return Err(OpTransactionError::Base(InvalidTransaction::Str(
                    "XLayerAA: nonce-free tx requires nonce_sequence == 0".into(),
                ))
                .into());
            }
            // Our replay-protection implementation slots the tx's
            // `nonce_free_hash` into a ring buffer. A missing hash would
            // collide on zero across every such tx, so require it present.
            if parts.nonce_free_hash.is_none() {
                return Err(OpTransactionError::Base(InvalidTransaction::Str(
                    "XLayerAA: nonce-free tx requires nonce_free_hash".into(),
                ))
                .into());
            }
        }

        // Expiry check.
        if parts.expiry != 0 {
            let block_ts = ctx.block().timestamp().saturating_to::<u64>();
            if block_ts > parts.expiry {
                return Err(OpTransactionError::Base(InvalidTransaction::Str(
                    format!(
                        "XLayerAA: transaction expired (expiry={}, current={block_ts})",
                        parts.expiry
                    )
                    .into(),
                ))
                .into());
            }
        }

        // Structural guards.
        let total_calls: usize = parts.call_phases.iter().map(Vec::len).sum();
        if total_calls > MAX_CALLS_PER_TX {
            return Err(OpTransactionError::Base(InvalidTransaction::Str(
                format!("XLayerAA: too many calls ({total_calls} > {MAX_CALLS_PER_TX})").into(),
            ))
            .into());
        }
        if parts.account_change_units > MAX_ACCOUNT_CHANGES_PER_TX {
            return Err(OpTransactionError::Base(InvalidTransaction::Str(
                format!(
                    "XLayerAA: too many account changes ({} > {MAX_ACCOUNT_CHANGES_PER_TX})",
                    parts.account_change_units
                )
                .into(),
            ))
            .into());
        }

        // EIP-155-style chain_id check. Upstream mainnet validate_env does
        // this for OP txs; the AA branch bypasses mainnet so we replicate it.
        if ctx.cfg().tx_chain_id_check() {
            match ctx.tx().chain_id() {
                Some(tx_chain_id) if tx_chain_id != ctx.cfg().chain_id() => {
                    return Err(InvalidTransaction::InvalidChainId.into());
                }
                None => return Err(InvalidTransaction::MissingChainId.into()),
                _ => {}
            }
        }

        Ok(())
    }

    fn validate_initial_tx_gas(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<revm::interpreter::InitialAndFloorGas, Self::Error> {
        if evm.ctx().tx().tx_type() != XLAYERAA_TX_TYPE {
            return self.op.validate_initial_tx_gas(evm);
        }

        let ctx = evm.ctx();
        let parts = ctx.tx().xlayeraa_parts();
        let aa_gas = parts.aa_intrinsic_gas;
        let calldata_overhead = estimation_calldata_overhead(parts);
        let is_estimation = ctx.cfg().is_base_fee_check_disabled();
        let gas_limit = ctx.tx().gas_limit();

        let effective_gas = if is_estimation { aa_gas + calldata_overhead } else { aa_gas };

        if effective_gas > gas_limit {
            return Err(InvalidTransaction::CallGasCostMoreThanGasLimit {
                gas_limit,
                initial_gas: effective_gas,
            }
            .into());
        }
        Ok(revm::interpreter::InitialAndFloorGas::new(effective_gas, 0))
    }

    fn validate_against_state_and_deduct_caller(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<(), Self::Error> {
        if evm.ctx().tx().tx_type() != XLAYERAA_TX_TYPE {
            return self.op.validate_against_state_and_deduct_caller(evm);
        }

        let sender = evm.ctx().tx().caller();
        let parts = evm.ctx().tx().xlayeraa_parts().clone();

        // EIP-8130 step 1: if the tx carries config-change or delegation
        // entries, the sender's account MUST NOT be locked. Must run before
        // gas deduction so a locked account never pays for a rejected tx.
        let needs_lock_check = parts.delegation_target.is_some()
            || !parts.sequence_updates.is_empty()
            || !parts.config_writes.is_empty();
        if needs_lock_check {
            check_account_lock::<EVM, ERROR>(evm, sender)?;
        }

        let (block, tx, cfg, journal, chain, _) = evm.ctx().all_mut();
        let spec = cfg.spec();

        // Refresh L1 block info (same as upstream OpHandler).
        if chain.l2_block != Some(block.number()) {
            *chain = L1BlockInfo::try_fetch(journal.db_mut(), block.number(), spec)?;
        }

        let nonce_sequence = tx.nonce();

        // --- Gas deduction from payer ---
        let payer = parts.payer;
        let mut payer_account = journal.load_account_with_code_mut(payer)?.data;
        let mut balance = payer_account.account().info.balance;

        if !cfg.is_fee_charge_disabled() {
            let Some(additional_cost) = chain.tx_cost_with_tx(tx, spec) else {
                return Err(ERROR::from_string("[XLayerAA] Failed to load enveloped tx.".into()));
            };
            let Some(new_balance) = balance.checked_sub(additional_cost) else {
                return Err(InvalidTransaction::LackOfFundForMaxFee {
                    fee: Box::new(additional_cost),
                    balance: Box::new(balance),
                }
                .into());
            };
            balance = new_balance;
        }

        let balance = calculate_caller_fee(balance, tx, block, cfg)?;
        payer_account.set_balance(balance);
        drop(payer_account);

        // Snapshot sender code presence for auto-delegation decision.
        let sender_account = journal.load_account_with_code_mut(sender)?.data;
        let sender_has_code = sender_account.account().info.code_hash != keccak256([]);
        drop(sender_account);

        // --- Nonce handling ---
        let nonce_key = parts.nonce_key;
        if nonce_key != NONCE_KEY_MAX {
            // 2D sequenced nonce via NonceManager.
            let slot = aa_nonce_slot(sender, nonce_key);
            journal.load_account(NONCE_MANAGER_ADDRESS)?;
            let current_seq = journal.sload(NONCE_MANAGER_ADDRESS, slot)?.data;

            let skip_nonce_check =
                cfg.is_nonce_check_disabled() || cfg.is_base_fee_check_disabled();

            if !skip_nonce_check {
                let expected = U256::from(nonce_sequence);
                if current_seq != expected {
                    if current_seq > expected {
                        return Err(InvalidTransaction::NonceTooLow {
                            tx: nonce_sequence,
                            state: current_seq.as_limbs()[0],
                        }
                        .into());
                    }
                    return Err(InvalidTransaction::NonceTooHigh {
                        tx: nonce_sequence,
                        state: current_seq.as_limbs()[0],
                    }
                    .into());
                }
            }
            let next_seq = if skip_nonce_check {
                current_seq + U256::from(1)
            } else {
                U256::from(nonce_sequence + 1)
            };
            journal.sstore(NONCE_MANAGER_ADDRESS, slot, next_seq)?;
        } else {
            // Nonce-free mode: expiring ring buffer replay protection.
            let now: u64 = block.timestamp().to();
            let expiry = parts.expiry;
            let skip_checks =
                cfg.is_nonce_check_disabled() || cfg.is_base_fee_check_disabled();

            if !skip_checks
                && (expiry <= now || expiry > now + NONCE_FREE_MAX_EXPIRY_WINDOW)
            {
                return Err(ERROR::from_string(format!(
                    "nonce-free expiry out of window: expiry={expiry}, now={now}"
                )));
            }

            // `validate_env` already rejects `None` for nonce-free txs;
            // re-check here rather than unwrap, so a skipped validate_env
            // path still cannot collide on zero-hash.
            let nf_hash = parts.nonce_free_hash.ok_or_else(|| {
                ERROR::from_string("XLayerAA: nonce-free tx requires nonce_free_hash".into())
            })?;
            journal.load_account(NONCE_MANAGER_ADDRESS)?;

            let seen_slot = aa_expiring_seen_slot(nf_hash);
            let seen_expiry = journal.sload(NONCE_MANAGER_ADDRESS, seen_slot)?.data;
            if !skip_checks && seen_expiry != U256::ZERO && seen_expiry > U256::from(now) {
                return Err(ERROR::from_string(
                    "nonce-free transaction replay: hash already seen".into(),
                ));
            }

            let ptr_raw = journal.sload(NONCE_MANAGER_ADDRESS, EXPIRING_RING_PTR_SLOT)?.data;
            let idx = ptr_raw.as_limbs()[0] as u32 % EXPIRING_NONCE_SET_CAPACITY;

            let ring_slot = aa_expiring_ring_slot(idx);
            let old_hash_raw = journal.sload(NONCE_MANAGER_ADDRESS, ring_slot)?.data;

            if old_hash_raw != U256::ZERO {
                let old_hash = B256::from(old_hash_raw.to_be_bytes::<32>());
                let old_seen_slot = aa_expiring_seen_slot(old_hash);
                let old_expiry = journal.sload(NONCE_MANAGER_ADDRESS, old_seen_slot)?.data;
                if !skip_checks && old_expiry != U256::ZERO && old_expiry > U256::from(now) {
                    return Err(ERROR::from_string(
                        "nonce-free buffer full: cannot evict unexpired entry".into(),
                    ));
                }
                journal.sstore(NONCE_MANAGER_ADDRESS, old_seen_slot, U256::ZERO)?;
            }

            journal.sstore(NONCE_MANAGER_ADDRESS, ring_slot, U256::from_be_bytes(nf_hash.0))?;
            journal.sstore(NONCE_MANAGER_ADDRESS, seen_slot, U256::from(expiry))?;

            let next_ptr = if idx + 1 >= EXPIRING_NONCE_SET_CAPACITY {
                U256::ZERO
            } else {
                U256::from(idx + 1)
            };
            journal.sstore(NONCE_MANAGER_ADDRESS, EXPIRING_RING_PTR_SLOT, next_ptr)?;
        }

        // --- Delegation ---
        if let Some(target) = parts.delegation_target {
            let acc = journal.load_account_with_code_mut(sender)?.data;
            let current_code = acc.account().info.code.as_ref();
            let is_empty = current_code.is_none_or(|c| c.is_empty());
            let is_delegation = current_code.is_some_and(|c| c.is_eip7702());
            drop(acc);

            if !is_empty && !is_delegation {
                return Err(ERROR::from_string(
                    "delegation entry rejected: sender has non-delegation bytecode".into(),
                ));
            }

            let code = if target.is_zero() {
                revm::bytecode::Bytecode::default()
            } else {
                revm::bytecode::Bytecode::new_eip7702(target)
            };
            let mut acc = journal.load_account_with_code_mut(sender)?.data;
            acc.set_code_and_hash_slow(code);
            drop(acc);
        } else if !sender_has_code
            && !parts.has_create_entry
            && parts.auto_delegation_code.len() == 23
        {
            let target = Address::from_slice(&parts.auto_delegation_code[3..]);
            let code = revm::bytecode::Bytecode::new_eip7702(target);
            let mut acc = journal.load_account_with_code_mut(sender)?.data;
            acc.set_code_and_hash_slow(code);
            drop(acc);
        }

        // --- Pre-execution storage writes (account creation owner registrations) ---
        for w in &parts.pre_writes {
            journal.load_account(w.address)?;
            journal.sstore(w.address, w.slot, w.value)?;
        }

        // --- Code placements (CREATE2 account runtime bytecode) ---
        for placement in &parts.code_placements {
            let code = revm::bytecode::Bytecode::new_raw(placement.code.clone());
            let mut acc = journal.load_account_with_code_mut(placement.address)?.data;
            acc.set_code_and_hash_slow(code);
            drop(acc);
        }

        // --- System logs for account creation ---
        for event in &parts.account_creation_logs {
            journal.log(config_log_to_system_log(ACCOUNT_CONFIG_ADDRESS, event));
        }

        Ok(())
    }

    fn execution(
        &mut self,
        evm: &mut Self::Evm,
        init_and_floor_gas: &revm::interpreter::InitialAndFloorGas,
    ) -> Result<FrameResult, Self::Error> {
        if evm.ctx().tx().tx_type() != XLAYERAA_TX_TYPE {
            return self.op.execution(evm, init_and_floor_gas);
        }

        let parts = evm.ctx().tx().xlayeraa_parts().clone();
        let sender = evm.ctx().tx().caller();

        let is_estimation = evm.ctx().cfg().is_base_fee_check_disabled();

        let nonce_warm_adjustment = if !is_estimation && parts.nonce_key != NONCE_KEY_MAX {
            let nonce_slot = aa_nonce_slot(sender, parts.nonce_key);
            let nonce_value =
                evm.ctx().journal_mut().sload(NONCE_MANAGER_ADDRESS, nonce_slot)?.data;
            if nonce_value > U256::from(1) { NONCE_COLD_WARM_DELTA } else { 0 }
        } else {
            0
        };

        let overhead = if is_estimation { estimation_calldata_overhead(&parts) } else { 0 };
        let gas_limit = evm
            .ctx()
            .tx()
            .gas_limit()
            .saturating_sub(parts.aa_intrinsic_gas + parts.custom_verifier_gas_cap + overhead)
            .saturating_add(nonce_warm_adjustment);

        let mut gas_remaining = gas_limit;
        let mut phase_results = std::vec::Vec::with_capacity(parts.call_phases.len());

        evm.ctx().journal_mut().load_account(sender)?;

        let mut verification_gas_used: u64 = 0;

        if !is_estimation {
            validate_config_change_preconditions::<EVM, ERROR>(evm, sender, &parts)?;

            let pending_sender_owner_overrides = validate_authorizer_chain::<EVM, ERROR, FRAME>(
                &mut self.op.mainnet,
                evm,
                sender,
                &parts,
                &mut verification_gas_used,
            )?;

            // Sender/payer custom verifier STATICCALLs.
            let verify_calls =
                [parts.sender_verify_call.as_ref(), parts.payer_verify_call.as_ref()];
            for verify_call in verify_calls.into_iter().flatten() {
                let owner_id = run_custom_verifier_staticcall::<EVM, ERROR, FRAME>(
                    &mut self.op.mainnet,
                    evm,
                    verify_call.verifier,
                    &verify_call.calldata,
                    sender,
                    parts.custom_verifier_gas_cap,
                    &mut verification_gas_used,
                    "custom verifier STATICCALL failed",
                    "custom verifier returned invalid owner_id (< 32 bytes)",
                )?;

                let pending_overrides =
                    (verify_call.account == sender).then_some(&pending_sender_owner_overrides);
                validate_owner_config::<EVM, ERROR>(
                    evm,
                    verify_call.account,
                    owner_id,
                    verify_call.verifier,
                    verify_call.required_scope,
                    pending_overrides,
                )?;
            }

            // DELEGATE wrapper owner-binding checks when the outer verifier
            // was a DELEGATE custom verifier.
            if parts.sender_verify_call.is_some()
                && parts.sender_verifier == DELEGATE_VERIFIER_ADDRESS
                && parts.owner_id != B256::ZERO
            {
                validate_owner_config::<EVM, ERROR>(
                    evm,
                    sender,
                    U256::from_be_bytes(parts.owner_id.0),
                    DELEGATE_VERIFIER_ADDRESS,
                    OWNER_SCOPE_SENDER,
                    Some(&pending_sender_owner_overrides),
                )?;
            }
            if parts.payer_verify_call.is_some()
                && parts.payer_verifier == DELEGATE_VERIFIER_ADDRESS
                && parts.payer_owner_id != B256::ZERO
                && parts.payer != parts.sender
            {
                let payer_pending_overrides =
                    (parts.payer == sender).then_some(&pending_sender_owner_overrides);
                validate_owner_config::<EVM, ERROR>(
                    evm,
                    parts.payer,
                    U256::from_be_bytes(parts.payer_owner_id.0),
                    DELEGATE_VERIFIER_ADDRESS,
                    OWNER_SCOPE_PAYER,
                    payer_pending_overrides,
                )?;
            }

            // Native verifier re-validation.
            if parts.sender_verify_call.is_none() && parts.sender_verifier != Address::ZERO {
                validate_native_verifier_owner::<EVM, ERROR>(
                    evm,
                    sender,
                    parts.sender_verifier,
                    parts.owner_id,
                    OWNER_SCOPE_SENDER,
                    Some(&pending_sender_owner_overrides),
                )?;
            }
            if parts.payer_verify_call.is_none()
                && parts.payer_verifier != Address::ZERO
                && parts.payer != parts.sender
            {
                let payer_pending_overrides =
                    (parts.payer == sender).then_some(&pending_sender_owner_overrides);
                validate_native_verifier_owner::<EVM, ERROR>(
                    evm,
                    parts.payer,
                    parts.payer_verifier,
                    parts.payer_owner_id,
                    OWNER_SCOPE_PAYER,
                    payer_pending_overrides,
                )?;
            }
        }

        // Apply config writes + sequence bumps after auth passes.
        for w in &parts.config_writes {
            evm.ctx().journal_mut().load_account(w.address)?;
            evm.ctx().journal_mut().sstore(w.address, w.slot, w.value)?;
        }
        if !parts.sequence_updates.is_empty() {
            evm.ctx().journal_mut().load_account(ACCOUNT_CONFIG_ADDRESS)?;
            for upd in &parts.sequence_updates {
                let current =
                    evm.ctx().journal_mut().sload(ACCOUNT_CONFIG_ADDRESS, upd.slot)?.data;
                let new_packed = upd.apply(current);
                evm.ctx().journal_mut().sstore(ACCOUNT_CONFIG_ADDRESS, upd.slot, new_packed)?;
            }
        }
        for event in &parts.config_change_logs {
            evm.ctx().journal_mut().log(config_log_to_system_log(ACCOUNT_CONFIG_ADDRESS, event));
        }

        let unused_verification_gas =
            parts.custom_verifier_gas_cap.saturating_sub(verification_gas_used);

        let mut accumulated_refunds: i64 = 0;

        for phase in &parts.call_phases {
            let checkpoint = evm.ctx().journal_mut().checkpoint();
            let mut phase_ok = true;
            let phase_gas_start = gas_remaining;
            let mut phase_refunds: i64 = 0;

            for call in phase {
                if gas_remaining == 0 {
                    phase_ok = false;
                    break;
                }

                evm.ctx().journal_mut().load_account(call.to)?;

                let call_gas = gas_remaining;
                let call_inputs = CallInputs {
                    input: CallInput::Bytes(call.data.clone()),
                    return_memory_offset: 0..0,
                    gas_limit: call_gas,
                    bytecode_address: call.to,
                    known_bytecode: None,
                    target_address: call.to,
                    caller: sender,
                    value: CallValue::Transfer(call.value),
                    scheme: CallScheme::Call,
                    is_static: false,
                };

                let frame_init = FrameInit {
                    depth: 0,
                    memory: {
                        let ctx = evm.ctx();
                        let mut mem = SharedMemory::new_with_buffer(
                            ctx.local().shared_memory_buffer().clone(),
                        );
                        mem.set_memory_limit(ctx.cfg().memory_limit());
                        mem
                    },
                    frame_input: FrameInput::Call(Box::new(call_inputs)),
                };

                let call_result = self.op.mainnet.run_exec_loop(evm, frame_init)?;
                let call_gas_used = call_gas.saturating_sub(call_result.gas().remaining());
                gas_remaining = gas_remaining.saturating_sub(call_gas_used);
                phase_refunds += call_result.gas().refunded();

                if !call_result.interpreter_result().result.is_ok() {
                    phase_ok = false;
                    break;
                }
            }

            if phase_ok {
                accumulated_refunds += phase_refunds;
            } else {
                evm.ctx().journal_mut().checkpoint_revert(checkpoint);
            }

            phase_results.push(crate::transaction::XLayerAAPhaseResult {
                success: phase_ok,
                gas_used: phase_gas_start.saturating_sub(gas_remaining),
            });

            // EIP-8130: "if any call in a phase reverts, all state changes for
            // that phase are discarded and remaining phases are skipped."
            if !phase_ok {
                break;
            }
        }

        let any_phase_succeeded = phase_results.iter().any(|r| r.success);
        let deploy_only_success = phase_results.is_empty() && !parts.code_placements.is_empty();
        let tx_succeeded = is_estimation || any_phase_succeeded || deploy_only_success;

        if !phase_results.is_empty() {
            evm.ctx()
                .journal_mut()
                .log(phase_statuses_system_log(TX_CONTEXT_ADDRESS, &phase_results));
        }

        let mut result_gas = Gas::new_spent(evm.ctx().tx().gas_limit());
        result_gas.erase_cost(gas_remaining + unused_verification_gas);
        if accumulated_refunds > 0 {
            result_gas.record_refund(accumulated_refunds);
        }

        let output = encode_phase_statuses(&phase_results);

        let instruction_result =
            if tx_succeeded { InstructionResult::Stop } else { InstructionResult::Revert };

        let mut frame_result = FrameResult::Call(CallOutcome::new(
            InterpreterResult { result: instruction_result, output, gas: result_gas },
            0..0,
        ));

        // `last_frame_result` and friends live on the inner op handler but
        // the dispatch goes through this handler's `Handler` impl, so call
        // the trait method on `self` rather than a nested sub-handler.
        Handler::last_frame_result(self, evm, &mut frame_result)?;
        Ok(frame_result)
    }

    fn last_frame_result(
        &mut self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        // Deposit-aware gas accounting is identical between XLayerAA and
        // op-revm's mainnet path; delegate.
        self.op.last_frame_result(evm, frame_result)
    }

    fn reimburse_caller(
        &self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        if evm.ctx().tx().tx_type() != XLAYERAA_TX_TYPE {
            return self.op.reimburse_caller(evm, frame_result);
        }

        // AA sponsored-tx refund: the payer (not the caller) is credited
        // with unused gas × effective gas price, plus any operator-fee refund.
        let mut additional_refund = U256::ZERO;
        if !evm.ctx().cfg().is_fee_charge_disabled() {
            let spec = evm.ctx().cfg().spec();
            additional_refund = evm.ctx().chain().operator_fee_refund(frame_result.gas(), spec);
        }

        let payer = evm.ctx().tx().xlayeraa_parts().payer;
        let basefee = evm.ctx().block().basefee() as u128;
        let effective_gas_price = evm.ctx().tx().effective_gas_price(basefee);
        let gas = frame_result.gas();
        let refund_amount = U256::from(
            effective_gas_price.saturating_mul((gas.remaining() + gas.refunded() as u64) as u128),
        ) + additional_refund;
        evm.ctx().journal_mut().load_account_mut(payer)?.incr_balance(refund_amount);
        Ok(())
    }

    fn refund(
        &self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
        eip7702_refund: i64,
    ) {
        // The gas-refund rules mirror op-revm's: deposit pre-Regolith skips
        // refund, everything else uses LONDON rules.
        frame_result.gas_mut().record_refund(eip7702_refund);
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let is_regolith = evm.ctx().cfg().spec().is_enabled_in(OpSpecId::REGOLITH);
        let is_gas_refund_disabled = is_deposit && !is_regolith;
        if !is_gas_refund_disabled {
            frame_result
                .gas_mut()
                .set_final_refund(evm.ctx().cfg().spec().into_eth_spec().is_enabled_in(SpecId::LONDON));
        }
    }

    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
        if is_deposit {
            return Ok(());
        }

        // Run the mainnet priority-fee credit to the coinbase, then distribute
        // L1 / base / operator fees to their respective recipients.
        self.op.mainnet.reward_beneficiary(evm, frame_result)?;

        let basefee = evm.ctx().block().basefee() as u128;
        let ctx = evm.ctx();
        let enveloped = ctx.tx().enveloped_tx().cloned();
        let spec = ctx.cfg().spec();
        let l1_block_info = ctx.chain_mut();

        let Some(enveloped_tx) = &enveloped else {
            return Err(ERROR::from_string(
                "[XLayerAA] Failed to load enveloped transaction.".into(),
            ));
        };

        let l1_cost = l1_block_info.calculate_tx_l1_cost(enveloped_tx, spec);
        let operator_fee_cost = if spec.is_enabled_in(OpSpecId::ISTHMUS) {
            l1_block_info.operator_fee_charge(
                enveloped_tx,
                U256::from(frame_result.gas().used()),
                spec,
            )
        } else {
            U256::ZERO
        };
        let base_fee_amount = U256::from(basefee.saturating_mul(frame_result.gas().used() as u128));

        for (recipient, amount) in [
            (L1_FEE_RECIPIENT, l1_cost),
            (BASE_FEE_RECIPIENT, base_fee_amount),
            (OPERATOR_FEE_RECIPIENT, operator_fee_cost),
        ] {
            ctx.journal_mut().balance_incr(recipient, amount)?;
        }

        Ok(())
    }

    fn execution_result(
        &mut self,
        evm: &mut Self::Evm,
        frame_result: <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<ExecutionResult<Self::HaltReason>, Self::Error> {
        match core::mem::replace(evm.ctx().error(), Ok(())) {
            Err(ContextError::Db(e)) => return Err(e.into()),
            Err(ContextError::Custom(e)) => return Err(Self::Error::from_string(e)),
            Ok(_) => (),
        }

        let exec_result =
            post_execution::output(evm.ctx(), frame_result).map_haltreason(OpHaltReason::Base);

        if exec_result.is_halt() {
            let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
            if is_deposit && evm.ctx().cfg().spec().is_enabled_in(OpSpecId::REGOLITH) {
                return Err(ERROR::from(OpTransactionError::HaltedDepositPostRegolith));
            }
        }
        evm.ctx().journal_mut().commit_tx();
        evm.ctx().chain_mut().clear_tx_l1_cost();
        evm.ctx().local_mut().clear();
        evm.frame_stack().clear();

        Ok(exec_result)
    }

    fn catch_error(
        &self,
        evm: &mut Self::Evm,
        error: Self::Error,
    ) -> Result<ExecutionResult<Self::HaltReason>, Self::Error> {
        // Deposit `catch_error` path is identical to op-revm; delegate.
        self.op.catch_error(evm, error)
    }
}

// ---------------------------------------------------------------------------
// InspectorHandler
// ---------------------------------------------------------------------------

impl<EVM, ERROR> InspectorHandler for XLayerAAHandler<EVM, ERROR, EthFrame<EthInterpreter>>
where
    EVM: InspectorEvmTr<
            Context: XLayerAAContextTr,
            Frame = EthFrame<EthInterpreter>,
            Inspector: Inspector<<EVM as revm::handler::EvmTr>::Context, EthInterpreter>,
        >,
    ERROR: EvmTrError<EVM> + From<OpTransactionError> + FromStringError + IsTxError,
{
    type IT = EthInterpreter;
}

/// Type alias matching op-revm's `OpError<CTX>`.
pub type XLayerAAError<CTX> = EVMError<
    <<CTX as ContextTr>::Db as revm::context_interface::Database>::Error,
    OpTransactionError,
>;

