//! Flashblock access list type wrapping EIP-7928 `AccountChanges`.

use alloy_eip7928::{compute_block_access_list_hash, AccountChanges};
use alloy_primitives::B256;

/// A flashblock access list containing all state reads and writes for a range of transactions.
///
/// Wraps the EIP-7928 `AccountChanges` vector with transaction index bounds and a commitment
/// hash for integrity verification.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct FlashblockAccessList {
    /// Per-account state changes (storage, balance, nonce, code) indexed by tx position.
    pub account_changes: Vec<AccountChanges>,
    /// Lowest transaction index (inclusive) in the full block covered by this access list.
    pub min_tx_index: u64,
    /// Highest transaction index (exclusive) in the full block covered by this access list.
    pub max_tx_index: u64,
    /// `keccak256(rlp_encode(account_changes))` — commitment hash for the access list.
    pub fal_hash: B256,
}

impl FlashblockAccessList {
    /// Constructs a new `FlashblockAccessList` from the given account changes and tx index range.
    ///
    /// Computes the `fal_hash` as `keccak256(rlp_encode(account_changes))`.
    pub fn build(
        account_changes: Vec<AccountChanges>,
        min_tx_index: u64,
        max_tx_index: u64,
    ) -> Self {
        let fal_hash = compute_block_access_list_hash(&account_changes);
        Self { account_changes, min_tx_index, max_tx_index, fal_hash }
    }
}
