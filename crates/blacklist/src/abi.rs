//! FR-4 — `getBlacklist(uint256,uint256)` ABI selector + encode/decode.
//!
//! The node only depends on this single read-only view ABI; it does **not** depend on
//! the mirror contract's storage layout (the contract may evolve freely behind
//! `getBlacklist`). Selector and codec are kept in one place as the cross-client
//! contract source of truth. [TD §4.1, §4.8]

use alloy_primitives::{keccak256, Address, U256};
use std::sync::LazyLock;

/// Canonical function signature used to derive the 4-byte selector.
pub const GET_BLACKLIST_SIGNATURE: &str = "getBlacklist(uint256,uint256)";

/// `keccak256("getBlacklist(uint256,uint256)")[0..4]`.
///
/// Computed once at first use from [`GET_BLACKLIST_SIGNATURE`] so the value is correct
/// by construction (no hand-transcribed bytes to drift). [TD §3.3, §4.1]
pub static GET_BLACKLIST_SELECTOR: LazyLock<[u8; 4]> = LazyLock::new(|| {
    let h = keccak256(GET_BLACKLIST_SIGNATURE.as_bytes());
    [h[0], h[1], h[2], h[3]]
});

/// ABI decode failures. Every variant maps to fail-open (empty snapshot) at the
/// snapshot layer. [TD §4.1 fail-open]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AbiError {
    /// Return data shorter than the minimum static head (2 words).
    #[error("abi return data too short")]
    TooShort,
    /// A length / offset field could not be represented as `usize` on this platform.
    #[error("abi length or offset overflow")]
    Overflow,
    /// The declared array bytes run past the end of the return data.
    #[error("abi array out of bounds")]
    OutOfBounds,
}

/// Encode a `getBlacklist(start, limit)` calldata blob: `selector ++ start ++ limit`.
pub fn encode_get_blacklist(start: U256, limit: U256) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 32 + 32);
    out.extend_from_slice(&*GET_BLACKLIST_SELECTOR);
    out.extend_from_slice(&start.to_be_bytes::<32>());
    out.extend_from_slice(&limit.to_be_bytes::<32>());
    out
}

/// Decode the `(uint256 total, address[] addresses)` return tuple.
///
/// Layout: word0 = `total`; word1 = byte offset to the dynamic `address[]`; at that
/// offset word0 = array length `L` followed by `L` left-padded address words.
/// Zero addresses are returned as-is here (the snapshot layer applies the
/// defensive zero-skip and slot accounting).
pub fn decode_get_blacklist(data: &[u8]) -> Result<(U256, Vec<Address>), AbiError> {
    if data.len() < 64 {
        return Err(AbiError::TooShort);
    }
    let total = U256::from_be_slice(&data[0..32]);
    let offset = U256::from_be_slice(&data[32..64]);
    let offset = usize::try_from(offset).map_err(|_| AbiError::Overflow)?;

    let len_end = offset.checked_add(32).ok_or(AbiError::Overflow)?;
    if data.len() < len_end {
        return Err(AbiError::OutOfBounds);
    }
    let len = U256::from_be_slice(&data[offset..len_end]);
    let len = usize::try_from(len).map_err(|_| AbiError::Overflow)?;

    let arr_start = len_end;
    let arr_bytes = len.checked_mul(32).ok_or(AbiError::Overflow)?;
    let arr_end = arr_start.checked_add(arr_bytes).ok_or(AbiError::Overflow)?;
    if data.len() < arr_end {
        return Err(AbiError::OutOfBounds);
    }

    let mut addrs = Vec::with_capacity(len);
    for i in 0..len {
        let word = &data[arr_start + i * 32..arr_start + i * 32 + 32];
        // Address is right-aligned in the 32-byte word: take the low 20 bytes.
        addrs.push(Address::from_slice(&word[12..32]));
    }
    Ok((total, addrs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn selector_matches_keccak_of_signature() {
        let h = keccak256(GET_BLACKLIST_SIGNATURE.as_bytes());
        assert_eq!(*GET_BLACKLIST_SELECTOR, [h[0], h[1], h[2], h[3]]);
    }

    /// Build an ABI return blob for `(total, addresses)` with the array at offset 0x40.
    fn encode_return(total: u64, addrs: &[Address]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&U256::from(total).to_be_bytes::<32>());
        out.extend_from_slice(&U256::from(64u64).to_be_bytes::<32>()); // offset = 0x40
        out.extend_from_slice(&U256::from(addrs.len() as u64).to_be_bytes::<32>());
        for a in addrs {
            let mut word = [0u8; 32];
            word[12..32].copy_from_slice(a.as_slice());
            out.extend_from_slice(&word);
        }
        out
    }

    #[test]
    fn encode_call_is_selector_plus_two_words() {
        let cd = encode_get_blacklist(U256::from(0u64), U256::from(1024u64));
        assert_eq!(cd.len(), 4 + 32 + 32);
        assert_eq!(&cd[0..4], &*GET_BLACKLIST_SELECTOR);
        assert_eq!(U256::from_be_slice(&cd[4..36]), U256::ZERO);
        assert_eq!(U256::from_be_slice(&cd[36..68]), U256::from(1024u64));
    }

    #[test]
    fn decode_roundtrip_single_page() {
        let a = address!("00000000000000000000000000000000000000aa");
        let b = address!("00000000000000000000000000000000000000bb");
        let blob = encode_return(2, &[a, b]);
        let (total, addrs) = decode_get_blacklist(&blob).expect("decode");
        assert_eq!(total, U256::from(2u64));
        assert_eq!(addrs, vec![a, b]);
    }

    #[test]
    fn decode_empty_array() {
        let blob = encode_return(0, &[]);
        let (total, addrs) = decode_get_blacklist(&blob).expect("decode");
        assert_eq!(total, U256::ZERO);
        assert!(addrs.is_empty());
    }

    #[test]
    fn decode_rejects_short_data() {
        assert_eq!(decode_get_blacklist(&[0u8; 10]), Err(AbiError::TooShort));
    }

    #[test]
    fn decode_rejects_out_of_bounds_array() {
        // Claims 5 addresses but provides none.
        let mut blob = Vec::new();
        blob.extend_from_slice(&U256::from(5u64).to_be_bytes::<32>());
        blob.extend_from_slice(&U256::from(64u64).to_be_bytes::<32>());
        blob.extend_from_slice(&U256::from(5u64).to_be_bytes::<32>());
        assert_eq!(decode_get_blacklist(&blob), Err(AbiError::OutOfBounds));
    }
}
