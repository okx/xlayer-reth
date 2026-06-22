//! Pure decision logic (the "rules"): the single source of truth for the feature's constants
//! and checks. Zero reth/revm coupling — the node adapters in [`crate::runtime`] call into
//! this. Organized as inner modules so each keeps its own tests:
//! - [`abi`]      — `getBlacklist` selector + codec
//! - [`snapshot`] — block-head paginated enumeration + fail-open
//! - [`inspector`]— execution-time balance-candidate accumulator
//! - [`eval`]     — two-check evaluator `log > balance`
//! - [`deposit`]  — included-as-reverted field rules + exempt senders
//! - [`mirror`]   — `chain_id → hardcoded mirror address` dispatch
//! - [`metrics`]  — Prometheus metrics

pub mod abi {
    //! `getBlacklist(uint256,uint256)` ABI via alloy `sol!` (standard ABI); the contract
    //! source of truth.

    use alloy_primitives::{Address, U256};
    use alloy_sol_types::{sol, SolCall};

    sol! {
        /// Mirror contract read-only enumeration: returns the total count and the
        /// `addresses[start .. start+limit]` slice.
        function getBlacklist(uint256 start, uint256 limit)
            external
            view
            returns (uint256 total, address[] addresses);
    }

    /// `getBlacklist` return-data decode failure → fail-open (empty snapshot).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
    #[error("getBlacklist return decode failed")]
    pub struct AbiError;

    /// Encode a `getBlacklist(start, limit)` calldata blob (`selector ++ start ++ limit`).
    pub fn encode_get_blacklist(start: U256, limit: U256) -> Vec<u8> {
        getBlacklistCall { start, limit }.abi_encode()
    }

    /// Decode the `(uint256 total, address[] addresses)` return tuple via standard ABI.
    pub fn decode_get_blacklist(data: &[u8]) -> Result<(U256, Vec<Address>), AbiError> {
        let r = getBlacklistCall::abi_decode_returns(data).map_err(|_| AbiError)?;
        Ok((r.total, r.addresses))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use alloy_primitives::{address, keccak256};
        use alloy_sol_types::SolValue;

        fn encode_return(total: u64, addrs: &[Address]) -> Vec<u8> {
            (U256::from(total), addrs.to_vec()).abi_encode_params()
        }

        #[test]
        fn selector_matches_keccak_of_signature() {
            let h = keccak256(b"getBlacklist(uint256,uint256)");
            assert_eq!(getBlacklistCall::SELECTOR, [h[0], h[1], h[2], h[3]]);
        }

        #[test]
        fn encode_call_is_selector_plus_two_words() {
            let cd = encode_get_blacklist(U256::from(0u64), U256::from(1024u64));
            assert_eq!(cd.len(), 4 + 32 + 32);
            assert_eq!(&cd[0..4], &getBlacklistCall::SELECTOR);
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
            assert_eq!(decode_get_blacklist(&[0u8; 10]), Err(AbiError));
        }
    }
}

pub mod inspector {
    //! Execution-time balance-candidate accumulator (the ETH-balance check). Plain data, no revm.

    use alloy_primitives::{Address, I256, U256};

    /// A candidate address for the ETH-balance check.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BalanceCandidate {
        /// The address whose net balance change is examined.
        pub address: Address,
        /// Committed balance before the transaction.
        pub balance_start: U256,
        /// Committed balance after the transaction.
        pub balance_end: U256,
        /// Signed fee-only delta to strip before judging a real transfer.
        pub fee_delta: I256,
        /// Whether this candidate arises from a selfdestruct beneficiary (affects category).
        pub selfdestruct: bool,
    }

    /// Accumulates per-transaction balance observations for the balance check.
    #[derive(Debug, Default, Clone)]
    pub struct BlacklistInspector {
        balance_candidates: Vec<BalanceCandidate>,
    }

    impl BlacklistInspector {
        /// A fresh, empty inspector (one per transaction).
        pub fn new() -> Self {
            Self::default()
        }

        /// Record a balance-check candidate.
        pub fn record_balance_candidate(&mut self, candidate: BalanceCandidate) {
            self.balance_candidates.push(candidate);
        }

        /// All recorded balance candidates.
        pub fn balance_candidates(&self) -> &[BalanceCandidate] {
            &self.balance_candidates
        }
    }
}

