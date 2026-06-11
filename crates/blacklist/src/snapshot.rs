//! FR-4 — block-head snapshot read: paginated `getBlacklist` enumeration + fail-open.
//!
//! Once per block, before the transaction loop, the node issues a read-only staticcall
//! to the mirror contract's `getBlacklist` from the system address and enumerates the
//! full list into an immutable in-memory [`BlacklistSnapshot`]. The enumeration
//! algorithm and all read parameters are cross-client constants — any mismatch between
//! op-reth and op-geth forks the chain.
//!
//! The actual revm staticcall is supplied by the caller through the [`MirrorViewCaller`]
//! trait so this module stays free of any revm/reth coupling and is unit-testable with
//! an in-memory fake. [TD §4.1]

use crate::abi::{decode_get_blacklist, encode_get_blacklist};
use alloy_primitives::{address, Address, Bytes, U256};
use std::collections::HashSet;
use tracing::{error, warn};

/// Per-page entry count requested from `getBlacklist`. (cross-client constant)
pub const PAGE_SIZE: u64 = 1024;
/// Gas budget for each per-page staticcall. (cross-client constant)
pub const PER_PAGE_GAS: u64 = 50_000_000;
/// Maximum entries enumerated; beyond this the list is deterministically truncated.
/// (cross-client constant)
pub const MAX_ENTRIES: u64 = 300_000;
/// System address used as the staticcall caller (`0xff..fe`). (cross-client constant)
pub const SYSTEM_ADDRESS: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

/// An immutable, per-block blacklist snapshot. Block-internal it never changes; between
/// blocks it is replaced wholesale.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BlacklistSnapshot {
    set: HashSet<Address>,
}

impl BlacklistSnapshot {
    /// An empty snapshot (the fail-open / disabled result).
    pub fn empty() -> Self {
        Self { set: HashSet::new() }
    }

    /// Whether `addr` is on the list.
    pub fn contains(&self, addr: &Address) -> bool {
        self.set.contains(addr)
    }

    /// Number of distinct addresses on the list.
    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// Whether the list is empty (disabled / fail-open / not-yet-populated).
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Insert an address (zero address is ignored — defensive skip).
    fn insert(&mut self, addr: Address) {
        if addr != Address::ZERO {
            self.set.insert(addr);
        }
    }
}

impl FromIterator<Address> for BlacklistSnapshot {
    fn from_iter<T: IntoIterator<Item = Address>>(iter: T) -> Self {
        let mut s = Self::empty();
        for a in iter {
            s.insert(a);
        }
        s
    }
}

/// Failure of a single mirror staticcall. Any error maps to fail-open. [TD §4.1]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ViewCallError {
    /// The staticcall reverted, ran out of gas, or otherwise failed to return data.
    #[error("mirror staticcall failed")]
    CallFailed,
}

/// Abstraction over a single read-only EVM staticcall to the mirror contract.
///
/// The node-side adapter implements this over a revm read-only EVM constructed on the
/// parent state. Implementations MUST NOT mutate state.
pub trait MirrorViewCaller {
    /// Issue `getBlacklist`-shaped calldata to `to` with the given gas budget, returning
    /// the raw ABI return bytes. `Err` means the call reverted / OOG / produced no data.
    fn static_call(&mut self, to: Address, input: Bytes, gas: u64) -> Result<Bytes, ViewCallError>;
}

