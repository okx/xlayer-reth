//! Pure-logic helpers used by [`XLayerAAHandler`](super::XLayerAAHandler).
//!
//! Each helper is a standalone function over revm traits so the handler
//! phase methods can compose them without carrying state. Ported from
//! base-revm `handler.rs` with names rewritten from `eip8130_*` / `Eip8130`
//! to `xlayeraa_*` / `XLayerAA`.

use std::{boxed::Box, collections::HashMap};

use alloy_sol_types::SolValue;
use op_revm::transaction::error::OpTransactionError;
use revm::{
    context::{
        LocalContextTr,
        journaled_state::account::JournaledAccountTr,
        result::InvalidTransaction,
    },
    context_interface::{Block, Cfg, ContextTr, JournalTr},
    handler::{
        EvmTr, FrameResult, Handler, MainnetHandler,
        evm::FrameTr,
        handler::EvmTrError,
    },
    interpreter::{
        SharedMemory,
        interpreter_action::{CallInput, CallInputs, CallScheme, CallValue, FrameInit, FrameInput},
    },
    primitives::{Address, B256, Bytes, U256, address, keccak256, uint},
};

use crate::{
    constants::{DELEGATE_VERIFIER_ADDRESS, OWNER_SCOPE_CONFIG},
    handler::XLayerAAContextTr,
    policy::{
        PendingOwnerState, PendingOwnerValidationError, pending_owner_state_for_change,
        validate_pending_owner_state,
    },
    transaction::XLayerAAParts,
};

// ---------------------------------------------------------------------------
// Handler-local constants (mirror base-revm's `handler.rs`)
// ---------------------------------------------------------------------------

/// Estimated calldata gas for a K1 auth blob that is absent during
/// `eth_estimateGas` but will be present in the real transaction.
pub const ESTIMATION_AUTH_CALLDATA_GAS: u64 = 1_100;

/// Gas delta between cold and warm nonce-key SSTORE costs.
pub const NONCE_COLD_WARM_DELTA: u64 = 17_100;

/// `AccountConfiguration` deployed contract address (CREATE2 from
/// `Deploy.s.sol` with salt `0`).
pub const ACCOUNT_CONFIG_ADDRESS: Address = address!("0x4F20618Cf5c160e7AA385268721dA968F86F0e61");

/// Native K1/ecrecover verifier sentinel (`address(1)`).
pub const K1_VERIFIER_ADDRESS: Address = address!("0x0000000000000000000000000000000000000001");

/// Sentinel verifier written when the implicit EOA owner is explicitly revoked.
pub const REVOKED_VERIFIER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

/// Monotonic cache: once the `AccountConfiguration` contract is detected,
/// the handler skips the code-existence check on subsequent calls.
pub static ACCOUNT_CONFIG_DEPLOYED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Base storage slot for the nonce mapping in `NonceManager` (slot index 1).
pub const NONCE_BASE_SLOT: U256 = uint!(1_U256);

/// Base storage slot for the packed `_accountState` mapping in
/// `AccountConfig` (slot index 1).
pub const LOCK_BASE_SLOT: U256 = uint!(1_U256);

/// Sentinel nonce key that activates nonce-free mode.
pub const NONCE_KEY_MAX: U256 = U256::MAX;

/// `expiringNonceSeen` base storage slot.
pub const EXPIRING_SEEN_BASE_SLOT: U256 = uint!(2_U256);

/// `expiringNonceRing` base storage slot.
pub const EXPIRING_RING_BASE_SLOT: U256 = uint!(3_U256);

/// `expiringNonceRingPtr` storage slot.
pub const EXPIRING_RING_PTR_SLOT: U256 = uint!(4_U256);

/// Circular buffer capacity for nonce-free replay protection.
pub const EXPIRING_NONCE_SET_CAPACITY: u32 = 300_000;

/// Maximum allowed expiry window for nonce-free transactions.
pub const NONCE_FREE_MAX_EXPIRY_WINDOW: u64 = 30;

/// Owner-config mapping base slot in `AccountConfig` (slot index 0).
pub const OWNER_CONFIG_BASE_SLOT: U256 = U256::ZERO;

// ---------------------------------------------------------------------------
// Slot / word helpers
// ---------------------------------------------------------------------------

