//! XLayer-specific hardforks.
//!
//! Variants here are for XLayer-exclusive protocol changes that can't
//! be expressed as [`reth_optimism_forks::OpHardfork`] entries —
//! currently just `XLayerAA` (EIP-8130 account-abstraction
//! transactions, type byte `0x7B`). Activation timestamps are threaded
//! through [`crate::XLAYER_MAINNET_HARDFORKS`] et al. as
//! `(XLayerHardfork::XLayerAA.boxed(), ForkCondition::Timestamp(ts))`.
//!
//! Query activation via the [`crate::XLayerHardforks`] extension trait
//! (blanket-impl'd on every `Hardforks`):
//!
//! ```ignore
//! use xlayer_chainspec::XLayerHardforks;
//! let active = chain_spec.is_xlayer_aa_active_at_timestamp(block.timestamp);
//! ```

use alloy_hardforks::hardfork;

hardfork!(
    /// XLayer-specific hardforks — variants the upstream
    /// [`reth_optimism_forks::OpHardfork`] enum can't express.
    XLayerHardfork {
        /// XLayerAA — enables EIP-8130 account-abstraction transactions
        /// (type `0x7B`), activates the seven predeploy contracts under
        /// `contracts/eip8130/`, and wires the `XLayerAAHandler` branches
        /// in `xlayer-revm`.
        XLayerAA,
    }
);

// ── Activation timestamps ─────────────────────────────────────────────

/// Sentinel "fork not yet scheduled" timestamp.
///
/// Used for mainnet and testnet XLayerAA activation until product
/// signs off on real timestamps. `u64::MAX` means the fork never
/// activates in practice (no real block timestamp will reach it), so
/// `is_xlayer_aa_active_at_timestamp(_)` returns `false` on those
/// chains for every real block. Keeping it as a named constant (not
/// repeating `u64::MAX` in each chainspec) makes the "TBD" status
/// explicit in diffs — a future PR that sets the real timestamp only
/// touches one or two lines.
pub const XLAYER_AA_TIMESTAMP_TBD: u64 = u64::MAX;

/// Devnet activates XLayerAA at the earliest-possible non-genesis
/// timestamp. `op-node` emits predeploy upgrade deposit txs at the
/// activation boundary block (`isActive(blockTs) && !isActive(parentTs)`),
/// and that check cannot fire inside genesis — so even a `0` here would
/// leave the 7 AA contracts uninstalled. `1` puts the boundary at the
/// first real L2 block (genesis timestamp stays at `0`), users can
/// submit AA txs from block 2 onward.
pub const XLAYER_DEVNET_XLAYER_AA_TIMESTAMP: u64 = 1;

/// Testnet XLayerAA activation — product sign-off pending.
pub const XLAYER_TESTNET_XLAYER_AA_TIMESTAMP: u64 = XLAYER_AA_TIMESTAMP_TBD;

/// Mainnet XLayerAA activation — product sign-off pending.
pub const XLAYER_MAINNET_XLAYER_AA_TIMESTAMP: u64 = XLAYER_AA_TIMESTAMP_TBD;

/// `xlayer-dev` (the `reth --dev` local chainspec) activates XLayerAA
/// at genesis. Unlike the op-node-backed chains, `--dev` mode has no
/// CL and therefore no upgrade-deposit-tx mechanism; the 7 predeploys
/// are seeded directly into the genesis alloc (see
/// [`crate::XLAYER_DEV`]). Setting `= 0` makes XLayerAA active from
/// block 0 so the AA tx-type validation is on from the first block
/// the auto-miner produces.
pub const XLAYER_DEV_XLAYER_AA_TIMESTAMP: u64 = 0;
