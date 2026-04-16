//! Precompile provider for EIP-8130 AA system contracts.
//!
//! Wraps revm's [`EthPrecompiles`] to intercept calls to the NonceManager
//! (`0x…aa02`) and TxContext (`0x…aa03`) addresses. All other addresses
//! are delegated to the standard Ethereum precompiles.
//!
//! The NonceManager precompile reads from storage via the EVM context.
//! The TxContext precompile reads from thread-local state populated by
//! the handler before EVM execution.

use alloy_primitives::{Address, Bytes};
use revm::{
    context::Cfg,
    context_interface::{ContextTr, LocalContextTr},
    handler::{EthPrecompiles, PrecompileProvider},
    interpreter::{CallInput, CallInputs, Gas, InstructionResult, InterpreterResult},
    primitives::hardfork::SpecId,
};
use xlayer_eip8130_consensus::{NONCE_MANAGER_ADDRESS, TX_CONTEXT_ADDRESS};

use crate::tx_context::get_eip8130_tx_context;

/// Precompile provider that adds EIP-8130 NonceManager and TxContext
/// on top of the standard Ethereum precompiles.
///
/// The AA precompiles are only active when `aa_enabled` is `true`.
/// The caller is responsible for setting this flag based on the
/// NativeAA hardfork activation status at the current block's timestamp.
#[derive(Debug, Clone)]
pub struct Eip8130Precompiles {
    /// Delegates to standard Ethereum precompiles.
    pub inner: EthPrecompiles,
    /// Current spec for gating.
    pub spec: SpecId,
    /// Whether the NativeAA hardfork is active (AA precompiles enabled).
    pub aa_enabled: bool,
}

impl Default for Eip8130Precompiles {
    fn default() -> Self {
        let inner = EthPrecompiles::default();
        Self { spec: inner.spec, inner, aa_enabled: false }
    }
}

impl Eip8130Precompiles {
    /// Creates a new precompile provider with the given spec.
    ///
    /// AA precompiles default to **disabled**; call [`with_aa_enabled`](Self::with_aa_enabled)
    /// to activate them.
    pub fn new(spec: SpecId) -> Self {
        Self { spec, ..Self::default() }
    }

    /// Returns a copy with AA precompiles enabled or disabled.
    pub fn with_aa_enabled(mut self, enabled: bool) -> Self {
        self.aa_enabled = enabled;
        self
    }

    /// Returns whether the address is an EIP-8130 or standard precompile.
    pub fn contains(&self, address: &Address) -> bool {
        if self.aa_enabled && (*address == NONCE_MANAGER_ADDRESS || *address == TX_CONTEXT_ADDRESS)
        {
            return true;
        }
        self.inner.contains(address)
    }

    /// Returns all warm addresses (standard + EIP-8130).
    pub fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        let mut addrs: Vec<Address> = self.inner.warm_addresses().collect();
        if self.aa_enabled {
            addrs.push(NONCE_MANAGER_ADDRESS);
            addrs.push(TX_CONTEXT_ADDRESS);
        }
        Box::new(addrs.into_iter())
    }
}

impl<CTX> PrecompileProvider<CTX> for Eip8130Precompiles
where
    CTX: ContextTr<Cfg: Cfg<Spec: Into<SpecId>>>,
{
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <CTX::Cfg as Cfg>::Spec) -> bool {
        let spec_id: SpecId = spec.clone().into();
        self.spec = spec_id;
        <EthPrecompiles as PrecompileProvider<CTX>>::set_spec(&mut self.inner, spec)
    }

    fn run(
        &mut self,
        context: &mut CTX,
        inputs: &CallInputs,
    ) -> Result<Option<InterpreterResult>, String> {
        if self.aa_enabled {
            let input_bytes = extract_input_bytes(context, inputs);

            if inputs.bytecode_address == NONCE_MANAGER_ADDRESS {
                return run_nonce_manager(inputs.gas_limit, context, &input_bytes);
            }

            if inputs.bytecode_address == TX_CONTEXT_ADDRESS {
                return run_tx_context(inputs.gas_limit, &input_bytes);
            }
        }

        // Delegate to standard Ethereum precompiles.
        self.inner.run(context, inputs)
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        Eip8130Precompiles::warm_addresses(self)
    }

    fn contains(&self, address: &Address) -> bool {
        Eip8130Precompiles::contains(self, address)
    }
}

