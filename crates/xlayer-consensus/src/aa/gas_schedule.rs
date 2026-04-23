//! Fork-bound intrinsic-gas schedule for XLayerAA transactions.
//!
//! Each fork that revises AA pricing gets its own `pub const` schedule;
//! [`XLayerAAGasSchedule::for_spec`] resolves the schedule active at a
//! given [`OpSpecId`].
//!
//! Threading the fork through the gas formula (rather than reading
//! module-level `const`s directly) is the idiomatic pattern used by
//! op-revm's spec-gated gas tables and by base-revm's
//! `VerifierGasCosts::BASE_V1` — it means a future `XLayerAAV2` fork
//! that bumps, say, `base_cost` is a single-line change here plus a
//! `match spec` arm, not a codebase-wide `const` churn.
//!
//! Naming: schedule variants mirror the XLayerAA hardfork the schedule
//! activates with (`XLAYER_AA`, later `XLAYER_AA_V2`, …), not the OP
//! fork they happen to ride on — OP-side hardforks (Canyon, Ecotone,
//! Jovian, …) carry no XLayerAA pricing changes.

use op_revm::OpSpecId;

use op_alloy_consensus::eip8130::{
    AA_BASE_COST, BYTECODE_BASE_GAS, BYTECODE_PER_BYTE_GAS, CONFIG_CHANGE_OP_GAS,
    CONFIG_CHANGE_SKIP_GAS, EOA_AUTH_GAS, NONCE_KEY_COLD_GAS, NONCE_KEY_WARM_GAS, SLOAD_GAS,
};

/// Intrinsic-gas prices that [`build_aa_parts`](super::build_aa_parts)
/// charges for an XLayerAA transaction. A schedule is a pure fee table —
/// no hashing of `tx`, no spec branching inside field reads.
#[derive(Debug, Clone, Copy)]
pub struct XLayerAAGasSchedule {
    /// Base intrinsic-gas floor for every AA transaction.
    pub base_cost: u64,
    /// Flat authentication cost when the sender uses EOA ecrecover.
    pub eoa_auth_gas: u64,
    /// Cold-SSTORE gas when the tx is the first one on a `nonce_key`.
    pub nonce_key_cold_gas: u64,
    /// Warm-SSTORE gas for a `nonce_key` already in state.
    pub nonce_key_warm_gas: u64,
    /// Base CREATE2 cost for each account-creation entry that deploys
    /// bytecode.
    pub bytecode_base_gas: u64,
    /// Per-byte cost for deployed bytecode on account-creation entries.
    pub bytecode_per_byte_gas: u64,
    /// SSTORE cost per applied account-change unit (config op / create
    /// entry / initial owner).
    pub config_change_op_gas: u64,
    /// SLOAD-only cost for a skipped config-change entry (wrong chain).
    pub config_change_skip_gas: u64,
    /// SLOAD cost incurred during authorizer-chain resolution.
    pub sload_gas: u64,
    /// Verifier gas charged off the sender's intrinsic budget, per
    /// native verifier kind. Custom verifiers meter on-chain under
    /// `CUSTOM_VERIFIER_GAS_CAP` and are intentionally not in this
    /// schedule.
    pub k1_verifier_gas: u64,
    pub p256_raw_verifier_gas: u64,
    pub p256_webauthn_verifier_gas: u64,
    pub delegate_verifier_gas: u64,
}

impl XLayerAAGasSchedule {
    /// Schedule active at the first XLayerAA hardfork (AA transactions
    /// become valid). Values mirror the crate-level `const`s so pool
    /// validators and RPC fillers can keep quoting the same numbers until
    /// a follow-up fork revises them.
    pub const XLAYER_AA: Self = Self {
        base_cost: AA_BASE_COST,
        eoa_auth_gas: EOA_AUTH_GAS,
        nonce_key_cold_gas: NONCE_KEY_COLD_GAS,
        nonce_key_warm_gas: NONCE_KEY_WARM_GAS,
        bytecode_base_gas: BYTECODE_BASE_GAS,
        bytecode_per_byte_gas: BYTECODE_PER_BYTE_GAS,
        config_change_op_gas: CONFIG_CHANGE_OP_GAS,
        config_change_skip_gas: CONFIG_CHANGE_SKIP_GAS,
        sload_gas: SLOAD_GAS,
        // Verifier gas borrows Base's `VerifierGasCosts::BASE_V1` so
        // Base-tooling estimates line up with ours for the common native
        // verifiers.
        k1_verifier_gas: 6_000,
        p256_raw_verifier_gas: 9_500,
        p256_webauthn_verifier_gas: 15_000,
        delegate_verifier_gas: 3_000,
    };

    /// Returns the schedule active at the given spec.
    ///
    /// Current policy: every spec that sees an AA transaction uses the
    /// initial [`XLAYER_AA`] schedule. When a fee-revision fork lands,
    /// add the new variant and select it here with a `match spec` arm —
    /// the rest of the crate already reads through this resolver.
    pub const fn for_spec(spec: OpSpecId) -> &'static Self {
        let _ = spec;
        &Self::XLAYER_AA
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_spec_returns_xlayer_aa_today() {
        // Until a fee-revision fork ships, every OpSpecId routes to the
        // initial schedule. This test is a guard: if someone adds a new
        // variant and forgets to update `for_spec`, callers would silently
        // charge the wrong price.
        let schedule = XLayerAAGasSchedule::for_spec(OpSpecId::JOVIAN);
        assert_eq!(schedule.base_cost, XLayerAAGasSchedule::XLAYER_AA.base_cost);
        assert_eq!(schedule.eoa_auth_gas, XLayerAAGasSchedule::XLAYER_AA.eoa_auth_gas);
    }
}