/// Enumerate the full blacklist at the current block head into a [`BlacklistSnapshot`].
///
/// Fail-open: any view-call failure, decode failure, type mismatch, `total` not
/// representable as `u64`, or `total == 0` yields an **empty** snapshot (logged). Because
/// both clients read the same parent state, failures are deterministic and consensus-safe.
///
/// Enumeration (cross-client, [TD §4.1] steps 1–7):
/// 1. `getBlacklist(0, PAGE_SIZE)` → `(total, page0)`.
/// 2. decode/call failure ∨ `total` not `u64` ∨ `total == 0` → empty.
/// 3. `n = min(total, MAX_ENTRIES)`; `total > MAX_ENTRIES` → truncate + `warn!`.
/// 4. next page `start = read` (consumed-entry count, not a fixed step).
/// 5. accumulate addresses until `read >= n`; each slot advances `read` (a zero slot
///    advances `read` but is not added to the set) so both clients truncate at the same index.
/// 6. a page returning an empty array stops enumeration.
/// 7. anti-spin guard: a non-empty page that does not advance `read` stops enumeration.
pub fn read_snapshot_at(caller: &mut impl MirrorViewCaller, mirror: Address) -> BlacklistSnapshot {
    // Step 1.
    let (total, first_page) = match call_page(caller, mirror, 0) {
        Ok(p) => p,
        Err(_) => {
            error!(target: "xlayer_blacklist", mirror = ?mirror, "snapshot read failed; fail-open empty");
            return BlacklistSnapshot::empty();
        }
    };

    // Step 2.
    if total == U256::ZERO {
        return BlacklistSnapshot::empty();
    }
    let total_u64 = match u64::try_from(total) {
        Ok(v) => v,
        Err(_) => {
            error!(target: "xlayer_blacklist", "getBlacklist total exceeds u64; fail-open empty");
            return BlacklistSnapshot::empty();
        }
    };

    // Step 3.
    let n = total_u64.min(MAX_ENTRIES);
    if total_u64 > MAX_ENTRIES {
        warn!(
            target: "xlayer_blacklist",
            total = total_u64,
            cap = MAX_ENTRIES,
            "blacklist total exceeds MAX_ENTRIES; truncating to deterministic prefix"
        );
    }

    let mut snapshot = BlacklistSnapshot::empty();
    let mut read: u64 = 0;
    let mut page = first_page;

    loop {
        // Step 6.
        if page.is_empty() {
            break;
        }
        let read_before = read;
        // Step 5.
        for addr in page {
            if read >= n {
                break;
            }
            snapshot.insert(addr); // zero-address skipped inside insert, slot still consumed
            read += 1;
        }
        if read >= n {
            break;
        }
        // Step 7 — anti-spin: non-empty page that didn't advance the cursor.
        if read == read_before {
            break;
        }
        // Step 4 — fetch the next page keyed by consumed count.
        page = match call_page(caller, mirror, read) {
            Ok((_, p)) => p,
            Err(_) => {
                error!(target: "xlayer_blacklist", "mid-enumeration page read failed; fail-open empty");
                return BlacklistSnapshot::empty();
            }
        };
    }

    snapshot
}

