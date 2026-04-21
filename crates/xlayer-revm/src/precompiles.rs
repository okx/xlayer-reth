//! XLayerAA (EIP-8130) system precompile helpers.
//!
//! Provides:
//!
//! - A [`sol!`] block ([`INonceManager`], [`ITxContext`], [`CallTuple`])
//!   that supplies ABI encoders, decoders, and selectors — no hand-rolled
//!   ABI serialization.
//! - [`aa_nonce_slot`] Solidity-mapping slot helper built on
//!   [`SolValue::abi_encode`].
//! - A [`XLayerAAPrecompiles`] decorator that wraps an inner
//!   [`PrecompileProvider`] and intercepts calls to the NonceManager /
//!   TxContext system addresses during XLayerAA transaction execution.
//!
//! Every value surfaced by the TxContext precompile (sender, payer,
//! `owner_id`, `max_cost`, `gas_limit`, `calls`) is derivable from
//! `context.tx()` + `tx.xlayeraa_parts()` at call time, so there is no
//! handler → precompile side channel — no thread-local, no transient storage.

use std::{boxed::Box, string::String, vec::Vec};

use alloy_sol_types::{sol, SolCall, SolValue};
use op_revm::OpSpecId;
use revm::{
    context::{Cfg, LocalContextTr},
    context_interface::{ContextTr, Transaction},
    handler::PrecompileProvider,
    interpreter::{CallInput, CallInputs, Gas, InstructionResult, InterpreterResult},
    primitives::{address, keccak256, uint, Address, Bytes, B256, U256},
    Database,
};

use crate::{
    constants::XLAYERAA_TX_TYPE,
    transaction::{XLayerAACall, XLayerAAParts, XLayerAATxTr},
};

// ---------------------------------------------------------------------------
// sol! bindings
// ---------------------------------------------------------------------------

sol! {
    /// Call record returned by [`ITxContext::getCalls`]. Matches the
    /// Solidity tuple `(address target, bytes data)`.
    struct CallTuple {
        address target;
        bytes data;
    }

    /// Interface served by the NonceManager system precompile.
    interface INonceManager {
        function getNonce(address account, uint256 nonceKey) external view returns (uint256);
    }

    /// Interface served by the TxContext system precompile.
    interface ITxContext {
        function getSender() external view returns (address);
        function getPayer() external view returns (address);
        function getOwnerId() external view returns (bytes32);
        function getMaxCost() external view returns (uint256);
        function getGasLimit() external view returns (uint256);
        function getCalls() external view returns (CallTuple[][] memory);
    }
}

impl From<&XLayerAACall> for CallTuple {
    fn from(c: &XLayerAACall) -> Self {
        Self { target: c.to, data: c.data.clone() }
    }
}

/// Derives `getGasLimit()` — the sender's execution-only gas budget.
#[inline]
fn aa_execution_gas_limit(parts: &XLayerAAParts, tx_gas_limit: u64) -> u64 {
    tx_gas_limit.saturating_sub(parts.aa_intrinsic_gas)
}

/// Derives `getMaxCost()` — the upper bound on spend visible to verifier
/// contracts. Excludes `payer_auth_cost` because the payer verifier may
/// still be executing when it calls `getMaxCost()`.
#[inline]
fn aa_max_cost(parts: &XLayerAAParts, tx_gas_limit: u64, max_fee_per_gas: U256) -> U256 {
    let exec_gas_limit = aa_execution_gas_limit(parts, tx_gas_limit);
    let known_intrinsic = parts.aa_intrinsic_gas.saturating_sub(parts.payer_intrinsic_gas);
    let total = U256::from(exec_gas_limit)
        + U256::from(known_intrinsic)
        + U256::from(parts.custom_verifier_gas_cap);
    total * max_fee_per_gas
}

// ---------------------------------------------------------------------------
// System precompile addresses and gas costs
// ---------------------------------------------------------------------------

/// NonceManager system precompile address.
pub const NONCE_MANAGER_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa02");

/// TxContext system precompile address.
pub const TX_CONTEXT_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa03");

/// Base storage slot for NonceManager nonce mapping.
pub const NONCE_BASE_SLOT: U256 = uint!(1_U256);

