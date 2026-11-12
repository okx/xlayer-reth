//! `quota_consistency_hash` canonical encoding and keccak256 hashing.
//!
//! This is a **Filter-local** mechanism — it never appears in any RCS↔Filter message. The
//! same function is used both at submit time and at the pre-package re-simulation so the two
//! hashes are directly comparable without encoding drift. It hashes a **canonical
//! structured encoding** of `actions.quota`, never a JSON string, so JSON surface
//! differences (key order / whitespace) do not change the hash.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, B256};

use crate::client::ActionItem;

/// The audit type covered by the consistency hash (only `actions.quota`).
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
            push_json_value(&mut buf, value);
        }
    }
    keccak256(&buf)
}

/// Canonical encoding for ABI-decoded JSON values. Type tags distinguish arrays from scalar
/// values; arrays are length-prefixed recursively and retain element order from the event log.
fn push_json_value(buf: &mut Vec<u8>, value: &serde_json::Value) {
    match value {
        serde_json::Value::String(value) => {
            buf.push(0);
            push_bytes(buf, value.to_ascii_lowercase().as_bytes())
        }
        serde_json::Value::Array(values) => {
            buf.push(1);
            push_u32(buf, values.len() as u32);
            for value in values {
                push_json_value(buf, value);
            }
        }
        // ABI decoding currently emits strings and arrays only. This fallback remains
        // deterministic for future JSON scalar extensions.
        value => {
            buf.push(2);
            push_bytes(buf, value.to_string().to_ascii_lowercase().as_bytes())
        }
    }
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

#[path = "../tests/quota_hash__tests.rs"]
#[cfg(test)]
mod tests;