pub mod snapshot {
    //! Block-head snapshot read: paginated `getBlacklist` enumeration + fail-open.

    use super::abi::{decode_get_blacklist, encode_get_blacklist};
    use alloy_primitives::{address, Address, Bytes, U256};
    use std::collections::HashSet;
    use tracing::{error, warn};

    /// Per-page entry count requested from `getBlacklist`.
    pub const PAGE_SIZE: u64 = 1024;
    /// Gas budget for each per-page staticcall.
    pub const PER_PAGE_GAS: u64 = 50_000_000;
    /// Maximum entries enumerated; beyond this the list is deterministically truncated.
    pub const MAX_ENTRIES: u64 = 300_000;
    /// System address used as the staticcall caller (`0xff..fe`).
    pub const SYSTEM_ADDRESS: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

    /// An immutable, per-block blacklist snapshot.
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

        /// Iterate the listed addresses. Order is unspecified.
        pub fn iter(&self) -> impl Iterator<Item = &Address> + '_ {
            self.set.iter()
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

    /// Failure of a single mirror staticcall. Any error maps to fail-open.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum ViewCallError {
        /// The staticcall reverted, ran out of gas, or otherwise failed to return data.
        #[error("mirror staticcall failed")]
        CallFailed,
    }

    /// Abstraction over a single read-only EVM staticcall to the mirror contract.
    pub trait MirrorViewCaller {
        /// Issue `getBlacklist`-shaped calldata to `to` with the given gas budget. `Err` means
        /// the call reverted / OOG / produced no data.
        fn static_call(
            &mut self,
            to: Address,
            input: Bytes,
            gas: u64,
        ) -> Result<Bytes, ViewCallError>;
    }

    /// Enumerate the full blacklist at the current block head into a [`BlacklistSnapshot`].
    /// Fail-open: any failure → empty snapshot (logged).
    pub fn read_snapshot_at(
        caller: &mut impl MirrorViewCaller,
        mirror: Address,
    ) -> BlacklistSnapshot {
        let (total, first_page) = match call_page(caller, mirror, 0) {
            Ok(p) => p,
            Err(_) => {
                error!(target: "xlayer_blacklist", mirror = ?mirror, "snapshot read failed; fail-open empty");
                return BlacklistSnapshot::empty();
            }
        };

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
            if page.is_empty() {
                break;
            }
            let read_before = read;
            for addr in page {
                if read >= n {
                    break;
                }
                snapshot.insert(addr);
                read += 1;
            }
            if read >= n {
                break;
            }
            if read == read_before {
                break;
            }
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

        struct FakeMirror {
            total: U256,
            addrs: Vec<Address>,
            fail: bool,
            garbage: bool,
            calls: Vec<u64>,
        }

        impl FakeMirror {
            fn new(total: u64, addrs: Vec<Address>) -> Self {
                Self {
                    total: U256::from(total),
                    addrs,
                    fail: false,
                    garbage: false,
                    calls: Vec::new(),
                }
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
            let mut m = FakeMirror::new(0, vec![]);
            assert!(read_snapshot_at(&mut m, AAA).is_empty());
        }

        #[test]
        fn single_page_loads_all() {
            let mut m = FakeMirror::new(2, vec![AAA, BBB]);
            let snap = read_snapshot_at(&mut m, AAA);
            assert_eq!(snap.len(), 2);
            assert!(snap.contains(&AAA) && snap.contains(&BBB));
        }

        #[test]
        fn multi_page_paginates_by_consumed_count() {
            let addrs: Vec<Address> = (0..1500u32)
                .map(|i| {
                    let mut b = [0u8; 20];
                    b[16..20].copy_from_slice(&i.to_be_bytes());
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
            let addrs = vec![AAA, BBB];
            let mut m = FakeMirror::new(300_500, addrs);
            let snap = read_snapshot_at(&mut m, AAA);
            assert_eq!(snap.len(), 2);
        }

        #[test]
        fn zero_address_slot_advances_but_is_skipped() {
            let mut m = FakeMirror::new(3, vec![AAA, Address::ZERO, BBB]);
            let snap = read_snapshot_at(&mut m, AAA);
            assert_eq!(snap.len(), 2);
            assert!(!snap.contains(&Address::ZERO));
            assert!(snap.contains(&AAA) && snap.contains(&BBB));
        }

        #[test]
        fn call_failure_is_fail_open() {
            let mut m = FakeMirror::new(2, vec![AAA, BBB]);
            m.fail = true;
            assert!(read_snapshot_at(&mut m, AAA).is_empty());
        }

        #[test]
        fn decode_failure_is_fail_open() {
            let mut m = FakeMirror::new(2, vec![AAA, BBB]);
            m.garbage = true;
            assert!(read_snapshot_at(&mut m, AAA).is_empty());
        }

        #[test]
        fn total_overflowing_u64_is_fail_open() {
            let huge = U256::from(u64::MAX) + U256::from(1u64);
            let mut m = FakeMirror::with_total(huge, vec![AAA]);
            assert!(read_snapshot_at(&mut m, AAA).is_empty());
        }
    }
}

pub mod eval {
    //! Two-check evaluator with fixed priority `log > balance`.

    use super::inspector::{BalanceCandidate, BlacklistInspector};
    use super::snapshot::BlacklistSnapshot;
    use alloy_primitives::{keccak256, Address, Log, B256, I256};
    use std::sync::LazyLock;

    /// `keccak256("Transfer(address,address,uint256)")` (ERC20/721).
    pub static TRANSFER_TOPIC0: LazyLock<B256> =
        LazyLock::new(|| keccak256(b"Transfer(address,address,uint256)"));
    /// `keccak256("TransferSingle(address,address,address,uint256,uint256)")` (ERC1155).
    pub static TRANSFER_SINGLE_TOPIC0: LazyLock<B256> =
        LazyLock::new(|| keccak256(b"TransferSingle(address,address,address,uint256,uint256)"));
    /// `keccak256("TransferBatch(address,address,address,uint256[],uint256[])")` (ERC1155).
    pub static TRANSFER_BATCH_TOPIC0: LazyLock<B256> =
        LazyLock::new(|| keccak256(b"TransferBatch(address,address,address,uint256[],uint256[])"));

    /// The category of a blacklist hit. Maps 1:1 to the `hook` metric label.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HitCategory {
        /// Committed Transfer-class event (highest priority).
        Log,
        /// Net committed ETH balance change beyond the fee-only set.
        EthBalance,
        /// Balance change attributable to a selfdestruct beneficiary.
        SelfDestruct,
    }

    impl HitCategory {
        /// Prometheus `hook=` label value.
        pub fn metric_label(self) -> &'static str {
            match self {
                HitCategory::Log => "log",
                HitCategory::EthBalance => "eth_balance",
                HitCategory::SelfDestruct => "selfdestruct",
            }
        }
    }

    /// A single blacklist hit: the category and the offending blacklisted address.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Hit {
        /// Which check fired (resolved by priority when several would fire).
        pub category: HitCategory,
        /// The blacklisted address that was touched.
        pub address: Address,
    }

    /// The two-check evaluator. Stateless; operates purely on its inputs.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct BlacklistEvaluator;

    impl BlacklistEvaluator {
        /// Evaluate the transaction. Priority **log > balance**; `None` on no hit / empty snapshot.
        pub fn evaluate(
            inspector: &BlacklistInspector,
            logs: &[Log],
            snapshot: &BlacklistSnapshot,
        ) -> Option<Hit> {
            if snapshot.is_empty() {
                return None;
            }

            for log in logs {
                if let Some(addr) = transfer_log_hit(log, snapshot) {
                    return Some(Hit { category: HitCategory::Log, address: addr });
                }
            }

            for cand in inspector.balance_candidates() {
                if snapshot.contains(&cand.address) && balance_hit(cand) {
                    let category = if cand.selfdestruct {
                        HitCategory::SelfDestruct
                    } else {
                        HitCategory::EthBalance
                    };
                    return Some(Hit { category, address: cand.address });
                }
            }

            None
        }
    }

    /// Extract a blacklisted from/to from a committed Transfer-class log, if any.
    fn transfer_log_hit(log: &Log, snapshot: &BlacklistSnapshot) -> Option<Address> {
        let topics = log.topics();
        let topic0 = topics.first()?;

        if *topic0 == *TRANSFER_TOPIC0 {
            if topics.len() < 3 {
                return None;
            }
            return first_listed(snapshot, &[topics[1], topics[2]]);
        }

        if *topic0 == *TRANSFER_SINGLE_TOPIC0 || *topic0 == *TRANSFER_BATCH_TOPIC0 {
            if topics.len() < 4 {
                return None;
            }
            return first_listed(snapshot, &[topics[2], topics[3]]);
        }

        None
    }

    /// Return the first of `topics` (interpreted as a right-aligned address) that is listed.
    fn first_listed(snapshot: &BlacklistSnapshot, topics: &[B256]) -> Option<Address> {
        for t in topics {
            let addr = Address::from_word(*t);
            if snapshot.contains(&addr) {
                return Some(addr);
            }
        }
        None
    }

    /// `(balance_end - balance_start - fee_delta) != 0` in signed 256-bit space.
    fn balance_hit(c: &BalanceCandidate) -> bool {
        let start = match I256::try_from(c.balance_start) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let end = match I256::try_from(c.balance_end) {
            Ok(v) => v,
            Err(_) => return false,
        };
        match end.checked_sub(start).and_then(|d| d.checked_sub(c.fee_delta)) {
            Some(net) => net != I256::ZERO,
            None => false,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use alloy_primitives::{address, Address, LogData, U256};

        const AAA: Address = address!("00000000000000000000000000000000000000aa");
        const BBB: Address = address!("00000000000000000000000000000000000000bb");
        const ROUTER: Address = address!("00000000000000000000000000000000000000cc");
        const TOKEN: Address = address!("00000000000000000000000000000000000000dd");

        fn snapshot_with(addrs: &[Address]) -> BlacklistSnapshot {
            addrs.iter().copied().collect()
        }

        fn addr_topic(a: Address) -> B256 {
            a.into_word()
        }

        fn transfer_log(topic0: B256, topics_tail: &[Address]) -> Log {
            let mut topics = vec![topic0];
            topics.extend(topics_tail.iter().map(|a| addr_topic(*a)));
            Log {
                address: TOKEN,
                data: LogData::new(topics, Default::default()).expect("log data"),
            }
        }

        #[test]
        fn empty_snapshot_never_hits() {
            let insp = BlacklistInspector::new();
            let log = transfer_log(*TRANSFER_TOPIC0, &[BBB, AAA]);
            let snap = BlacklistSnapshot::empty();
            assert_eq!(BlacklistEvaluator::evaluate(&insp, &[log], &snap), None);
        }

        #[test]
        fn erc20_transfer_log_hits_on_to() {
            let insp = BlacklistInspector::new();
            let log = transfer_log(*TRANSFER_TOPIC0, &[BBB, AAA]);
            let snap = snapshot_with(&[AAA]);
            let hit = BlacklistEvaluator::evaluate(&insp, &[log], &snap).expect("hit");
            assert_eq!(hit.category, HitCategory::Log);
            assert_eq!(hit.address, AAA);
        }

        #[test]
        fn erc1155_transfer_single_hits_on_topics_2_3() {
            let insp = BlacklistInspector::new();
            let log = transfer_log(*TRANSFER_SINGLE_TOPIC0, &[ROUTER, BBB, AAA]);
            let snap = snapshot_with(&[AAA]);
            let hit = BlacklistEvaluator::evaluate(&insp, &[log], &snap).expect("hit");
            assert_eq!(hit.category, HitCategory::Log);
        }

        #[test]
        fn transfer_topic0_with_index_miss_does_not_hit() {
            let insp = BlacklistInspector::new();
            let log = transfer_log(*TRANSFER_TOPIC0, &[BBB, ROUTER]);
            let snap = snapshot_with(&[AAA]);
            assert_eq!(BlacklistEvaluator::evaluate(&insp, &[log], &snap), None);
        }

        #[test]
        fn insufficient_topics_defensively_skipped() {
            let insp = BlacklistInspector::new();
            let log = transfer_log(*TRANSFER_TOPIC0, &[AAA]);
            let snap = snapshot_with(&[AAA]);
            assert_eq!(BlacklistEvaluator::evaluate(&insp, &[log], &snap), None);
        }

        #[test]
        fn eth_balance_change_beyond_fee_hits() {
            let mut insp = BlacklistInspector::new();
            insp.record_balance_candidate(BalanceCandidate {
                address: AAA,
                balance_start: U256::from(100u64),
                balance_end: U256::from(150u64),
                fee_delta: I256::ZERO,
                selfdestruct: false,
            });
            let snap = snapshot_with(&[AAA]);
            let hit = BlacklistEvaluator::evaluate(&insp, &[], &snap).expect("hit");
            assert_eq!(hit.category, HitCategory::EthBalance);
        }

        #[test]
        fn fee_only_balance_change_does_not_hit() {
            let mut insp = BlacklistInspector::new();
            insp.record_balance_candidate(BalanceCandidate {
                address: AAA,
                balance_start: U256::from(100u64),
                balance_end: U256::from(79u64),
                fee_delta: I256::try_from(-21i64).unwrap(),
                selfdestruct: false,
            });
            let snap = snapshot_with(&[AAA]);
            assert_eq!(BlacklistEvaluator::evaluate(&insp, &[], &snap), None);
        }

        #[test]
        fn selfdestruct_candidate_categorized_as_selfdestruct() {
            let mut insp = BlacklistInspector::new();
            insp.record_balance_candidate(BalanceCandidate {
                address: AAA,
                balance_start: U256::ZERO,
                balance_end: U256::from(500u64),
                fee_delta: I256::ZERO,
                selfdestruct: true,
            });
            let snap = snapshot_with(&[AAA]);
            let hit = BlacklistEvaluator::evaluate(&insp, &[], &snap).expect("hit");
            assert_eq!(hit.category, HitCategory::SelfDestruct);
        }

        #[test]
        fn priority_log_beats_balance() {
            let mut insp = BlacklistInspector::new();
            insp.record_balance_candidate(BalanceCandidate {
                address: AAA,
                balance_start: U256::ZERO,
                balance_end: U256::from(1u64),
                fee_delta: I256::ZERO,
                selfdestruct: false,
            });
            let log = transfer_log(*TRANSFER_TOPIC0, &[BBB, AAA]);
            let snap = snapshot_with(&[AAA]);
            let hit = BlacklistEvaluator::evaluate(&insp, &[log], &snap).expect("hit");
            assert_eq!(hit.category, HitCategory::Log);
        }

        #[test]
        fn topic0_constants_match_keccak() {
            assert_eq!(*TRANSFER_TOPIC0, keccak256(b"Transfer(address,address,uint256)"));
            assert_eq!(
                *TRANSFER_SINGLE_TOPIC0,
                keccak256(b"TransferSingle(address,address,address,uint256,uint256)")
            );
            assert_eq!(
                *TRANSFER_BATCH_TOPIC0,
                keccak256(b"TransferBatch(address,address,address,uint256[],uint256[])")
            );
        }
    }
}

