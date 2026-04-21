//! EIP-8130 wire-level constants.
//!
//! Gas constants here are the *wire* view — values that the intrinsic-gas
//! formula in a later milestone will feed from. They live in this crate (not
//! in `xlayer-revm`) so the consensus path can compute an AA tx's declared
//! gas obligations without pulling in revm.

/// The EIP-2718 transaction type byte for XLayerAA transactions.
pub const AA_TX_TYPE_ID: u8 = 0x7B;

/// Payer signature domain-separator byte. Prepended to the payer-signing
/// preimage so a valid sender signature cannot be replayed as a payer
/// signature on the same transaction body.
pub const AA_PAYER_TYPE: u8 = 0x7C;

/// Base intrinsic gas for an AA transaction. Replaces the 21_000 floor that
/// legacy Ethereum txs use — the additional work the protocol performs
/// (nonce-manager SSTORE, pre-writes, log emission) is metered via the other
/// sub-components below.
pub const AA_BASE_COST: u64 = 15_000;

/// Size of the EVM deployment header prepended to user bytecode when a
/// create entry deploys through `CREATE2`. The header wraps the runtime code
/// in a `RETURN`-shaped constructor.
pub const DEPLOYMENT_HEADER_SIZE: usize = 14;

/// Upper bound on `sender_auth` / `payer_auth` blob size; bounds the DoS
/// surface for auth-blob parsing and signature verification.
pub const MAX_SIGNATURE_SIZE: usize = 2048;

// ---------------------------------------------------------------------------
// Intrinsic gas sub-components
// ---------------------------------------------------------------------------

/// Cold-SSTORE gas charged when the tx is the first one to use a given
/// `nonce_key` channel.
pub const NONCE_KEY_COLD_GAS: u64 = 22_100;

/// Warm-SSTORE gas for a `nonce_key` channel that already exists in state.
pub const NONCE_KEY_WARM_GAS: u64 = 5_000;

/// Base CREATE2 cost for each create entry that actually deploys bytecode.
pub const BYTECODE_BASE_GAS: u64 = 32_000;

/// Per-byte cost for deployed bytecode.
pub const BYTECODE_PER_BYTE_GAS: u64 = 200;

/// SSTORE cost for each applied account-change unit. Account-change units
/// are: config operations on the matching chain, create entries, and initial
/// owners attached to a create entry.
pub const CONFIG_CHANGE_OP_GAS: u64 = 20_000;

/// SLOAD-only cost for a skipped config-change entry (wrong chain id). The
/// entry is still read to advance the sequence cursor.
pub const CONFIG_CHANGE_SKIP_GAS: u64 = 2_100;

/// Cost of a single SLOAD during auth resolution.
pub const SLOAD_GAS: u64 = 2_100;

/// Flat gas cost for EOA (ecrecover) authentication.
pub const EOA_AUTH_GAS: u64 = 6_000;

/// Upper bound on the total number of calls (across all phases) in one tx.
/// The per-tx budget bounds the fan-out of phased call execution and caps
/// mempool validation work.
pub const MAX_CALLS_PER_TX: usize = 100;

/// Upper bound on the account-change units in one tx. Unit counting rules:
/// each create entry is 1, each initial owner in a create entry is 1, and
/// each config operation is 1.
pub const MAX_ACCOUNT_CHANGES_PER_TX: usize = 10;

/// Upper bound on the total number of `OwnerChange`s across all
/// `ConfigChangeEntry`s in a single tx. Bounds the owner-change validation
/// surface independently of `MAX_ACCOUNT_CHANGES_PER_TX`.
pub const MAX_CONFIG_OPS_PER_TX: usize = 5;

/// Gas ceiling for a single custom verifier STATICCALL. Custom verifiers
/// are metered out of the payer's budget, separate from the sender's
/// execution `gas_limit` — the cap stops a pathological verifier from
/// starving block inclusion.
pub const CUSTOM_VERIFIER_GAS_CAP: u64 = 200_000;

/// Sentinel `nonce_key` value that activates nonce-free mode. In nonce-free
/// mode the protocol does not read or increment `NonceManager`; replay
/// protection relies on `expiry` and a per-hash ring buffer.
pub const NONCE_KEY_MAX: alloy_primitives::U256 = alloy_primitives::U256::MAX;
