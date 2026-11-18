//! RCS-produced golden vectors for the synthetic-ban derivation, shared by `rcs-filter`'s own
//! tests and by `xlayer-rpc`'s request-path tests. Compiled only with `test-utils`.

use alloy_primitives::B256;

/// Vectors produced by RCS's own derivation code; see the file's `provenance` object.
pub const GOLDEN: &str = include_str!("../../tests/data/synthetic-ban-golden-vectors.json");

pub struct GoldenBan {
    pub n: usize,
    pub elements: Vec<B256>,
    pub data: String,
    pub source_hash: String,
}

pub fn golden_bans() -> Vec<GoldenBan> {
    let doc: serde_json::Value = serde_json::from_str(GOLDEN).expect("golden vectors are JSON");
    doc["synthetic_bans"]
        .as_array()
        .expect("synthetic_bans is an array")
        .iter()
        .map(|ban| GoldenBan {
            n: ban["n"].as_u64().expect("n") as usize,
            elements: ban["elements"]
                .as_array()
                .expect("elements")
                .iter()
                .map(|e| e.as_str().expect("element").parse().expect("element is a hash"))
                .collect(),
            data: ban["data"].as_str().expect("data").to_string(),
            source_hash: ban["source_hash"].as_str().expect("source_hash").to_string(),
        })
        .collect()
}

/// The L1-log-derived `source_hash` of a user deposit, as RCS computes it from a decoded log.
pub fn golden_l1_user_deposit_source_hash() -> String {
    let doc: serde_json::Value = serde_json::from_str(GOLDEN).expect("golden vectors are JSON");
    doc["l1_user_deposit"]["source_hash"].as_str().expect("source_hash").to_string()
}