/// Hashes an `abi.encode`-serialized key pair into a storage slot `U256`.
#[inline]
fn mapping_slot<T: SolValue>(key: T) -> U256 {
    U256::from_be_bytes(keccak256(key.abi_encode()).0)
}

/// Computes the `NonceManager` storage slot for `nonce[account][nonce_key]`.
///
/// Solidity layout:
/// `keccak256(nonce_key ‖ keccak256(account ‖ NONCE_BASE_SLOT))`.
pub fn aa_nonce_slot(account: Address, nonce_key: U256) -> U256 {
    let inner = keccak256((account, NONCE_BASE_SLOT).abi_encode());
    mapping_slot((nonce_key, inner))
}

/// Computes the storage slot for `expiringNonceSeen[txHash]`.
pub fn aa_expiring_seen_slot(tx_hash: B256) -> U256 {
    mapping_slot((tx_hash, EXPIRING_SEEN_BASE_SLOT))
}

/// Computes the storage slot for `expiringNonceRing[index]`.
pub fn aa_expiring_ring_slot(index: u32) -> U256 {
    mapping_slot((U256::from(index), EXPIRING_RING_BASE_SLOT))
}

/// Computes the `AccountConfig` storage slot for `lock_state(account)`.
pub fn aa_lock_slot(account: Address) -> U256 {
    mapping_slot((account, LOCK_BASE_SLOT))
}

/// Computes the `AccountConfig` storage slot for
/// `owner_config(account, owner_id)`.
pub fn aa_owner_config_slot(account: Address, owner_id: U256) -> U256 {
    let inner = keccak256((owner_id, OWNER_CONFIG_BASE_SLOT).abi_encode());
    mapping_slot((account, inner))
}

/// Parses a packed `owner_config` word into `(verifier_address, scope)`.
///
/// Layout: `[zeros(11) | scope(1) | verifier(20)]` (big-endian 32 bytes).
pub fn parse_owner_config_word(word: U256) -> (Address, u8) {
    let bytes = word.to_be_bytes::<32>();
    let scope = bytes[11];
    let verifier = Address::from_slice(&bytes[12..32]);
    (verifier, scope)
}

/// Reads one sequence value from packed
/// `AccountState { multichain, local, unlocksAt, unlockDelay }`.
pub fn read_packed_sequence(slot_value: U256, is_multichain: bool) -> u64 {
    if is_multichain { slot_value.as_limbs()[0] } else { (slot_value >> 64_u8).as_limbs()[0] }
}

/// Extra gas to reserve during `eth_estimateGas` for auth blob calldata that
/// will be present in the real transaction but is absent in the estimation
/// request.
pub fn estimation_calldata_overhead(parts: &XLayerAAParts) -> u64 {
    let mut overhead = 0;
    if parts.sender_auth_empty {
        overhead += ESTIMATION_AUTH_CALLDATA_GAS;
    }
    if parts.payer_auth_empty {
        overhead += ESTIMATION_AUTH_CALLDATA_GAS;
    }
    overhead
}

/// Constructs an `InvalidTransaction::Str` error wrapped as
/// `OpTransactionError::Base`.
pub fn xlayeraa_invalid_tx<ERROR: From<OpTransactionError>>(
    msg: impl Into<std::borrow::Cow<'static, str>>,
) -> ERROR {
    OpTransactionError::Base(InvalidTransaction::Str(msg.into())).into()
}

// ---------------------------------------------------------------------------
// Owner-config validators
// ---------------------------------------------------------------------------