pub mod deposit {
    //! L1 forced-inclusion deposit handling: included-as-reverted field rules.

    use alloy_primitives::{address, Address, U256};

    /// System call sender (`0xff..fe`). Never intercepted.
    pub const SYSTEM_ADDRESS: Address = address!("fffffffffffffffffffffffffffffffffffffffe");
    /// L1-attributes depositor (`0xDeaD…0001`). Never intercepted.
    pub const L1_ATTRIBUTES_DEPOSITOR: Address =
        address!("deaddeaddeaddeaddeaddeaddeaddeaddead0001");

    /// Exhaustive exempt-sender set: deposits from these are never intercepted.
    pub const EXEMPT_SENDERS: [Address; 2] = [SYSTEM_ADDRESS, L1_ATTRIBUTES_DEPOSITOR];

    /// `CanyonDepositReceiptVersion` — written once Canyon is active.
    pub const CANYON_DEPOSIT_RECEIPT_VERSION: u64 = 1;

    /// Whether a deposit from `from` is exempt from interception.
    pub fn is_exempt_sender(from: &Address) -> bool {
        EXEMPT_SENDERS.contains(from)
    }

    /// The receipt + state field overrides applied to an intercepted deposit.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RevertedDepositOutcome {
        /// Receipt status — always `false` (0).
        pub status: bool,
        /// Receipt `gasUsed` — full `gasLimit`.
        pub gas_used: u64,
        /// `receipt.DepositNonce` — the pre-exec sender nonce `N`.
        pub deposit_nonce: u64,
        /// Post-state sender account nonce — `N+1`.
        pub account_nonce: u64,
        /// `receipt.DepositReceiptVersion` — `Some(CANYON…)` iff Canyon active, else `None`.
        pub deposit_receipt_version: Option<u64>,
        /// Mint amount to re-credit after rollback (`Some` iff the deposit carried a mint).
        pub keep_mint: Option<U256>,
        /// Whether to clear the receipt logs (always `true`).
        pub clear_logs: bool,
    }

    /// Compute the included-as-reverted field overrides for an intercepted deposit.
    pub fn apply_included_as_reverted(
        pre_exec_nonce: u64,
        gas_limit: u64,
        mint: Option<U256>,
        canyon_active: bool,
    ) -> RevertedDepositOutcome {
        RevertedDepositOutcome {
            status: false,
            gas_used: gas_limit,
            deposit_nonce: pre_exec_nonce,
            account_nonce: pre_exec_nonce.saturating_add(1),
            deposit_receipt_version: canyon_active.then_some(CANYON_DEPOSIT_RECEIPT_VERSION),
            keep_mint: mint,
            clear_logs: true,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn system_and_l1_attributes_are_exempt() {
            assert!(is_exempt_sender(&SYSTEM_ADDRESS));
            assert!(is_exempt_sender(&L1_ATTRIBUTES_DEPOSITOR));
        }

        #[test]
        fn normal_sender_is_not_exempt() {
            assert!(!is_exempt_sender(&address!("00000000000000000000000000000000000000aa")));
        }

        #[test]
        fn reverted_deposit_field_rules_canyon_active() {
            let o = apply_included_as_reverted(7, 21_000, Some(U256::from(1_000u64)), true);
            assert!(!o.status);
            assert_eq!(o.gas_used, 21_000);
            assert_eq!(o.deposit_nonce, 7);
            assert_eq!(o.account_nonce, 8);
            assert_eq!(o.deposit_receipt_version, Some(CANYON_DEPOSIT_RECEIPT_VERSION));
            assert_eq!(o.keep_mint, Some(U256::from(1_000u64)));
            assert!(o.clear_logs);
        }

        #[test]
        fn reverted_deposit_no_version_when_canyon_inactive() {
            let o = apply_included_as_reverted(0, 50_000, None, false);
            assert_eq!(o.deposit_receipt_version, None);
            assert_eq!(o.keep_mint, None);
            assert_eq!(o.account_nonce, 1);
        }

        #[test]
        fn deposit_nonce_and_account_nonce_intentionally_differ() {
            let o = apply_included_as_reverted(42, 100, None, true);
            assert_eq!(o.deposit_nonce, 42);
            assert_eq!(o.account_nonce, 43);
            assert_ne!(o.deposit_nonce, o.account_nonce);
        }
    }
}

