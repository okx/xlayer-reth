//! Pure call-frame observation types shared by the observation producers and the screening
//! matcher.
//!
//! These types are deliberately owned by this crate and contain **plain primitives only** —
//! `Address`, `B256` and small enums. The crate stays free of any EVM / frame-lifecycle
//! dependency, exactly like the native-transfer capture seam: producers live in the block
//! builder (and, in future, in the audit RPC), both of which already depend on this crate, so
//! one shared type can reach `ScreenInput` with no dependency cycle.
//!
//! A producer observes only frames whose address appears in the active rule snapshot's selector
//! set, and it reports what *actually executed*: the runtime code hash of the bytecode the EVM
//! really loaded and ran for that frame. Values are never derived from calldata, never read from
//! pre-execution storage, and never taken from an implementation pointer.

use alloy_primitives::{Address, B256};

/// How a call frame related to the selected address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallObservationKind {
    /// The selected address executed its own code.
    Call,
    /// The selected address's own code performed a first-level `DELEGATECALL`.
    DirectDelegatecall,
}

impl CallObservationKind {
    /// The lowercase wire token bound to `<name>.callType`.
    pub fn as_str(self) -> &'static str {
        match self {
            CallObservationKind::Call => "call",
            CallObservationKind::DirectDelegatecall => "direct_delegatecall",
        }
    }
}

/// One observed call frame involving a selected address.
///
/// `impl_address` / `impl_code_hash` are `Some` **only** for
/// [`CallObservationKind::DirectDelegatecall`] and are always `Some` together: they describe the
/// direct first-level delegatecall target of the selected address's own code. A frame reached
/// through an implementation's own internal delegatecall is not the selected address's direct
/// delegatecall and is never reported here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallObservation {
    pub kind: CallObservationKind,
    /// The selected address (the observed frame's execution identity).
    pub address: Address,
    /// Runtime code hash of the bytecode that actually executed for the selected address.
    pub code_hash: B256,
    /// Direct first-level delegatecall target, if any.
    pub impl_address: Option<Address>,
    /// Runtime code hash of the bytecode that actually executed for `impl_address`.
    pub impl_code_hash: Option<B256>,
}

impl CallObservation {
    /// A `call` observation: the selected address ran its own code.
    pub fn call(address: Address, code_hash: B256) -> Self {
        Self {
            kind: CallObservationKind::Call,
            address,
            code_hash,
            impl_address: None,
            impl_code_hash: None,
        }
    }

    /// A `direct_delegatecall` observation for the selected address.
    pub fn direct_delegatecall(
        address: Address,
        code_hash: B256,
        impl_address: Address,
        impl_code_hash: B256,
    ) -> Self {
        Self {
            kind: CallObservationKind::DirectDelegatecall,
            address,
            code_hash,
            impl_address: Some(impl_address),
            impl_code_hash: Some(impl_code_hash),
        }
    }
}

/// Identity of the rule snapshot whose selector set configured a capture window.
///
/// This is the **full** rule identity (`protocol_version` + `content_version`) plus a digest of
/// the selector address set. The digest alone is never sufficient: two snapshots can name the
/// same addresses while differing in the conditions evaluated against them, so equality requires
/// every field. An observation set whose stamp does not equal the evaluating snapshot's stamp
/// cannot be adjudicated and fails closed — it must never be reduced to "nothing matched".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotStamp {
    pub protocol_version: u32,
    pub content_version: u64,
    /// `keccak256` over the sorted, deduplicated selector addresses of the snapshot.
    pub selector_digest: B256,
}

/// Why an observation set cannot be adjudicated.
///
/// A fixed, low-cardinality enum used for metric labels and bounded diagnostics — never a dynamic
/// string, and never carrying observation content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationUnavailable {
    /// The execution path ran no observation capture at all, so nothing is known about its call
    /// frames. This is **not** the same as "capture ran and saw nothing".
    NotCaptured,
    /// Capture ran but could not complete (an invariant failed), so the set may be missing frames.
    Incomplete,
    /// Capture exceeded its per-transaction observation budget, so the set is truncated.
    BudgetExceeded,
    /// The observation set was produced under a different rule snapshot than the one evaluating
    /// it, so an absent frame cannot be distinguished from an unobserved one.
    SnapshotMismatch,
}

impl ObservationUnavailable {
    /// Stable lowercase token for bounded diagnostics and metric labels.
    pub fn as_str(self) -> &'static str {
        match self {
            ObservationUnavailable::NotCaptured => "not_captured",
            ObservationUnavailable::Incomplete => "incomplete",
            ObservationUnavailable::BudgetExceeded => "budget_exceeded",
            ObservationUnavailable::SnapshotMismatch => "snapshot_mismatch",
        }
    }
}

/// Call-frame observations handed to screening — a **tri-state**, not a possibly-empty list.
///
/// `Available { items: [] }` means capture ran and genuinely observed nothing, which is the
/// ordinary no-match case and binds `matched = false`. `Unavailable` means the engine cannot
/// adjudicate a rule that declares callstack selectors, and such a rule must never evaluate to
/// allow. Collapsing the two would turn "not captured" into "captured, nothing matched" and could
/// release a transaction a rule was written to reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observations<'a> {
    Available { stamp: SnapshotStamp, items: &'a [CallObservation] },
    Unavailable { reason: ObservationUnavailable },
}

impl<'a> Observations<'a> {
    /// The conventional value for a path that performs no capture.
    pub const NOT_CAPTURED: Observations<'static> =
        Observations::Unavailable { reason: ObservationUnavailable::NotCaptured };

    /// Builds an available set for `stamp`.
    pub fn available(stamp: SnapshotStamp, items: &'a [CallObservation]) -> Self {
        Observations::Available { stamp, items }
    }
}

#[cfg(test)]
mod tests {
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
        let obs =
            CallObservation::direct_delegatecall(addr(0x11), hash(0xaa), addr(0x22), hash(0xbb));
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
}