/// Validates owner authorization against effective state: pending in-tx
/// owner changes first, then on-chain storage.
pub fn validate_owner_against_effective_config<EVM, ERROR>(
    evm: &mut EVM,
    account: Address,
    owner_id: U256,
    expected_verifier: Address,
    required_scope: u8,
    allow_implicit_eoa: bool,
    pending_owner_overrides: Option<&HashMap<U256, PendingOwnerState>>,
) -> Result<(), ERROR>
where
    EVM: EvmTr<Context: XLayerAAContextTr>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError>,
{
    if let Some(state) = pending_owner_overrides.and_then(|m| m.get(&owner_id)) {
        validate_pending_owner_state(state, expected_verifier, required_scope).map_err(|err| {
            let msg: std::borrow::Cow<'static, str> = match err {
                PendingOwnerValidationError::Revoked => {
                    "owner explicitly revoked in pending config changes".into()
                }
                PendingOwnerValidationError::VerifierMismatch { expected, actual } => {
                    format!("verifier mismatch: expected {expected}, got {actual}").into()
                }
                PendingOwnerValidationError::MissingScope { required_scope } => {
                    format!("owner lacks required scope bit 0x{required_scope:02x}").into()
                }
            };
            xlayeraa_invalid_tx::<ERROR>(msg)
        })?;
        return Ok(());
    }

    evm.ctx().journal_mut().load_account(ACCOUNT_CONFIG_ADDRESS)?;
    let slot = aa_owner_config_slot(account, owner_id);
    let config_word = evm.ctx().journal_mut().sload(ACCOUNT_CONFIG_ADDRESS, slot)?.data;
    let (on_chain_verifier, scope) = parse_owner_config_word(config_word);

    if on_chain_verifier == REVOKED_VERIFIER {
        return Err(xlayeraa_invalid_tx::<ERROR>(
            "native verifier owner explicitly revoked (REVOKED_VERIFIER sentinel)",
        ));
    }

    if on_chain_verifier == Address::ZERO {
        if allow_implicit_eoa {
            let implicit_owner_id = {
                let mut buf = [0u8; 32];
                buf[..20].copy_from_slice(account.as_slice());
                U256::from_be_bytes(buf)
            };
            if owner_id == implicit_owner_id && expected_verifier == K1_VERIFIER_ADDRESS {
                return Ok(());
            }
        }
        return Err(xlayeraa_invalid_tx::<ERROR>(
            "owner_config not found and implicit EOA rule does not apply",
        ));
    }

    if on_chain_verifier != expected_verifier {
        return Err(xlayeraa_invalid_tx::<ERROR>(format!(
            "verifier mismatch: expected {expected_verifier}, got {on_chain_verifier}",
        )));
    }
    if scope != 0 && (scope & required_scope) == 0 {
        return Err(xlayeraa_invalid_tx::<ERROR>(format!(
            "owner lacks required scope bit 0x{required_scope:02x}",
        )));
    }
    Ok(())
}

/// Validates that `owner_id` is registered in `AccountConfig` with the
/// expected verifier address and required scope.
pub fn validate_owner_config<EVM, ERROR>(
    evm: &mut EVM,
    account: Address,
    owner_id: U256,
    expected_verifier: Address,
    required_scope: u8,
    pending_owner_overrides: Option<&HashMap<U256, PendingOwnerState>>,
) -> Result<(), ERROR>
where
    EVM: EvmTr<Context: XLayerAAContextTr>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError>,
{
    validate_owner_against_effective_config::<EVM, ERROR>(
        evm,
        account,
        owner_id,
        expected_verifier,
        required_scope,
        false,
        pending_owner_overrides,
    )
}

/// Re-validates a native verifier's `owner_config` at inclusion time.
///
/// For `DELEGATE` verifiers this requires two SLOADs: one to resolve the
/// delegation target and another to check the inner verifier's config.
pub fn validate_native_verifier_owner<EVM, ERROR>(
    evm: &mut EVM,
    account: Address,
    verifier: Address,
    owner_id: B256,
    required_scope: u8,
    pending_owner_overrides: Option<&HashMap<U256, PendingOwnerState>>,
) -> Result<(), ERROR>
where
    EVM: EvmTr<Context: XLayerAAContextTr>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError>,
{
    let owner_id_uint = U256::from_be_bytes(owner_id.0);
    let has_pending_override =
        pending_owner_overrides.and_then(|m| m.get(&owner_id_uint)).is_some();

    validate_owner_against_effective_config::<EVM, ERROR>(
        evm,
        account,
        owner_id_uint,
        verifier,
        required_scope,
        true,
        pending_owner_overrides,
    )?;

    if verifier == DELEGATE_VERIFIER_ADDRESS && !has_pending_override {
        let inner_slot = aa_owner_config_slot(account, owner_id_uint);
        let inner_word = evm.ctx().journal_mut().sload(ACCOUNT_CONFIG_ADDRESS, inner_slot)?.data;
        let (inner_verifier, inner_scope) = parse_owner_config_word(inner_word);

        if inner_verifier == Address::ZERO {
            return Err(xlayeraa_invalid_tx::<ERROR>("delegate inner verifier owner revoked"));
        }
        if inner_scope != 0 && (inner_scope & required_scope) == 0 {
            return Err(xlayeraa_invalid_tx::<ERROR>(format!(
                "delegate inner owner lacks required scope 0x{required_scope:02x}"
            )));
        }
    }

    Ok(())
}