pub mod mirror {
    //! `chain_id → mirror contract address` dispatch (no CLI switch).

    use alloy_primitives::{address, Address};

    /// X Layer devnet chain id.
    pub const XLAYER_DEVNET_CHAIN_ID: u64 = 195;
    /// X Layer testnet chain id.
    pub const XLAYER_TESTNET_CHAIN_ID: u64 = 1952;
    /// X Layer mainnet chain id.
    pub const XLAYER_MAINNET_CHAIN_ID: u64 = 196;

    /// Devnet (195) mirror address — deterministic, fixed.
    pub const DEVNET_MIRROR_ADDRESS: Address = address!("73511669fd4de447fed18bb79bafeac93ab7f31f");

    /// Testnet (1952) mirror address — PLACEHOLDER.
    // TODO: replace placeholder before testnet enablement.
    pub const TESTNET_MIRROR_ADDRESS: Address =
        address!("000000000000000000626c61636b6c6973741952");

    /// Mainnet (196) mirror address — PLACEHOLDER.
    // TODO: replace placeholder before mainnet enablement.
    pub const MAINNET_MIRROR_ADDRESS: Address =
        address!("000000000000000000626c61636b6c6973740196");

    /// Resolve the mirror contract address for `chain_id` (`None` = full no-op).
    pub fn mirror_address_for_chain(chain_id: u64) -> Option<Address> {
        match chain_id {
            XLAYER_DEVNET_CHAIN_ID => Some(DEVNET_MIRROR_ADDRESS),
            XLAYER_TESTNET_CHAIN_ID => Some(TESTNET_MIRROR_ADDRESS),
            XLAYER_MAINNET_CHAIN_ID => Some(MAINNET_MIRROR_ADDRESS),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn devnet_resolves_to_fixed_address() {
            assert_eq!(
                mirror_address_for_chain(XLAYER_DEVNET_CHAIN_ID),
                Some(DEVNET_MIRROR_ADDRESS)
            );
            let bytes = DEVNET_MIRROR_ADDRESS.into_array();
            assert_eq!(bytes[0], 0x73);
            assert_eq!(bytes[19], 0x1f);
        }

        #[test]
        fn testnet_and_mainnet_resolve_to_placeholder_some() {
            assert_eq!(
                mirror_address_for_chain(XLAYER_TESTNET_CHAIN_ID),
                Some(TESTNET_MIRROR_ADDRESS)
            );
            assert_eq!(
                mirror_address_for_chain(XLAYER_MAINNET_CHAIN_ID),
                Some(MAINNET_MIRROR_ADDRESS)
            );
        }

        #[test]
        fn placeholders_have_expected_bytes() {
            assert_eq!(
                TESTNET_MIRROR_ADDRESS,
                address!("000000000000000000626c61636b6c6973741952")
            );
            assert_eq!(
                MAINNET_MIRROR_ADDRESS,
                address!("000000000000000000626c61636b6c6973740196")
            );
            assert_eq!(&TESTNET_MIRROR_ADDRESS.into_array()[9..18], b"blacklist");
            assert_eq!(&MAINNET_MIRROR_ADDRESS.into_array()[9..18], b"blacklist");
        }

        #[test]
        fn unrecognized_chain_resolves_to_none() {
            assert_eq!(mirror_address_for_chain(10), None);
            assert_eq!(mirror_address_for_chain(1), None);
            assert_eq!(mirror_address_for_chain(0), None);
        }
    }
}

pub mod metrics {
    //! Prometheus metrics for the blacklist feature.

