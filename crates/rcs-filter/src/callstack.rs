//! Execution-frame observations for a selected contract's own direct `DELEGATECALL`.
//!
//! This module holds **plain primitives only** so the screening crate gains no EVM or
//! frame-lifecycle dependency: the frame logic that produces these values lives in the
//! capture crate, and only the finished observations cross into screening.
//!
//! Every field here comes from a real execution frame. Nothing is derived from a proxy storage
//! slot, from calldata, or from an account's `code_hash` — an account field can name a
//! delegation designator rather than the bytecode that actually ran.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use alloy_primitives::{keccak256, Address, B256};

/// Maximum aliases one rule may declare in `direct_delegatecalls`. Matches
/// [`crate::rules::MAX_EVENTS_PER_RULE`] and bounds the per-rule candidate product the
/// combined binding budget accounts for.
pub const MAX_DIRECT_DELEGATECALL_ALIASES: usize = 8;

/// One executed direct `DELEGATECALL` of a selected address. Every field comes from a real
/// execution frame; none is derived from storage or calldata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectDelegatecallObservation {
    /// Execution context address that issued the `DELEGATECALL` (the parent frame's
    /// `target_address`).
    pub context_address: Address,
    /// keccak256 of the runtime bytecode the **parent** frame actually loaded and executed.
    pub context_code_hash: B256,
    /// Direct `DELEGATECALL` target (the new frame's `bytecode_address`).
    pub delegate_address: Address,
    /// keccak256 of the runtime bytecode the **new** frame actually loaded and executed.
    pub delegate_code_hash: B256,
}

/// Identity of the snapshot whose alias set configured a capture window. The **full** identity —
/// a digest alone is never sufficient, because two different protocol/content versions can share
/// an alias set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotStamp {
    pub protocol_version: u32,
    pub content_version: u64,
    /// Order-independent digest of the active alias address set.
    pub alias_set_digest: B256,
}

/// Order-independent digest of an alias address set: keccak256 over the sorted, deduplicated
/// 20-byte address values. Two textually different rule sets selecting the same addresses share a
/// digest, and reordering the rules never changes it.
pub fn alias_set_digest<I>(addresses: I) -> B256
where
    I: IntoIterator<Item = Address>,
{
    let mut sorted: Vec<Address> = addresses.into_iter().collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut bytes = Vec::with_capacity(sorted.len() * 20);
    for address in &sorted {
        bytes.extend_from_slice(address.as_slice());
    }
    keccak256(&bytes)
}

/// Why a trustworthy observation set could not be produced. This is **fail-closed input**, never
/// "capture ran and nothing matched" — collapsing the two would turn an unavailable capture into a
/// clean allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationsUnavailable {
    /// Capture was never armed on this execution path.
    NotCaptured,
    /// The frame stack was unbalanced or exceeded its bound.
    IncompleteFrames,
    /// The per-transaction observation budget was exceeded.
    BudgetExceeded { attempted: usize, limit: usize },
    /// The observation stamp did not match the evaluating snapshot.
    SnapshotMismatch,
    /// The two own-code signals disagreed, or the selected account's code was unavailable, so
    /// whether the parent frame ran its own code could not be decided.
    OwnCodeIndeterminate,
    /// The parked result was already consumed, or no result was ever parked.
    ResultAlreadyTaken,
    /// The execution engine could not return a trustworthy observation.
    EngineError,
}

impl ObservationsUnavailable {
    /// Fixed, low-cardinality discriminant for logs and metric labels. Never carries an address,
    /// a code hash or any other observation content.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotCaptured => "not_captured",
            Self::IncompleteFrames => "incomplete_frames",
            Self::BudgetExceeded { .. } => "budget_exceeded",
            Self::SnapshotMismatch => "snapshot_mismatch",
            Self::OwnCodeIndeterminate => "own_code_indeterminate",
            Self::ResultAlreadyTaken => "result_already_taken",
            Self::EngineError => "engine_error",
        }
    }
}

/// Tri-state, deliberately **not** an `Option`: "available and empty" and "unavailable" are
/// different verdicts. The first is a clean no-match, the second fails closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observations {
    Available { stamp: SnapshotStamp, items: Vec<DirectDelegatecallObservation> },
    Unavailable(ObservationsUnavailable),
}

impl Observations {
    /// Borrowed `Unavailable(NotCaptured)`, for callers on a path where capture never ran.
    ///
    /// Screening ignores this value whenever no rule declares an alias, so an event-only
    /// deployment keeps its existing behaviour; a rule that *does* declare an alias fails closed
    /// on it.
    pub fn not_captured() -> &'static Observations {
        static NOT_CAPTURED: OnceLock<Observations> = OnceLock::new();
        NOT_CAPTURED.get_or_init(|| Observations::Unavailable(ObservationsUnavailable::NotCaptured))
    }

    /// Observations for `address`, in execution order. Empty when the selected address produced
    /// no direct delegatecall, or when the set is unavailable — callers must check availability
    /// separately rather than reading an empty slice as "nothing matched".
    pub fn items_for(&self, address: Address) -> Vec<&DirectDelegatecallObservation> {
        match self {
            Observations::Available { items, .. } => {
                items.iter().filter(|o| o.context_address == address).collect()
            }
            Observations::Unavailable(_) => Vec::new(),
        }
    }
}