/// Re-validates config-change preconditions at inclusion time.
///
/// - account is not locked
/// - each config-change sequence matches expected on-chain value, with
///   in-tx chaining across multiple entries.
pub fn validate_config_change_preconditions<EVM, ERROR>(
    evm: &mut EVM,
    sender: Address,
    parts: &XLayerAAParts,
) -> Result<(), ERROR>
where
    EVM: EvmTr<Context: XLayerAAContextTr>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError>,
{
    if parts.sequence_updates.is_empty() && parts.config_writes.is_empty() {
        return Ok(());
    }

    if !ACCOUNT_CONFIG_DEPLOYED.load(std::sync::atomic::Ordering::Relaxed) {
        let acct = evm.ctx().journal_mut().load_account_with_code_mut(ACCOUNT_CONFIG_ADDRESS)?.data;
        let has_code = acct.account().info.code_hash != keccak256([]);
        drop(acct);
        if has_code {
            ACCOUNT_CONFIG_DEPLOYED.store(true, std::sync::atomic::Ordering::Relaxed);
        } else {
            return Err(xlayeraa_invalid_tx::<ERROR>(
                "config changes require AccountConfiguration to be deployed",
            ));
        }
    }

    evm.ctx().journal_mut().load_account(ACCOUNT_CONFIG_ADDRESS)?;

    // Lock-state check: locked accounts cannot process config changes.
    let lock_slot = aa_lock_slot(sender);
    let lock_word = evm.ctx().journal_mut().sload(ACCOUNT_CONFIG_ADDRESS, lock_slot)?.data;
    let lock_bytes = lock_word.to_be_bytes::<32>();
    let mut ua = [0u8; 8];
    ua[3..8].copy_from_slice(&lock_bytes[11..16]);
    let unlocks_at = u64::from_be_bytes(ua);
    let now: u64 = evm.ctx().block().timestamp().to();
    if now < unlocks_at {
        return Err(xlayeraa_invalid_tx::<ERROR>("config changes not allowed: account is locked"));
    }

    if parts.sequence_updates.is_empty() {
        return Ok(());
    }

    let seq_slot = parts.sequence_updates[0].slot;
    let packed = evm.ctx().journal_mut().sload(ACCOUNT_CONFIG_ADDRESS, seq_slot)?.data;
    let mut expected_multichain = read_packed_sequence(packed, true);
    let mut expected_local = read_packed_sequence(packed, false);

    for upd in &parts.sequence_updates {
        let tx_sequence = upd.new_value.checked_sub(1).ok_or_else(|| {
            xlayeraa_invalid_tx::<ERROR>("invalid config change sequence (underflow)")
        })?;

        if upd.is_multichain {
            if tx_sequence != expected_multichain {
                return Err(xlayeraa_invalid_tx::<ERROR>(format!(
                    "config change sequence mismatch: expected {expected_multichain}, got {tx_sequence}"
                )));
            }
            expected_multichain = upd.new_value;
        } else {
            if tx_sequence != expected_local {
                return Err(xlayeraa_invalid_tx::<ERROR>(format!(
                    "config change sequence mismatch: expected {expected_local}, got {tx_sequence}"
                )));
            }
            expected_local = upd.new_value;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Verifier STATICCALL / authorizer-chain
// ---------------------------------------------------------------------------

/// Runs a custom verifier STATICCALL and decodes the returned `owner_id`.
///
/// Charges gas against the transaction's custom-verifier budget via
/// `verification_gas_used`.
#[allow(clippy::too_many_arguments)]
pub fn run_custom_verifier_staticcall<EVM, ERROR, FRAME>(
    mainnet: &mut MainnetHandler<EVM, ERROR, FRAME>,
    evm: &mut EVM,
    verifier: Address,
    calldata: &Bytes,
    caller: Address,
    verification_gas_cap: u64,
    verification_gas_used: &mut u64,
    call_failed_msg: &'static str,
    invalid_owner_id_msg: &'static str,
) -> Result<U256, ERROR>
where
    EVM: EvmTr<Context: XLayerAAContextTr, Frame = FRAME>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError>,
    FRAME: FrameTr<FrameResult = FrameResult, FrameInit = FrameInit>,
{
    evm.ctx().journal_mut().load_account(verifier)?;

    let call_gas = verification_gas_cap.saturating_sub(*verification_gas_used);
    let call_inputs = CallInputs {
        input: CallInput::Bytes(calldata.clone()),
        return_memory_offset: 0..0,
        gas_limit: call_gas,
        bytecode_address: verifier,
        known_bytecode: None,
        target_address: verifier,
        caller,
        value: CallValue::Transfer(U256::ZERO),
        scheme: CallScheme::StaticCall,
        is_static: true,
    };

    let frame_init = FrameInit {
        depth: 0,
        memory: {
            let ctx = evm.ctx();
            let mut mem =
                SharedMemory::new_with_buffer(ctx.local().shared_memory_buffer().clone());
            mem.set_memory_limit(ctx.cfg().memory_limit());
            mem
        },
        frame_input: FrameInput::Call(Box::new(call_inputs)),
    };

    let result = mainnet.run_exec_loop(evm, frame_init)?;
    let used = call_gas.saturating_sub(result.gas().remaining());
    *verification_gas_used = verification_gas_used.saturating_add(used);

    if !result.interpreter_result().result.is_ok() {
        return Err(xlayeraa_invalid_tx::<ERROR>(call_failed_msg));
    }

    let output = result.interpreter_result().output.as_ref();
    if output.len() < 32 {
        return Err(xlayeraa_invalid_tx::<ERROR>(invalid_owner_id_msg));
    }

    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&output[..32]);
    Ok(U256::from_be_bytes(bytes))
}

/// Validates the config change authorizer chain.
///
/// For each authorizer entry:
/// - custom verifiers: STATICCALL to resolve `owner_id`, then check
///   `owner_config(sender, owner_id)` for `CONFIG` scope;
/// - native verifiers: check the pre-authenticated `owner_id` directly.
///
/// **Chaining:** AUTHORIZE operations from earlier entries are tracked in an
/// in-memory map so a single tx can chain: entry 1 (old owner) adds new
/// owner X; entry 2 (X) performs further changes.
pub fn validate_authorizer_chain<EVM, ERROR, FRAME>(
    mainnet: &mut MainnetHandler<EVM, ERROR, FRAME>,
    evm: &mut EVM,
    sender: Address,
    parts: &XLayerAAParts,
    verification_gas_used: &mut u64,
) -> Result<HashMap<U256, PendingOwnerState>, ERROR>
where
    EVM: EvmTr<Context: XLayerAAContextTr, Frame = FRAME>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError>,
    FRAME: FrameTr<FrameResult = FrameResult, FrameInit = FrameInit>,
{
    if parts.authorizer_validations.is_empty() {
        return Ok(HashMap::new());
    }

    let mut pending_owners: HashMap<U256, PendingOwnerState> = HashMap::new();

    for validation in &parts.authorizer_validations {
        if validation.verify_call.is_none()
            && validation.owner_id == B256::ZERO
            && validation.owner_changes.is_empty()
        {
            continue;
        }

        let owner_id = if let Some(verify_call) = &validation.verify_call {
            run_custom_verifier_staticcall::<EVM, ERROR, FRAME>(
                mainnet,
                evm,
                verify_call.verifier,
                &verify_call.calldata,
                sender,
                parts.custom_verifier_gas_cap,
                verification_gas_used,
                "config change authorizer STATICCALL failed",
                "config change authorizer returned invalid owner_id",
            )?
        } else {
            U256::from_be_bytes(validation.owner_id.0)
        };

        if owner_id.is_zero() {
            return Err(xlayeraa_invalid_tx::<ERROR>(
                "config change authorizer returned zero owner_id",
            ));
        }

        validate_owner_against_effective_config::<EVM, ERROR>(
            evm,
            sender,
            owner_id,
            validation.verifier,
            OWNER_SCOPE_CONFIG,
            true,
            Some(&pending_owners),
        )?;

        for op in &validation.owner_changes {
            if let Some(state) =
                pending_owner_state_for_change(op.change_type, op.verifier, op.scope)
            {
                pending_owners.insert(U256::from_be_bytes(op.owner_id.0), state);
            }
        }
    }

    Ok(pending_owners)
}