    use super::eval::HitCategory;
    use reth_metrics::{
        metrics::{Gauge, Histogram},
        Metrics,
    };

    /// Blacklist metrics under the `xlayer_blacklist` scope.
    #[derive(Metrics, Clone)]
    #[metrics(scope = "xlayer_blacklist")]
    pub struct BlacklistMetrics {
        /// Current block snapshot size → `xlayer_blacklist_cache_size`.
        pub cache_size: Gauge,
        /// Block-head snapshot read duration → `xlayer_blacklist_snapshot_read_duration_nanoseconds`
        /// (named by its real unit).
        pub snapshot_read_duration_nanoseconds: Histogram,
    }

    /// Metric name for the category-labelled execution-gate revert counter.
    const EXEC_REVERT_TOTAL: &str = "xlayer_blacklist_exec_revert_total";

    impl BlacklistMetrics {
        /// Increment `xlayer_blacklist_exec_revert_total{hook=<category>}` by one.
        pub fn increment_exec_revert(category: HitCategory) {
            metrics::counter!(EXEC_REVERT_TOTAL, "hook" => category.metric_label()).increment(1);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn metrics_struct_constructs() {
            let _m = BlacklistMetrics::default();
        }

        #[test]
        fn exec_revert_labels_cover_all_categories() {
            BlacklistMetrics::increment_exec_revert(HitCategory::Log);
            BlacklistMetrics::increment_exec_revert(HitCategory::EthBalance);
            BlacklistMetrics::increment_exec_revert(HitCategory::SelfDestruct);
        }
    }
}