/// Extracts input bytes from either shared memory or direct bytes.
fn extract_input_bytes<CTX: ContextTr>(context: &CTX, inputs: &CallInputs) -> Vec<u8> {
    match &inputs.input {
        CallInput::SharedBuffer(range) => context
            .local()
            .shared_memory_buffer_slice(range.clone())
            .map(|s| s.to_vec())
            .unwrap_or_default(),
        CallInput::Bytes(bytes) => bytes.to_vec(),
    }
}

/// Runs the NonceManager precompile: `getNonce(address, uint256) → uint64`.
fn run_nonce_manager<CTX: ContextTr>(
    gas_limit: u64,
    context: &mut CTX,
    input: &[u8],
) -> Result<Option<InterpreterResult>, String> {
    let result = xlayer_eip8130_consensus::handle_nonce_manager(context.db_mut(), input);

    Ok(Some(map_precompile_result(gas_limit, result)))
}

/// Runs the TxContext precompile: routes to getSender/getPayer/etc.
fn run_tx_context(gas_limit: u64, input: &[u8]) -> Result<Option<InterpreterResult>, String> {
    let ctx = get_eip8130_tx_context();
    let result = match ctx {
        Some(ref c) => {
            let values = c.to_values();
            xlayer_eip8130_consensus::handle_tx_context(&values, input)
        }
        None => Err(xlayer_eip8130_consensus::PrecompileError::InvalidInput),
    };

    Ok(Some(map_precompile_result(gas_limit, result)))
}

/// Maps a consensus-level precompile result into a revm `InterpreterResult`.
fn map_precompile_result(
    gas_limit: u64,
    result: Result<(u64, Bytes), xlayer_eip8130_consensus::PrecompileError>,
) -> InterpreterResult {
    let mut gas = Gas::new(gas_limit);

    match result {
        Ok((gas_used, output)) => {
            if !gas.record_cost(gas_used) {
                return InterpreterResult {
                    result: InstructionResult::PrecompileOOG,
                    gas,
                    output: Bytes::new(),
                };
            }
            InterpreterResult { result: InstructionResult::Return, gas, output }
        }
        Err(_) => InterpreterResult {
            result: InstructionResult::PrecompileError,
            gas,
            output: Bytes::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aa_enabled_provider() -> Eip8130Precompiles {
        Eip8130Precompiles::default().with_aa_enabled(true)
    }

    #[test]
    fn contains_aa_precompiles_when_enabled() {
        let provider = aa_enabled_provider();
        assert!(provider.contains(&NONCE_MANAGER_ADDRESS));
        assert!(provider.contains(&TX_CONTEXT_ADDRESS));
    }

    #[test]
    fn does_not_contain_aa_precompiles_when_disabled() {
        let provider = Eip8130Precompiles::default();
        assert!(!provider.aa_enabled);
        assert!(!provider.contains(&NONCE_MANAGER_ADDRESS));
        assert!(!provider.contains(&TX_CONTEXT_ADDRESS));
    }

    #[test]
    fn contains_standard_precompiles() {
        let provider = Eip8130Precompiles::default();
        // ecrecover address
        let ecrecover =
            Address::from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert!(provider.contains(&ecrecover));
    }

    #[test]
    fn warm_addresses_include_aa_when_enabled() {
        let provider = aa_enabled_provider();
        let addrs: Vec<Address> = provider.warm_addresses().collect();
        assert!(addrs.contains(&NONCE_MANAGER_ADDRESS));
        assert!(addrs.contains(&TX_CONTEXT_ADDRESS));
    }

    #[test]
    fn warm_addresses_exclude_aa_when_disabled() {
        let provider = Eip8130Precompiles::default();
        let addrs: Vec<Address> = provider.warm_addresses().collect();
        assert!(!addrs.contains(&NONCE_MANAGER_ADDRESS));
        assert!(!addrs.contains(&TX_CONTEXT_ADDRESS));
    }
}
