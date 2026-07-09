//! `quota_consistency_hash` canonical encoding + keccak256 (FR-7, TD §4.8, ADR-0003).
//!
//! This is a **Filter-local** mechanism — it never appears in any RCS↔Filter message. The
//! same function is used both at submit time and at the pre-package re-simulation so the two
//! hashes are directly comparable (R-5: no encoding drift). It hashes a **canonical
//! structured encoding** of `actions.quota`, never a JSON string, so JSON surface
//! differences (key order / whitespace) do not change the hash (FR-7 AC3).

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, B256};

use crate::client::ActionItem;

/// The audit type covered by the consistency hash (only `actions.quota`, TD §4.8).
const QUOTA: &str = "quota";

/// Computes the consistency hash over `actions.quota`. Determinism comes from:
/// fixed field order (name → address → params), `BTreeMap`-ordered params, length-prefixed
/// fields (no delimiter ambiguity), addresses as raw 20 bytes (lower-cased hex, no checksum),
/// and decimal amount strings — all shared with the submit-time computation.
pub fn encode_and_hash(actions: &BTreeMap<String, Vec<ActionItem>>) -> B256 {
    let empty = Vec::new();
    let quota = actions.get(QUOTA).unwrap_or(&empty);

    let mut buf = Vec::new();
    push_u32(&mut buf, quota.len() as u32);
    for item in quota {
        push_bytes(&mut buf, item.name.as_bytes());
        push_address(&mut buf, &item.address);
        push_u32(&mut buf, item.params.len() as u32);
        for (key, value) in &item.params {
            push_bytes(&mut buf, key.as_bytes());
            push_bytes(&mut buf, value.to_ascii_lowercase().as_bytes());
        }
    }
    keccak256(&buf)
}

/// Length-prefixed byte field (u32 BE length + bytes).
fn push_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    push_u32(buf, bytes.len() as u32);
    buf.extend_from_slice(bytes);
}

/// A u32 big-endian length prefix.
fn push_u32(buf: &mut Vec<u8>, n: u32) {
    buf.extend_from_slice(&n.to_be_bytes());
}

/// Pushes an address as its raw 20 bytes when parseable (canonical, checksum-insensitive),
/// else falls back to its lower-cased string bytes.
fn push_address(buf: &mut Vec<u8>, address: &str) {
    match address.parse::<Address>() {
        Ok(addr) => {
            push_u32(buf, 20);
            buf.extend_from_slice(addr.as_slice());
        }
        Err(_) => push_bytes(buf, address.to_ascii_lowercase().as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota(value: &str) -> BTreeMap<String, Vec<ActionItem>> {
        let mut params = BTreeMap::new();
        params.insert("from".to_string(), "0x0101010101010101010101010101010101010101".to_string());
        params.insert("to".to_string(), "0x0303030303030303030303030303030303030303".to_string());
        params.insert("value".to_string(), value.to_string());
        let mut m = BTreeMap::new();
        m.insert(
            "quota".to_string(),
            vec![ActionItem {
                name: "transfer".to_string(),
                address: "0x0202020202020202020202020202020202020202".to_string(),
                params,
            }],
        );
        m
    }

    #[test]
    fn identical_content_hashes_equal() {
        assert_eq!(
            encode_and_hash(&quota("1000000000000000000")),
            encode_and_hash(&quota("1000000000000000000"))
        );
    }

    #[test]
    fn param_insertion_order_hashes_equal() {
        // Same semantic content, params inserted in different order → canonical (BTreeMap)
        // ordering makes the hash identical (FR-7 AC3: not a JSON-string hash).
        let mut a_params = BTreeMap::new();
        a_params
            .insert("from".to_string(), "0x0101010101010101010101010101010101010101".to_string());
        a_params.insert("value".to_string(), "1000000000000000000".to_string());
        a_params.insert("to".to_string(), "0x0303030303030303030303030303030303030303".to_string());
        let mut a = BTreeMap::new();
        a.insert(
            "quota".to_string(),
            vec![ActionItem {
                name: "transfer".to_string(),
                address: "0x0202020202020202020202020202020202020202".to_string(),
                params: a_params,
            }],
        );
        assert_eq!(encode_and_hash(&a), encode_and_hash(&quota("1000000000000000000")));
    }

    #[test]
    fn different_value_hashes_differ() {
        assert_ne!(
            encode_and_hash(&quota("1000000000000000000")),
            encode_and_hash(&quota("2000000000000000000"))
        );
    }
}
