//! Thread-local storage for EIP-8130 transaction context.
//!
//! Revm's `PrecompileProvider::run` receives only a generic `CTX` and
//! `CallInputs`, which cannot carry AA-specific fields like `sender`,
//! `payer`, or `call_phases`. We use thread-local storage to bridge the
//! gap: the handler populates it before EVM execution and precompiles
//! read from it during `STATICCALL`s.

use std::cell::RefCell;

use alloy_primitives::{Address, Bytes, B256, U256};
use xlayer_eip8130_consensus::TxContextValues;

/// Lightweight snapshot of EIP-8130 tx fields needed by the TxContext
/// precompile during EVM execution. Stored in a thread-local.
#[derive(Clone, Debug)]
pub struct Eip8130TxContext {
    /// Resolved sender address.
    pub sender: Address,
    /// Resolved payer address (same as sender for self-pay).
    pub payer: Address,
    /// Authenticated owner ID (`bytes32`).
    pub owner_id: B256,
    /// Execution-only gas limit (excluding intrinsic + verifier cap).
    pub gas_limit: u64,
    /// Maximum cost: `(gas_limit + known_intrinsic) * max_fee_per_gas`.
    pub max_cost: U256,
    /// Phased calls: `calls[phase_index][call_index] = (target, data)`.
    pub call_phases: Vec<Vec<(Address, Bytes)>>,
}

impl Eip8130TxContext {
    /// Converts into the consensus crate's `TxContextValues` for precompile
    /// handling.
    pub fn to_values(&self) -> TxContextValues {
        TxContextValues {
            sender: self.sender,
            payer: self.payer,
            owner_id: self.owner_id,
            gas_limit: self.gas_limit,
            max_cost: self.max_cost,
            calls: self.call_phases.clone(),
        }
    }
}

thread_local! {
    static EIP8130_TX_CONTEXT: RefCell<Option<Eip8130TxContext>> = const { RefCell::new(None) };
}

/// Stores the AA transaction context for the current thread.
///
/// Called by the handler before EVM execution begins.
pub fn set_eip8130_tx_context(ctx: Eip8130TxContext) {
    EIP8130_TX_CONTEXT.with(|c| *c.borrow_mut() = Some(ctx));
}

/// Retrieves the AA transaction context for the current thread.
///
/// Called by precompile handlers during EVM execution.
pub fn get_eip8130_tx_context() -> Option<Eip8130TxContext> {
    EIP8130_TX_CONTEXT.with(|c| c.borrow().clone())
}

/// Clears the AA transaction context for the current thread.
///
/// Called by the handler after EVM execution completes (or on error).
pub fn clear_eip8130_tx_context() {
    EIP8130_TX_CONTEXT.with(|c| *c.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_clear_roundtrip() {
        let ctx = Eip8130TxContext {
            sender: Address::repeat_byte(0xAA),
            payer: Address::repeat_byte(0xBB),
            owner_id: B256::repeat_byte(0xCC),
            gas_limit: 100_000,
            max_cost: U256::from(1_000_000u64),
            call_phases: vec![vec![(Address::repeat_byte(0x01), Bytes::from(vec![0x42]))]],
        };

        set_eip8130_tx_context(ctx.clone());
        let retrieved = get_eip8130_tx_context().expect("context should be set");
        assert_eq!(retrieved.sender, Address::repeat_byte(0xAA));
        assert_eq!(retrieved.payer, Address::repeat_byte(0xBB));
        assert_eq!(retrieved.gas_limit, 100_000);

        clear_eip8130_tx_context();
        assert!(get_eip8130_tx_context().is_none());
    }

    #[test]
    fn to_values_conversion() {
        let ctx = Eip8130TxContext {
            sender: Address::repeat_byte(0xAA),
            payer: Address::repeat_byte(0xBB),
            owner_id: B256::repeat_byte(0xCC),
            gas_limit: 42_000,
            max_cost: U256::from(500_000u64),
            call_phases: Vec::new(),
        };

        let values = ctx.to_values();
        assert_eq!(values.sender, ctx.sender);
        assert_eq!(values.payer, ctx.payer);
        assert_eq!(values.owner_id, ctx.owner_id);
        assert_eq!(values.gas_limit, ctx.gas_limit);
        assert_eq!(values.max_cost, ctx.max_cost);
    }

    #[test]
    fn default_is_none() {
        // Clear in case prior tests left state.
        clear_eip8130_tx_context();
        assert!(get_eip8130_tx_context().is_none());
    }
}
