//! XLayer revm extensions.
//!
//! This crate ships the revm-side building blocks required to execute
//! **XLayerAA** transactions (tx type `0x7B`, originally specified as
//! EIP-8130). It is structured as a *decorator* over the upstream
//! `op-revm::OpHandler` rather than a fork, and intentionally stays
//! self-contained in revm primitives — no dependency on the consensus-layer
//! `TxXLayerAA` wire type.
//!
//! ## Modules
//!
//! - [`constants`] — owner scopes, gas caps, tx type byte.
//! - [`policy`]    — pending owner-state overlay and scope checks.
//! - [`transaction`] — pure-data execution parts (`XLayerAAParts`) and
//!   config-log encoders.
//! - [`precompiles`] — NonceManager / TxContext precompile helpers plus the
//!   [`XLayerAAPrecompiles`](precompiles::XLayerAAPrecompiles) decorator.
//! - [`handler`]   — [`XLayerAAHandler`](handler::XLayerAAHandler) decorator
//!   over `OpHandler`.
//!
//! Callers wire this crate in by constructing an `EvmFactory` that uses
//! `XLayerAAHandler` + `XLayerAAPrecompiles`. Non-AA transactions flow
//! unchanged through the underlying `OpHandler` path.
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

#[cfg(not(feature = "std"))]
extern crate alloc as std;

pub mod constants;
pub mod handler;
pub mod policy;
pub mod precompiles;
pub mod transaction;
pub mod tx_env;

pub use tx_env::XLayerAATransaction;

pub use constants::{
    DEFAULT_CUSTOM_VERIFIER_GAS_CAP, DELEGATE_VERIFIER_ADDRESS, MAX_ACCOUNT_CHANGES_PER_TX,
    MAX_CALLS_PER_TX, OWNER_SCOPE_CONFIG, OWNER_SCOPE_PAYER, OWNER_SCOPE_SENDER, XLAYERAA_TX_TYPE,
    custom_verifier_gas_cap, set_custom_verifier_gas_cap,
};
pub use policy::{
    PendingOwnerState, PendingOwnerValidationError, owner_scope_allows,
    pending_owner_state_for_change, validate_pending_owner_state,
};
pub use precompiles::{
    CallTuple, INonceManager, ITxContext, NONCE_BASE_SLOT, NONCE_MANAGER_ADDRESS,
    NONCE_MANAGER_GAS, TX_CONTEXT_ADDRESS, TX_CONTEXT_GAS, XLayerAAPrecompiles, XLayerAATxContext,
    aa_nonce_slot, clear_xlayeraa_tx_context, get_xlayeraa_tx_context, set_xlayeraa_tx_context,
};
pub use transaction::{
    XLayerAAAuthorizerValidation, XLayerAACall, XLayerAACodePlacement, XLayerAAConfigLog,
    XLayerAAConfigOp, XLayerAAParts, XLayerAAPhaseResult, XLayerAASequenceUpdate,
    XLayerAAStorageWrite, XLayerAATxTr, XLayerAAVerifyCall, config_log_to_system_log,
    decode_phase_statuses, encode_phase_statuses, extract_phase_statuses_from_logs,
    phase_statuses_log_topic, phase_statuses_system_log,
};
