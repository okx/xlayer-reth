//! Golden and byte-level vectors for [`super::verify_synthetic_ban`].

use super::*;
use alloy_primitives::hex;

/// Vectors produced by RCS's own derivation code; see the file's `provenance` object.
pub(crate) const GOLDEN: &str = include_str!("../testdata/synthetic-ban-golden-vectors.json");

pub(crate) struct GoldenBan {
    pub(crate) n: usize,
    pub(crate) elements: Vec<B256>,
    pub(crate) data: String,
    pub(crate) source_hash: String,
}

pub(crate) fn golden_bans() -> Vec<GoldenBan> {
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
pub(crate) fn golden_l1_user_deposit_source_hash() -> String {
    let doc: serde_json::Value = serde_json::from_str(GOLDEN).expect("golden vectors are JSON");
    doc["l1_user_deposit"]["source_hash"].as_str().expect("source_hash").to_string()
}

fn bytes(data: &str) -> Vec<u8> {
    hex::decode(data).expect("golden data is hex")
}

fn hash(value: &str) -> B256 {
    value.parse().expect("golden source_hash is a hash")
}

fn one() -> GoldenBan {
    golden_bans().into_iter().find(|ban| ban.n == 1).expect("an n = 1 vector")
}

fn two() -> GoldenBan {
    golden_bans().into_iter().find(|ban| ban.n == 2).expect("an n = 2 vector")
}

#[test]
fn every_golden_vector_produced_by_the_rcs_derivation_verifies() {
    let bans = golden_bans();
    assert_eq!(bans.iter().map(|ban| ban.n).collect::<Vec<_>>(), vec![1, 2, 500]);
    for ban in bans {
        assert_eq!(ban.elements.len(), ban.n);
        assert!(
            verify_synthetic_ban(hash(&ban.source_hash), &bytes(&ban.data)),
            "n = {}: the vector RCS built must verify",
            ban.n
        );
    }
}

#[test]
fn the_golden_calldata_is_the_canonical_encoding_of_its_elements() {
    // An independent restatement of the layout, so a vector whose bytes drifted from its listed
    // elements is caught here rather than trusted.
    for ban in golden_bans() {
        let mut expected = keccak256(BATCH_ADD_TO_BLACKLIST_SIGNATURE.as_bytes())[..4].to_vec();
        expected.extend_from_slice(B256::with_last_byte(32).as_slice());
        expected.extend_from_slice(&alloy_primitives::U256::from(ban.n).to_be_bytes::<32>());
        for element in &ban.elements {
            expected.extend_from_slice(element.as_slice());
        }
        assert_eq!(bytes(&ban.data), expected, "n = {}", ban.n);
    }
}

#[test]
fn a_wrong_domain_selector_or_offset_is_not_a_ban() {
    let ban = two();
    let data = bytes(&ban.data);

    let mut other_domain = Keccak256::new();
    other_domain.update(b"xlayer-rcs:blacklist-simulation:v2");
    other_domain.update(&data[HEAD_LEN..]);
    assert!(!verify_synthetic_ban(other_domain.finalize(), &data), "wrong domain");

    let mut wrong_selector = data.clone();
    wrong_selector[0] ^= 0x01;
    assert!(!verify_synthetic_ban(hash(&ban.source_hash), &wrong_selector), "wrong selector");

    let mut wrong_offset = data.clone();
    wrong_offset[35] = 64;
    assert!(!verify_synthetic_ban(hash(&ban.source_hash), &wrong_offset), "offset 64");
    let mut wide_offset = data;
    wide_offset[4] = 0x01;
    assert!(!verify_synthetic_ban(hash(&ban.source_hash), &wide_offset), "offset high byte");
}

/// Builds a canonical-looking ban over `n` copies of one element, with a matching hash, so only
/// the length bound decides.
fn ban_of_len(n: usize) -> (B256, Vec<u8>) {
    let mut data = keccak256(BATCH_ADD_TO_BLACKLIST_SIGNATURE.as_bytes())[..4].to_vec();
    data.extend_from_slice(B256::with_last_byte(32).as_slice());
    data.extend_from_slice(&alloy_primitives::U256::from(n).to_be_bytes::<32>());
    for _ in 0..n {
        data.extend_from_slice(B256::repeat_byte(0x42).as_slice());
    }
    let mut hasher = Keccak256::new();
    hasher.update(SYNTHETIC_BAN_SOURCE_HASH_DOMAIN);
    hasher.update(&data[HEAD_LEN..]);
    (hasher.finalize(), data)
}

#[test]
fn the_length_word_must_be_between_one_and_five_hundred() {
    let (source_hash, data) = ban_of_len(0);
    assert!(!verify_synthetic_ban(source_hash, &data), "n = 0");
    let (source_hash, data) = ban_of_len(501);
    assert!(!verify_synthetic_ban(source_hash, &data), "n = 501");
    let (source_hash, data) = ban_of_len(500);
    assert!(verify_synthetic_ban(source_hash, &data), "n = 500 is the bound itself");
    let (source_hash, data) = ban_of_len(1);
    assert!(verify_synthetic_ban(source_hash, &data), "n = 1");
}

#[test]
fn a_length_word_with_any_high_byte_set_is_not_a_ban() {
    let ban = one();
    let data = bytes(&ban.data);
    for index in 36..66 {
        let mut wide = data.clone();
        wide[index] = 0x01;
        assert!(!verify_synthetic_ban(hash(&ban.source_hash), &wide), "high byte {index}");
    }
}

#[test]
fn a_trailing_byte_or_a_truncated_element_is_not_a_ban() {
    let ban = two();
    let data = bytes(&ban.data);

    let mut trailing = data.clone();
    trailing.push(0);
    assert!(!verify_synthetic_ban(hash(&ban.source_hash), &trailing), "trailing byte");

    let truncated = &data[..data.len() - 1];
    assert!(!verify_synthetic_ban(hash(&ban.source_hash), truncated), "truncated element");

    assert!(!verify_synthetic_ban(hash(&ban.source_hash), &data[..HEAD_LEN - 1]), "short head");
    assert!(!verify_synthetic_ban(hash(&ban.source_hash), &[]), "empty input");
}

#[test]
fn reordered_elements_are_not_a_ban_for_the_original_hash() {
    let ban = two();
    let data = bytes(&ban.data);
    let mut reordered = data[..HEAD_LEN].to_vec();
    reordered.extend_from_slice(ban.elements[1].as_slice());
    reordered.extend_from_slice(ban.elements[0].as_slice());
    assert_ne!(reordered, data);
    assert!(!verify_synthetic_ban(hash(&ban.source_hash), &reordered));
}

#[test]
fn an_l1_derived_source_hash_never_verifies_for_canonical_ban_calldata() {
    // The same canonical calldata, carried by a deposit whose `source_hash` was derived from an
    // L1 log the way RCS derives every user deposit's: the derivation, not the calldata, decides.
    let ban = one();
    let l1_derived = hash(&golden_l1_user_deposit_source_hash());
    assert!(!verify_synthetic_ban(l1_derived, &bytes(&ban.data)));
    assert!(verify_synthetic_ban(hash(&ban.source_hash), &bytes(&ban.data)), "control");
}
