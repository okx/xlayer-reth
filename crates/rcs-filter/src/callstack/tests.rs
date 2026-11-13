use super::*;

fn addr(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

#[test]
fn call_observation_has_null_impl_fields() {
    let obs = CallObservation::call(addr(0x11), hash(0xaa));
    assert_eq!(obs.kind, CallObservationKind::Call);
    assert_eq!(obs.kind.as_str(), "call");
    assert_eq!(obs.address, addr(0x11));
    assert_eq!(obs.code_hash, hash(0xaa));
    assert!(obs.impl_address.is_none());
    assert!(obs.impl_code_hash.is_none());
}

#[test]
fn direct_delegatecall_observation_carries_both_impl_fields() {
    let obs = CallObservation::direct_delegatecall(addr(0x11), hash(0xaa), addr(0x22), hash(0xbb));
    assert_eq!(obs.kind, CallObservationKind::DirectDelegatecall);
    assert_eq!(obs.kind.as_str(), "direct_delegatecall");
    assert_eq!(obs.impl_address, Some(addr(0x22)));
    assert_eq!(obs.impl_code_hash, Some(hash(0xbb)));
}

#[test]
fn stamp_equality_requires_every_field() {
    let base =
        SnapshotStamp { protocol_version: 2, content_version: 7, selector_digest: hash(0x01) };
    assert_eq!(base, base);
    assert_ne!(base, SnapshotStamp { protocol_version: 1, ..base });
    // Same selector addresses, different rule content: still a different snapshot.
    assert_ne!(base, SnapshotStamp { content_version: 8, ..base });
    assert_ne!(base, SnapshotStamp { selector_digest: hash(0x02), ..base });
}

#[test]
fn not_captured_is_unavailable_not_empty() {
    let observations = Observations::NOT_CAPTURED;
    match observations {
        Observations::Unavailable { reason } => {
            assert_eq!(reason, ObservationUnavailable::NotCaptured);
            assert_eq!(reason.as_str(), "not_captured");
        }
        Observations::Available { .. } => panic!("NOT_CAPTURED must not be Available"),
    }
}

#[test]
fn available_with_no_items_is_a_genuine_no_match() {
    let stamp =
        SnapshotStamp { protocol_version: 2, content_version: 1, selector_digest: hash(0x01) };
    match Observations::available(stamp, &[]) {
        Observations::Available { stamp: got, items } => {
            assert_eq!(got, stamp);
            assert!(items.is_empty());
        }
        Observations::Unavailable { .. } => panic!("an empty capture result is Available"),
    }
}

#[test]
fn unavailable_reasons_have_distinct_tokens() {
    let tokens: Vec<&str> = [
        ObservationUnavailable::NotCaptured,
        ObservationUnavailable::Incomplete,
        ObservationUnavailable::BudgetExceeded,
        ObservationUnavailable::SnapshotMismatch,
    ]
    .iter()
    .map(|reason| reason.as_str())
    .collect();
    let unique: std::collections::BTreeSet<&str> = tokens.iter().copied().collect();
    assert_eq!(tokens.len(), unique.len(), "every reason needs its own operational token");
}
