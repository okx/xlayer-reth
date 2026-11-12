//! Recognises the synthetic blacklist-replay deposits RCS prepends to a Force Tx audit request.
//!
//! This is a **content binding**, not source authentication. A synthetic ban's `source_hash` is a
//! domain-separated hash of exactly the `bytes32[]` its calldata carries, so the check below proves
//! that an entry's bytes match that derivation and nothing more. The domain string and the
//! encoding are public: whoever authors an `xlayer_auditTransactions` request can build a matching
//! entry for calldata of its choice. Whether a `source_hash` is genuine — an RCS-built ban, or the
//! L1-log derivation of a real user deposit — is a property of the caller that built the request,
//! which this RPC cannot verify.
//!
//! The derivation is a cross-component contract with RCS (`internal/forcetx/signer.go` @
//! `8944b24f00c058cb5e65090a5c210335d1f186ed`). Changing any constant here without RCS changing in
//! lockstep turns every real ban prefix into an approved original.

use alloy_primitives::{keccak256, Keccak256, B256};

/// Domain separator RCS prepends to the ban batch before hashing it into the synthetic
/// deposit's `source_hash` (`signer.go:27`, `syntheticBlacklistSourceHashDomain`, hashed by
/// `syntheticBlacklistSourceHash` at `signer.go:426`).
pub const SYNTHETIC_BAN_SOURCE_HASH_DOMAIN: &[u8] = b"xlayer-rcs:blacklist-simulation:v1";

/// The single function a synthetic ban calls. Its selector is computed from this signature rather
/// than hard-coded, as RCS does (`signer.go:76`, `batchAddToBlacklistSelector`).
pub const BATCH_ADD_TO_BLACKLIST_SIGNATURE: &str = "batchAddToBlacklist(bytes32[])";

/// Upper bound of one ban batch, equal to the contract's `MAX_BATCH_SIZE` (`signer.go:22`,
/// `maxBlacklistBatchSize`).
pub const MAX_SYNTHETIC_BAN_BATCH: usize = 500;

/// Selector (4) ‖ offset word (32) ‖ length word (32): the fixed head of the canonical encoding
/// (`signer.go:83`, `encodeBatchAddToBlacklistCalldata`).
const HEAD_LEN: usize = 4 + 32 + 32;

/// Whether a built deposit's own `source_hash` and `input` are a synthetic ban in RCS's v1 shape.
///
/// Called with the bytes of the deposit transaction `build_deposit_tx` produced and the batch
/// executes, never with request strings, so the bytes checked are the bytes the EVM runs and two
/// spellings of one request decode to one answer.
///
/// `input` must be the canonical ABI encoding of `batchAddToBlacklist(bytes32[])` with
/// `1 ..= MAX_SYNTHETIC_BAN_BATCH` elements: the computed selector, an offset word of exactly 32,
/// a length word `n` whose high bytes are all zero, and exactly `68 + 32·n` bytes in total. Any
/// other layout returns `false`. `source_hash` must then equal
/// `keccak256(SYNTHETIC_BAN_SOURCE_HASH_DOMAIN ‖ e₁ ‖ … ‖ eₙ)` over the elements in order.
///
/// `from`, `to`, `mint`, `value`, `gas_limit` and `is_system_transaction` are deliberately not
/// checked: the `source_hash` binding is what identifies the entry, and the operator and contract
/// addresses belong to RCS's deployment configuration, which this node does not hold.
///
/// A `true` result never authenticates the caller or the entry; see the module documentation.
/// Bounded (at most 500 elements, one linear hash) and allocation-free.
pub fn verify_synthetic_ban(source_hash: B256, input: &[u8]) -> bool {
    if input.len() < HEAD_LEN {
        return false;
    }
    let selector = keccak256(BATCH_ADD_TO_BLACKLIST_SIGNATURE.as_bytes());
    if input[..4] != selector[..4] {
        return false;
    }
    let Some(offset) = small_word(&input[4..36]) else { return false };
    if offset != 32 {
        return false;
    }
    let Some(n) = small_word(&input[36..HEAD_LEN]) else { return false };
    if n == 0 || n > MAX_SYNTHETIC_BAN_BATCH {
        return false;
    }
    // `n <= 500`, so this cannot overflow.
    if input.len() != HEAD_LEN + 32 * n {
        return false;
    }
    let mut hasher = Keccak256::new();
    hasher.update(SYNTHETIC_BAN_SOURCE_HASH_DOMAIN);
    hasher.update(&input[HEAD_LEN..]);
    hasher.finalize() == source_hash
}

/// The value of a 32-byte big-endian word that fits in two bytes, or `None` when any higher byte
/// is set. Both words checked here are at most 500, so a wider value is never canonical.
fn small_word(word: &[u8]) -> Option<usize> {
    let (high, low) = word.split_at(30);
    high.iter().all(|byte| *byte == 0).then(|| usize::from(u16::from_be_bytes([low[0], low[1]])))
}

#[path = "../../tests/audit__synthetic_ban__tests.rs"]
#[cfg(test)]
pub(crate) mod tests;