/// Gas cost for TxContext precompile calls.
pub const TX_CONTEXT_GAS: u64 = 100;

/// Gas cost for NonceManager precompile calls.
pub const NONCE_MANAGER_GAS: u64 = 2_100;

// ---------------------------------------------------------------------------
// Slot helper
// ---------------------------------------------------------------------------

/// Computes the NonceManager storage slot for `nonce[account][nonce_key]`.
///
/// Solidity mapping layout:
/// `keccak256(nonce_key ‖ keccak256(account ‖ NONCE_BASE_SLOT))`.
pub fn aa_nonce_slot(account: Address, nonce_key: U256) -> U256 {
    let inner = keccak256((account, NONCE_BASE_SLOT).abi_encode());
    let outer = keccak256((nonce_key, inner).abi_encode());
    U256::from_be_bytes(outer.0)
}

// ---------------------------------------------------------------------------
// Precompile execution
// ---------------------------------------------------------------------------

fn map_precompile_output(
    gas_limit: u64,
    output: Result<(u64, Bytes), String>,
) -> InterpreterResult {
    let mut result = InterpreterResult {
        result: InstructionResult::Return,
        gas: Gas::new(gas_limit),
        output: Bytes::new(),
    };

    match output {
        Ok((gas_used, bytes)) => {
            if gas_limit < gas_used {
                result.result = InstructionResult::PrecompileOOG;
            } else {
                let enough_gas = result.gas.record_cost(gas_used);
                debug_assert!(enough_gas, "gas should be sufficient after explicit limit check");
                result.output = bytes;
            }
        }
        Err(_) => {
            result.result = InstructionResult::PrecompileError;
        }
    }

    result
}

fn run_nonce_manager_precompile<CTX>(
    context: &mut CTX,
    input: &[u8],
) -> Result<(u64, Bytes), String>
where
    CTX: ContextTr<Cfg: Cfg<Spec = OpSpecId>, Tx: XLayerAATxTr>,
{
    let call = INonceManager::getNonceCall::abi_decode(input)
        .map_err(|e| format!("invalid getNonce call: {e}"))?;

    let slot = aa_nonce_slot(call.account, call.nonceKey);
    let storage_value =
        context.db_mut().storage(NONCE_MANAGER_ADDRESS, slot).map_err(|e| e.to_string())?;

    // Solidity returns a uint256 from getNonce — mask the upper bytes (the
    // packed AccountState layout reuses the high bits for other fields).
    let masked = storage_value & U256::from(u64::MAX);
    let encoded = INonceManager::getNonceCall::abi_encode_returns(&masked);

    Ok((NONCE_MANAGER_GAS, Bytes::from(encoded)))
}

fn run_tx_context_precompile<CTX>(context: &CTX, input: &[u8]) -> Result<(u64, Bytes), String>
where
    CTX: ContextTr<Cfg: Cfg<Spec = OpSpecId>, Tx: XLayerAATxTr>,
{
    if input.len() < 4 {
        return Err("invalid tx context input".to_string());
    }

    let tx = context.tx();
    let aa_parts = if tx.tx_type() == XLAYERAA_TX_TYPE { Some(tx.xlayeraa_parts()) } else { None };

    let selector: [u8; 4] = input[0..4].try_into().map_err(|_| "short selector".to_string())?;

    let output = match selector {
        ITxContext::getSenderCall::SELECTOR => {
            let sender = aa_parts.map_or(Address::ZERO, |p| p.sender);
            ITxContext::getSenderCall::abi_encode_returns(&sender)
        }
        ITxContext::getPayerCall::SELECTOR => {
            let payer = aa_parts.map_or(Address::ZERO, |p| p.payer);
            ITxContext::getPayerCall::abi_encode_returns(&payer)
        }
        ITxContext::getOwnerIdCall::SELECTOR => {
            let owner_id = aa_parts.map_or(B256::ZERO, |p| p.owner_id);
            ITxContext::getOwnerIdCall::abi_encode_returns(&owner_id)
        }
        ITxContext::getMaxCostCall::SELECTOR => {
            let max_cost = aa_parts.map_or(U256::ZERO, |p| {
                aa_max_cost(p, tx.gas_limit(), U256::from(tx.max_fee_per_gas()))
            });
            ITxContext::getMaxCostCall::abi_encode_returns(&max_cost)
        }
        ITxContext::getGasLimitCall::SELECTOR => {
            let gas_limit = aa_parts.map_or(0u64, |p| aa_execution_gas_limit(p, tx.gas_limit()));
            ITxContext::getGasLimitCall::abi_encode_returns(&U256::from(gas_limit))
        }
        ITxContext::getCallsCall::SELECTOR => {
            let empty: Vec<Vec<XLayerAACall>> = Vec::new();
            let phases = aa_parts.map_or(&empty[..], |p| &p.call_phases);
            let phases_sol: Vec<Vec<CallTuple>> =
                phases.iter().map(|p| p.iter().map(CallTuple::from).collect()).collect();
            ITxContext::getCallsCall::abi_encode_returns(&phases_sol)
        }
        _ => return Err("unknown tx context selector".to_string()),
    };

    Ok((TX_CONTEXT_GAS, Bytes::from(output)))
}

