//! FR-6 — `chain_id → mirror contract address` dispatch (no CLI switch).
//!
//! The feature is enabled iff the running chain id is in the hardcoded map AND the
//! mirror address has on-chain code. Changing the enabled set or an address requires a
//! code upgrade + network-wide release. The chain id is obtained from the existing
//! chainspec via `chain_spec.chain().id()` — no new CLI parameter is introduced.

use alloy_primitives::{address, Address};

/// X Layer devnet chain id. [Code:crates/chainspec/src/xlayer_devnet.rs:70]
pub const XLAYER_DEVNET_CHAIN_ID: u64 = 195;
/// X Layer testnet chain id. [Code:crates/chainspec/src/xlayer_testnet.rs:29]
pub const XLAYER_TESTNET_CHAIN_ID: u64 = 1952;
/// X Layer mainnet chain id. [Code:crates/chainspec/src/xlayer_mainnet.rs:29]
pub const XLAYER_MAINNET_CHAIN_ID: u64 = 196;

/// Devnet (195) mirror address — deterministic, fixed.
/// Checksummed form: `0x73511669fd4dE447feD18BB79bAFeAC93aB7F31f`. [TD §4.6]
pub const DEVNET_MIRROR_ADDRESS: Address = address!("73511669fd4de447fed18bb79bafeac93ab7f31f");

/// Testnet (1952) mirror address — PLACEHOLDER (byte-identical with op-geth).
// TODO(XLOP-1071): replace placeholder before testnet enablement (Blocker 2).
// Must stay byte-identical with op-geth `params.XLayerBlacklistMirror[1952]`
// = HexToAddress("0x..626C61636B6C6973741952"), i.e. ASCII "blacklist" + 0x1952,
// left-zero-padded to 20 bytes. During the placeholder period the mirror has no
// on-chain code, so the snapshot read fails open to an empty list (effectively
// disabled — see [`crate::snapshot`]); both clients behave identically (no-op) AND
// now resolve the same address, so a future lockstep swap to the real address is safe.
pub const TESTNET_MIRROR_ADDRESS: Address = address!("000000000000000000626c61636b6c6973741952");

/// Mainnet (196) mirror address — PLACEHOLDER (byte-identical with op-geth).
// TODO(XLOP-1071): replace placeholder before mainnet enablement (Blocker 2).
// Must stay byte-identical with op-geth `params.XLayerBlacklistMirror[196]`
// = HexToAddress("0x..626C61636B6C6973740196"), i.e. ASCII "blacklist" + 0x0196,
// left-zero-padded to 20 bytes.
pub const MAINNET_MIRROR_ADDRESS: Address = address!("000000000000000000626c61636b6c6973740196");

/// Resolve the mirror contract address for `chain_id`.
///
/// Returns `Some(addr)` for the three X Layer networks (testnet/mainnet currently
/// resolve to placeholder addresses that have no on-chain code → fail-open no-op until
/// Blocker 2 closes), and `None` for every other chain id (full no-op). [TD §4.6, DM-6.x]
pub fn mirror_address_for_chain(chain_id: u64) -> Option<Address> {
    match chain_id {
        XLAYER_DEVNET_CHAIN_ID => Some(DEVNET_MIRROR_ADDRESS),
        XLAYER_TESTNET_CHAIN_ID => Some(TESTNET_MIRROR_ADDRESS),
        XLAYER_MAINNET_CHAIN_ID => Some(MAINNET_MIRROR_ADDRESS),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devnet_resolves_to_fixed_address() {
        // DM-6.1
        assert_eq!(mirror_address_for_chain(XLAYER_DEVNET_CHAIN_ID), Some(DEVNET_MIRROR_ADDRESS));
        // Last byte sanity of the fixed devnet address (0x..f31f).
        let bytes = DEVNET_MIRROR_ADDRESS.into_array();
        assert_eq!(bytes[0], 0x73);
        assert_eq!(bytes[19], 0x1f);
    }

    #[test]
    fn testnet_and_mainnet_resolve_to_placeholder_some() {
        // DM-6.2 — placeholder period returns Some(placeholder); no code on-chain → no-op.
        assert_eq!(mirror_address_for_chain(XLAYER_TESTNET_CHAIN_ID), Some(TESTNET_MIRROR_ADDRESS));
        assert_eq!(mirror_address_for_chain(XLAYER_MAINNET_CHAIN_ID), Some(MAINNET_MIRROR_ADDRESS));
    }

    #[test]
    fn placeholders_are_byte_identical_with_op_geth() {
        // Cross-client pin: op-geth params.XLayerBlacklistMirror = HexToAddress of
        // "0x..626C61636B6C6973741952" / "..0196" → right-most 20 bytes = ASCII
        // "blacklist" + 0x1952 / 0x0196, left-zero-padded. Diverging from these bytes
        // (e.g. resetting to ZERO, or swapping only one client) risks a future fork.
        assert_eq!(TESTNET_MIRROR_ADDRESS, address!("000000000000000000626c61636b6c6973741952"));
        assert_eq!(MAINNET_MIRROR_ADDRESS, address!("000000000000000000626c61636b6c6973740196"));
        // "blacklist" ASCII anchor (62=b,6c=l,61=a,63=c,6b=k,6c=l,69=i,73=s,74=t).
        assert_eq!(&TESTNET_MIRROR_ADDRESS.into_array()[9..18], b"blacklist");
        assert_eq!(&MAINNET_MIRROR_ADDRESS.into_array()[9..18], b"blacklist");
    }

    #[test]
    fn unrecognized_chain_resolves_to_none() {
        // DM-6.3 — e.g. OP mainnet (10), Ethereum (1).
        assert_eq!(mirror_address_for_chain(10), None);
        assert_eq!(mirror_address_for_chain(1), None);
        assert_eq!(mirror_address_for_chain(0), None);
    }
}