/// The four condition variables exposed per alias, as the suffixes appended to `<alias>.`.
pub const OBSERVATION_VARIABLE_SUFFIXES: [&str; 4] =
    ["contextAddress", "contextCodeHash", "delegateAddress", "delegateCodeHash"];

/// Flat JSONLogic bindings for one bound observation, keyed `<alias>.<field>` to match how
/// `<event>.<param>` keys already resolve. Address and hash values are lowercase `0x` hex.
pub fn observation_bindings(
    alias: &str,
    observation: &DirectDelegatecallObservation,
) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    let mut put = |suffix: &str, value: String| {
        out.insert(format!("{alias}.{suffix}"), serde_json::Value::String(value));
    };
    put("contextAddress", lower_address(observation.context_address));
    put("contextCodeHash", lower_hash(observation.context_code_hash));
    put("delegateAddress", lower_address(observation.delegate_address));
    put("delegateCodeHash", lower_hash(observation.delegate_code_hash));
    out
}

/// Lowercase `0x`-prefixed 20-byte address string.
pub fn lower_address(address: Address) -> String {
    format!("0x{}", alloy_primitives::hex::encode(address.as_slice()))
}

/// Lowercase `0x`-prefixed 32-byte hash string.
pub fn lower_hash(hash: B256) -> String {
    format!("0x{}", alloy_primitives::hex::encode(hash.as_slice()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(byte: u8) -> Address {
        Address::repeat_byte(byte)
    }

    fn observation(context: u8, delegate: u8) -> DirectDelegatecallObservation {
        DirectDelegatecallObservation {
            context_address: address(context),
            context_code_hash: B256::repeat_byte(0xc0),
            delegate_address: address(delegate),
            delegate_code_hash: B256::repeat_byte(0xd0),
        }
    }

    #[test]
    fn alias_set_digest_is_order_independent_and_dedups() {
        let a = address(1);
        let b = address(2);
        assert_eq!(alias_set_digest([a, b]), alias_set_digest([b, a]));
        assert_eq!(alias_set_digest([a, b, a]), alias_set_digest([a, b]));
        assert_ne!(alias_set_digest([a]), alias_set_digest([a, b]));
        // An empty set still has a stable digest rather than a sentinel value.
        assert_eq!(alias_set_digest([]), alias_set_digest([]));
    }

    #[test]
    fn unavailable_is_not_an_empty_available_set() {
        let empty = Observations::Available {
            stamp: SnapshotStamp {
                protocol_version: 1,
                content_version: 0,
                alias_set_digest: alias_set_digest([address(1)]),
            },
            items: Vec::new(),
        };
        let unavailable = Observations::Unavailable(ObservationsUnavailable::NotCaptured);
        assert_ne!(empty, unavailable);
        // Both yield no items for the address, which is exactly why availability has to be
        // checked separately from emptiness.
        assert!(empty.items_for(address(1)).is_empty());
        assert!(unavailable.items_for(address(1)).is_empty());
    }

    #[test]
    fn items_for_preserves_execution_order_and_filters_by_context() {
        let items = vec![observation(1, 9), observation(2, 9), observation(1, 8)];
        let set = Observations::Available {
            stamp: SnapshotStamp {
                protocol_version: 1,
                content_version: 0,
                alias_set_digest: alias_set_digest([address(1), address(2)]),
            },
            items,
        };
        let for_one = set.items_for(address(1));
        assert_eq!(for_one.len(), 2);
        assert_eq!(for_one[0].delegate_address, address(9));
        assert_eq!(for_one[1].delegate_address, address(8));
        assert_eq!(set.items_for(address(3)).len(), 0);
    }

    #[test]
    fn bindings_expose_exactly_the_four_documented_variables_lowercase() {
        let bindings = observation_bindings("proxy1", &observation(0xab, 0xcd));
        let mut keys: Vec<&str> = bindings.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "proxy1.contextAddress",
                "proxy1.contextCodeHash",
                "proxy1.delegateAddress",
                "proxy1.delegateCodeHash",
            ]
        );
        assert_eq!(
            bindings["proxy1.contextAddress"],
            serde_json::json!("0xabababababababababababababababababababab")
        );
        assert_eq!(
            bindings["proxy1.delegateCodeHash"],
            serde_json::json!("0xd0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0")
        );
        // `matched`, `callType`, `implAddress` and `implCodeHash` are deliberately absent.
        for forbidden in ["matched", "callType", "implAddress", "implCodeHash"] {
            assert!(!bindings.contains_key(&format!("proxy1.{forbidden}")));
        }
    }

    #[test]
    fn unavailable_labels_are_fixed_and_distinct() {
        let reasons = [
            ObservationsUnavailable::NotCaptured,
            ObservationsUnavailable::IncompleteFrames,
            ObservationsUnavailable::BudgetExceeded { attempted: 11, limit: 10 },
            ObservationsUnavailable::SnapshotMismatch,
            ObservationsUnavailable::OwnCodeIndeterminate,
            ObservationsUnavailable::ResultAlreadyTaken,
            ObservationsUnavailable::EngineError,
        ];
        let mut labels: Vec<&str> = reasons.iter().map(ObservationsUnavailable::label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), reasons.len(), "every reason needs its own label");
    }

    #[test]
    fn not_captured_is_a_stable_borrowed_value() {
        let first = Observations::not_captured();
        let second = Observations::not_captured();
        assert!(std::ptr::eq(first, second));
        assert_eq!(*first, Observations::Unavailable(ObservationsUnavailable::NotCaptured));
    }
}