// ---------------------------------------------------------------------------
// Decorator precompile provider
// ---------------------------------------------------------------------------

/// Precompile provider decorator: wraps any [`PrecompileProvider`] and
/// intercepts NonceManager / TxContext system addresses when an XLayerAA
/// transaction is in flight. All other calls delegate to the inner provider.
#[derive(Debug, Clone)]
pub struct XLayerAAPrecompiles<Inner> {
    inner: Inner,
}

impl<Inner> XLayerAAPrecompiles<Inner> {
    /// Wraps an inner precompile provider.
    pub const fn new(inner: Inner) -> Self {
        Self { inner }
    }

    /// Returns a reference to the inner provider.
    pub const fn inner(&self) -> &Inner {
        &self.inner
    }

    /// Returns a mutable reference to the inner provider.
    pub fn inner_mut(&mut self) -> &mut Inner {
        &mut self.inner
    }

    /// Unwraps the inner provider.
    pub fn into_inner(self) -> Inner {
        self.inner
    }
}

impl<Inner, CTX> PrecompileProvider<CTX> for XLayerAAPrecompiles<Inner>
where
    Inner: PrecompileProvider<CTX, Output = InterpreterResult>,
    CTX: ContextTr<Cfg: Cfg<Spec = OpSpecId>, Tx: XLayerAATxTr>,
{
    type Output = InterpreterResult;

    #[inline]
    fn set_spec(&mut self, spec: <CTX::Cfg as Cfg>::Spec) -> bool {
        self.inner.set_spec(spec)
    }

    #[inline]
    fn run(
        &mut self,
        context: &mut CTX,
        inputs: &CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        let is_aa_tx = context.tx().tx_type() == XLAYERAA_TX_TYPE;

        if is_aa_tx
            && (inputs.bytecode_address == NONCE_MANAGER_ADDRESS
                || inputs.bytecode_address == TX_CONTEXT_ADDRESS)
        {
            let input_bytes: Vec<u8> = match &inputs.input {
                CallInput::SharedBuffer(range) => context
                    .local()
                    .shared_memory_buffer_slice(range.clone())
                    .map(|slice| slice.to_vec())
                    .unwrap_or_default(),
                CallInput::Bytes(bytes) => bytes.to_vec(),
            };

            if inputs.bytecode_address == NONCE_MANAGER_ADDRESS {
                let output = run_nonce_manager_precompile(context, &input_bytes);
                return Ok(Some(map_precompile_output(inputs.gas_limit, output)));
            }

            // TX_CONTEXT_ADDRESS
            let output = run_tx_context_precompile(context, &input_bytes);
            return Ok(Some(map_precompile_output(inputs.gas_limit, output)));
        }

        self.inner.run(context, inputs)
    }

    #[inline]
    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        let mut addresses: Vec<Address> = self.inner.warm_addresses().collect();
        addresses.push(NONCE_MANAGER_ADDRESS);
        addresses.push(TX_CONTEXT_ADDRESS);
        Box::new(addresses.into_iter())
    }

    #[inline]
    fn contains(&self, address: &Address) -> bool {
        *address == NONCE_MANAGER_ADDRESS
            || *address == TX_CONTEXT_ADDRESS
            || self.inner.contains(address)
    }
}