/// One `getBlacklist(start, PAGE_SIZE)` staticcall + decode.
fn call_page(
    caller: &mut impl MirrorViewCaller,
    mirror: Address,
    start: u64,
) -> Result<(U256, Vec<Address>), ViewCallError> {
    let input = encode_get_blacklist(U256::from(start), U256::from(PAGE_SIZE));
    let out = caller.static_call(mirror, Bytes::from(input), PER_PAGE_GAS)?;
    decode_get_blacklist(&out).map_err(|_| ViewCallError::CallFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    const AAA: Address = address!("00000000000000000000000000000000000000aa");
    const BBB: Address = address!("00000000000000000000000000000000000000bb");

    /// In-memory fake mirror. Returns ABI-encoded pages keyed by `start`.
    struct FakeMirror {
        /// (total, full address list); pages are sliced from this on demand.
        total: U256,
        addrs: Vec<Address>,
        /// Force a hard call failure on every call.
        fail: bool,
        /// Return malformed bytes (decode failure).
        garbage: bool,
        calls: Vec<u64>,
    }

    impl FakeMirror {
        fn new(total: u64, addrs: Vec<Address>) -> Self {
            Self { total: U256::from(total), addrs, fail: false, garbage: false, calls: Vec::new() }
        }
        fn with_total(total: U256, addrs: Vec<Address>) -> Self {
            Self { total, addrs, fail: false, garbage: false, calls: Vec::new() }
        }
        fn encode_page(&self, start: u64) -> Vec<u8> {
            let start = start as usize;
            let page: Vec<Address> =
                self.addrs.iter().skip(start).take(PAGE_SIZE as usize).copied().collect();
            let mut out = Vec::new();
            out.extend_from_slice(&self.total.to_be_bytes::<32>());
            out.extend_from_slice(&U256::from(64u64).to_be_bytes::<32>());
            out.extend_from_slice(&U256::from(page.len() as u64).to_be_bytes::<32>());
            for a in &page {
                let mut word = [0u8; 32];
                word[12..32].copy_from_slice(a.as_slice());
                out.extend_from_slice(&word);
            }
            out
        }
    }

    impl MirrorViewCaller for FakeMirror {
        fn static_call(
            &mut self,
            _to: Address,
            input: Bytes,
            _gas: u64,
        ) -> Result<Bytes, ViewCallError> {
            if self.fail {
                return Err(ViewCallError::CallFailed);
            }
            // Decode the requested start from calldata (selector + start + limit).
            let start = U256::from_be_slice(&input[4..36]);
            let start = u64::try_from(start).unwrap();
            self.calls.push(start);
            if self.garbage {
                return Ok(Bytes::from(vec![0u8; 10]));
            }
            Ok(Bytes::from(self.encode_page(start)))
        }
    }

    #[test]
    fn empty_total_is_empty_snapshot() {
        // DM-4.1
        let mut m = FakeMirror::new(0, vec![]);
        assert!(read_snapshot_at(&mut m, AAA).is_empty());
    }

    #[test]
    fn single_page_loads_all() {
        // DM-4.2
        let mut m = FakeMirror::new(2, vec![AAA, BBB]);
        let snap = read_snapshot_at(&mut m, AAA);
        assert_eq!(snap.len(), 2);
        assert!(snap.contains(&AAA) && snap.contains(&BBB));
    }

    #[test]
    fn multi_page_paginates_by_consumed_count() {
        // DM-4.3 — 1500 entries → page0 start=0, page1 start=1024.
        let addrs: Vec<Address> = (0..1500u32)
            .map(|i| {
                let mut b = [0u8; 20];
                b[16..20].copy_from_slice(&i.to_be_bytes());
                // Set a constant high byte so no generated address is ever the zero
                // address (which `insert` defensively skips) — every `i` stays distinct
                // and non-zero regardless of its low bytes.
                b[0] = 1;
                Address::from(b)
            })
            .collect();
        let mut m = FakeMirror::new(1500, addrs);
        let snap = read_snapshot_at(&mut m, AAA);
        assert_eq!(snap.len(), 1500);
        assert_eq!(m.calls, vec![0, 1024]);
    }

    #[test]
    fn over_cap_truncates_to_max_entries() {
        // DM-4.4 — total > MAX_ENTRIES but list serves only a few; loads the prefix.
        let addrs = vec![AAA, BBB];
        let mut m = FakeMirror::new(300_500, addrs);
        let snap = read_snapshot_at(&mut m, AAA);
        // Only 2 real entries returned before the page empties (step 6).
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn zero_address_slot_advances_but_is_skipped() {
        // DM-4.5
        let mut m = FakeMirror::new(3, vec![AAA, Address::ZERO, BBB]);
        let snap = read_snapshot_at(&mut m, AAA);
        assert_eq!(snap.len(), 2);
        assert!(!snap.contains(&Address::ZERO));
        assert!(snap.contains(&AAA) && snap.contains(&BBB));
    }

    #[test]
    fn call_failure_is_fail_open() {
        // DM-4.8
        let mut m = FakeMirror::new(2, vec![AAA, BBB]);
        m.fail = true;
        assert!(read_snapshot_at(&mut m, AAA).is_empty());
    }

    #[test]
    fn decode_failure_is_fail_open() {
        // DM-4.9
        let mut m = FakeMirror::new(2, vec![AAA, BBB]);
        m.garbage = true;
        assert!(read_snapshot_at(&mut m, AAA).is_empty());
    }

    #[test]
    fn total_overflowing_u64_is_fail_open() {
        // DM-4.10 — total > u64::MAX.
        let huge = U256::from(u64::MAX) + U256::from(1u64);
        let mut m = FakeMirror::with_total(huge, vec![AAA]);
        assert!(read_snapshot_at(&mut m, AAA).is_empty());
    }
}
