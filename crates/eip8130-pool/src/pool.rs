//! 2D-nonce-aware transaction pool for EIP-8130 AA transactions.
//!
//! Reth's standard pool uses `(sender, nonce_sequence)` as the identity key
//! and tracks account nonces from EVM state. EIP-8130 transactions use the
//! `NONCE_MANAGER` contract for nonce management, need expiry tracking, and
//! can have multiple lanes that collide in the standard pool.
//!
//! This module provides an [`Eip8130Pool`] that stores **all** EIP-8130
//! transactions, keyed by the full 2D identity `(sender, nonce_key,
//! nonce_sequence)`. The standard Reth pool still receives them for RPC
//! backward compatibility, but does not drive nonce ordering, expiry, or
//! P2P gossip for these transactions.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_primitives::{Address, B256};
use parking_lot::RwLock;
use reth_transaction_pool::{
    error::InvalidPoolTransactionError,
    identifier::{SenderId, TransactionId},
    PoolTransaction, TransactionOrigin, ValidPoolTransaction,
};
use tokio::sync::broadcast;

use crate::transaction::{AddOutcome, Eip8130SequenceId, Eip8130TxId};

/// Per-sequence state: on-chain nonce and ordered map of pool transactions.
struct SequenceState<T> {
    next_nonce: u64,
    txs: BTreeMap<u64, PooledEntry<T>>,
}

impl<T> Default for SequenceState<T> {
    fn default() -> Self {
        Self { next_nonce: 0, txs: BTreeMap::new() }
    }
}

/// An entry stored in the pool alongside the full transaction.
struct PooledEntry<T> {
    id: Eip8130TxId,
    transaction: T,
    origin: TransactionOrigin,
    timestamp: Instant,
    /// Whether this transaction is executable (contiguous nonce chain from on-chain nonce).
    is_pending: bool,
    /// Monotonic insertion counter for eviction tie-breaking.
    submission_id: u64,
    /// Cached `max_priority_fee_per_gas` for eviction ordering.
    priority_fee: u128,
    /// Unix timestamp after which this transaction is invalid. `0` = no expiry.
    expiry: u64,
}

/// Key for ordering transactions in the eviction set.
///
/// Lower priority is evicted first. On equal priority, newer submissions
/// (higher `submission_id`) are evicted first to favour older transactions.
/// The hash field provides a unique tiebreaker for `BTreeSet`.
#[derive(Debug, Clone)]
struct EvictionKey {
    priority_fee: u128,
    submission_id: u64,
    hash: B256,
}

impl PartialEq for EvictionKey {
    fn eq(&self, other: &Self) -> bool {
        self.priority_fee == other.priority_fee
            && self.submission_id == other.submission_id
            && self.hash == other.hash
    }
}

impl Eq for EvictionKey {}

impl PartialOrd for EvictionKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EvictionKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority_fee
            .cmp(&other.priority_fee)
            .then_with(|| other.submission_id.cmp(&self.submission_id))
            .then_with(|| self.hash.cmp(&other.hash))
    }
}

/// Throughput tier for an account, determined lazily by the pool when an
/// account is about to breach the default cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ThroughputTier {
    /// Account has not been checked or does not qualify — default throughput.
    #[default]
    Default,
    /// Account is locked — elevated sender throughput.
    Locked,
    /// Account is locked and has trusted bytecode — elevated sender and payer throughput.
    LockedTrustedBytecode,
}

/// Result of a lazy tier check, returned by the `check_tier` closure.
#[derive(Debug, Clone, Copy)]
pub struct TierCheckResult {
    /// The resolved throughput tier for the account.
    pub tier: ThroughputTier,
    /// Suggested cache lifetime based on the on-chain unlock deadline.
    pub cache_for: Option<Duration>,
}

/// Configuration for the EIP-8130 2D nonce pool.
#[derive(Debug, Clone)]
pub struct Eip8130PoolConfig {
    /// Maximum transactions per sequence lane `(sender, nonce_key)`.
    pub max_txs_per_sequence: usize,
    /// Maximum total transactions in the pool.
    pub max_pool_size: usize,
    /// Sender-role limit for accounts at the default tier.
    pub default_max_sender_txs: usize,
    /// Sender-role limit for locked accounts.
    pub locked_max_sender_txs: usize,
    /// Sender-role limit for locked accounts with trusted bytecode.
    pub trusted_max_sender_txs: usize,
    /// Payer-role limit for accounts at the default (and locked) tier.
    pub default_max_payer_txs: usize,
    /// Payer-role limit for locked accounts with trusted bytecode.
    pub trusted_max_payer_txs: usize,
    /// Maximum time a cached tier remains valid.
    pub tier_cache_ttl: Duration,
    /// Minimum percentage fee increase required for replacement transactions.
    pub price_bump_percent: u64,
}

impl Default for Eip8130PoolConfig {
    fn default() -> Self {
        Self {
            max_txs_per_sequence: 16,
            max_pool_size: 4096,
            default_max_sender_txs: 8,
            locked_max_sender_txs: 64,
            trusted_max_sender_txs: 128,
            default_max_payer_txs: 8,
            trusted_max_payer_txs: 128,
            tier_cache_ttl: Duration::from_secs(300),
            price_bump_percent: 10,
        }
    }
}

impl Eip8130PoolConfig {
    /// Returns the sender-role transaction cap for the given tier.
    pub fn max_sender_txs_for_tier(&self, tier: ThroughputTier) -> usize {
        match tier {
            ThroughputTier::Default => self.default_max_sender_txs,
            ThroughputTier::Locked => self.locked_max_sender_txs,
            ThroughputTier::LockedTrustedBytecode => self.trusted_max_sender_txs,
        }
    }

    /// Returns the payer-role transaction cap for the given tier.
    pub fn max_payer_txs_for_tier(&self, tier: ThroughputTier) -> usize {
        match tier {
            ThroughputTier::Default | ThroughputTier::Locked => self.default_max_payer_txs,
            ThroughputTier::LockedTrustedBytecode => self.trusted_max_payer_txs,
        }
    }
}

/// Cached throughput tier with a monotonic expiry.
#[derive(Debug, Clone, Copy)]
struct CachedTier {
    tier: ThroughputTier,
    expires_at: Instant,
}

struct PoolInner<T> {
    sequences: HashMap<Eip8130SequenceId, SequenceState<T>>,
    by_hash: HashMap<B256, Eip8130TxId>,
    /// Forward index: nonce storage slot → sequence ID.
    slot_to_seq: HashMap<B256, Eip8130SequenceId>,
    /// Reverse index: sequence ID → nonce storage slot (for O(1) cleanup).
    seq_to_slot: HashMap<Eip8130SequenceId, B256>,
    /// Per-account count of pool txs where the account acts as **sender**.
    sender_txs: HashMap<Address, usize>,
    /// Per-account count of pool txs where the account acts as **payer**.
    payer_txs: HashMap<Address, usize>,
    /// Payer address for each tx hash.
    payer_by_hash: HashMap<B256, Address>,
    /// Cached throughput tier per account.
    account_tiers: HashMap<Address, CachedTier>,
    /// Monotonic counter for eviction tie-breaking.
    next_submission_id: u64,
    /// All transactions ordered by eviction priority (lowest first).
    by_eviction_order: BTreeSet<EvictionKey>,
}

impl<T> Default for PoolInner<T> {
    fn default() -> Self {
        Self {
            sequences: HashMap::new(),
            by_hash: HashMap::new(),
            slot_to_seq: HashMap::new(),
            seq_to_slot: HashMap::new(),
            sender_txs: HashMap::new(),
            payer_txs: HashMap::new(),
            payer_by_hash: HashMap::new(),
            account_tiers: HashMap::new(),
            next_submission_id: 0,
            by_eviction_order: BTreeSet::new(),
        }
    }
}

/// A 2D-nonce-aware pool for EIP-8130 transactions.
///
/// Thread-safe via interior `RwLock`. All public methods acquire the lock
/// internally, so callers do not need external synchronization.
pub struct Eip8130Pool<T> {
    inner: RwLock<PoolInner<T>>,
    config: Eip8130PoolConfig,
    /// Broadcasts the hash of every newly-pending EIP-8130 transaction.
    pending_tx_sender: broadcast::Sender<B256>,
}

/// Capacity of the broadcast channel for pending EIP-8130 transaction hashes.
const PENDING_TX_BROADCAST_CAPACITY: usize = 256;

impl<T> Default for Eip8130Pool<T> {
    fn default() -> Self {
        Self::with_config(Eip8130PoolConfig::default())
    }
}

impl<T> Eip8130Pool<T> {
    /// Creates an empty pool with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a pool with the given configuration.
    pub fn with_config(config: Eip8130PoolConfig) -> Self {
        let (pending_tx_sender, _) = broadcast::channel(PENDING_TX_BROADCAST_CAPACITY);
        Self { inner: RwLock::new(PoolInner::default()), config, pending_tx_sender }
    }

    /// Returns a reference to the pool configuration.
    pub fn config(&self) -> &Eip8130PoolConfig {
        &self.config
    }

    /// Subscribes to newly-pending EIP-8130 transaction hashes.
    pub fn subscribe_pending_transactions(&self) -> broadcast::Receiver<B256> {
        self.pending_tx_sender.subscribe()
    }

    /// Returns `true` if the pool contains a transaction with the given hash.
    pub fn contains(&self, hash: &B256) -> bool {
        self.inner.read().by_hash.contains_key(hash)
    }

    /// Returns the 2D identity for a transaction hash, if present.
    pub fn get_id(&self, hash: &B256) -> Option<Eip8130TxId> {
        self.inner.read().by_hash.get(hash).cloned()
    }

    /// Number of transactions currently in the pool.
    pub fn len(&self) -> usize {
        self.inner.read().by_hash.len()
    }

    /// Returns `true` if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.read().by_hash.is_empty()
    }

    /// Looks up the sequence ID for a nonce storage slot, if tracked.
    pub fn seq_id_for_slot(&self, slot: &B256) -> Option<Eip8130SequenceId> {
        self.inner.read().slot_to_seq.get(slot).cloned()
    }

    /// Resets the cached throughput tier for an account.
    pub fn invalidate_account_tier(&self, account: &Address) {
        self.inner.write().account_tiers.remove(account);
    }

    /// Returns all transaction hashes in the pool.
    pub fn all_hashes(&self) -> Vec<B256> {
        self.inner.read().by_hash.keys().copied().collect()
    }

    /// Removes a transaction by hash. Returns the id if found.
    pub fn remove_transaction(&self, hash: &B256) -> Option<Eip8130TxId>
    where
        T: PoolTransaction,
    {
        let mut inner = self.inner.write();
        Self::remove_from_inner(&mut inner, hash)
    }

    /// Removes multiple transactions by hash. Returns the hashes that were
    /// actually present.
    pub fn remove_transactions(&self, hashes: &[B256]) -> Vec<B256>
    where
        T: PoolTransaction,
    {
        let mut inner = self.inner.write();
        hashes.iter().filter_map(|h| Self::remove_from_inner(&mut inner, h).map(|_| *h)).collect()
    }

    /// Removes all transactions whose `expiry` is non-zero and ≤ `current_timestamp`.
    pub fn sweep_expired(&self, current_timestamp: u64) -> Vec<B256>
    where
        T: PoolTransaction,
    {
        let expired_hashes: Vec<B256> = {
            let inner = self.inner.read();
            inner
                .by_hash
                .iter()
                .filter_map(|(hash, id)| {
                    let seq_id = id.sequence_id();
                    let seq = inner.sequences.get(&seq_id)?;
                    let entry = seq.txs.get(&id.nonce_sequence)?;
                    if entry.expiry != 0 && entry.expiry <= current_timestamp {
                        Some(*hash)
                    } else {
                        None
                    }
                })
                .collect()
        };

        if expired_hashes.is_empty() {
            return Vec::new();
        }
        self.remove_transactions(&expired_hashes)
    }

    /// Updates the known on-chain nonce for a sequence lane and removes any
    /// transactions with `nonce_sequence < new_nonce`. Remaining transactions
    /// are re-evaluated for pending/queued status.
    ///
    /// Returns the hashes of pruned transactions.
    pub fn update_sequence_nonce(&self, seq_id: &Eip8130SequenceId, new_nonce: u64) -> Vec<B256>
    where
        T: PoolTransaction,
    {
        let mut inner = self.inner.write();
        let mut removed_hashes = Vec::new();
        let mut eviction_keys_to_remove = Vec::new();

        if let Some(seq) = inner.sequences.get_mut(seq_id) {
            seq.next_nonce = new_nonce;
            let stale: Vec<u64> = seq.txs.range(..new_nonce).map(|(&nonce, _)| nonce).collect();
            for nonce in stale {
                if let Some(entry) = seq.txs.remove(&nonce) {
                    let hash = *entry.transaction.hash();
                    eviction_keys_to_remove.push(EvictionKey {
                        priority_fee: entry.priority_fee,
                        submission_id: entry.submission_id,
                        hash,
                    });
                    removed_hashes.push(hash);
                }
            }
            Self::promote_sequence(seq);
        }

        for key in eviction_keys_to_remove {
            inner.by_eviction_order.remove(&key);
        }

        let sender = seq_id.sender;
        for hash in &removed_hashes {
            inner.by_hash.remove(hash);
            if let Some(payer) = inner.payer_by_hash.remove(hash) {
                Self::decrement_counter(&mut inner.payer_txs, &payer, 1);
                if payer != sender {
                    Self::decrement_counter(&mut inner.sender_txs, &sender, 1);
                }
            }
        }

        if inner.sequences.get(seq_id).is_some_and(|s| s.txs.is_empty()) {
            inner.sequences.remove(seq_id);
            if let Some(slot) = inner.seq_to_slot.remove(seq_id) {
                inner.slot_to_seq.remove(&slot);
            }
        }

        removed_hashes
    }

    fn remove_from_inner(inner: &mut PoolInner<T>, hash: &B256) -> Option<Eip8130TxId>
    where
        T: PoolTransaction,
    {
        let id = inner.by_hash.remove(hash)?;
        let seq_id = id.sequence_id();
        let entry = inner.sequences.get_mut(&seq_id)?.txs.remove(&id.nonce_sequence)?;

        inner.by_eviction_order.remove(&EvictionKey {
            priority_fee: entry.priority_fee,
            submission_id: entry.submission_id,
            hash: *hash,
        });

        // Demote descendants — removing this tx creates a nonce gap.
        if let Some(seq) = inner.sequences.get_mut(&seq_id) {
            Self::demote_from_nonce(seq, id.nonce_sequence);
        }

        let sender = id.sender;
        if let Some(payer) = inner.payer_by_hash.remove(hash) {
            Self::decrement_counter(&mut inner.payer_txs, &payer, 1);
            if payer != sender {
                Self::decrement_counter(&mut inner.sender_txs, &sender, 1);
            }
        } else {
            Self::decrement_counter(&mut inner.sender_txs, &sender, 1);
        }

        if inner.sequences.get(&seq_id).is_some_and(|s| s.txs.is_empty()) {
            inner.sequences.remove(&seq_id);
            if let Some(slot) = inner.seq_to_slot.remove(&seq_id) {
                inner.slot_to_seq.remove(&slot);
            }
        }
        Some(id)
    }

    /// Decrements a counter map entry, removing it when it reaches zero.
    fn decrement_counter(map: &mut HashMap<Address, usize>, account: &Address, n: usize) {
        use std::collections::hash_map::Entry;
        if let Entry::Occupied(mut entry) = map.entry(*account) {
            let count = entry.get_mut();
            *count = count.saturating_sub(n);
            if *count == 0 {
                entry.remove();
            }
        }
    }

    /// Evicts the single lowest-priority transaction from the pool.
    fn try_evict_one(inner: &mut PoolInner<T>) -> bool
    where
        T: PoolTransaction,
    {
        // First pass: find the lowest-priority queued tx.
        let queued_hash = inner
            .by_eviction_order
            .iter()
            .find(|key| {
                if let Some(id) = inner.by_hash.get(&key.hash) {
                    let seq_id = id.sequence_id();
                    inner
                        .sequences
                        .get(&seq_id)
                        .and_then(|s| s.txs.get(&id.nonce_sequence))
                        .is_some_and(|e| !e.is_pending)
                } else {
                    false
                }
            })
            .map(|key| key.hash);

        // Fall back to lowest-priority pending tx if no queued txs exist.
        let hash_to_evict =
            queued_hash.or_else(|| inner.by_eviction_order.iter().next().map(|key| key.hash));

        if let Some(hash) = hash_to_evict {
            Self::remove_from_inner(inner, &hash);
            true
        } else {
            false
        }
    }

    /// Walks the sequence forward from `next_nonce` and sets `is_pending`
    /// for every entry in a contiguous run, then `false` for anything
    /// after the first gap.
    fn promote_sequence(seq: &mut SequenceState<T>) {
        let mut next = seq.next_nonce;
        let mut hit_gap = false;
        for (&nonce, entry) in seq.txs.iter_mut() {
            if !hit_gap && nonce == next {
                entry.is_pending = true;
                next += 1;
            } else {
                hit_gap = true;
                entry.is_pending = false;
            }
        }
    }

    /// Marks all entries with `nonce_sequence > removed_nonce` as queued.
    fn demote_from_nonce(seq: &mut SequenceState<T>, removed_nonce: u64) {
        for (_, entry) in seq
            .txs
            .range_mut((std::ops::Bound::Excluded(removed_nonce), std::ops::Bound::Unbounded))
        {
            entry.is_pending = false;
        }
    }

    /// Returns `true` if `new_fee` satisfies the price bump over `old_fee`.
    fn meets_price_bump(old_fee: u128, new_fee: u128, bump_pct: u64) -> bool {
        let min_new = old_fee.saturating_mul(100 + bump_pct as u128) / 100;
        new_fee >= min_new
    }
}

impl<T: PoolTransaction> Eip8130Pool<T> {
    /// Attempts to add a validated transaction to the pool.
    ///
    /// If a transaction already exists at the same `(sender, nonce_key,
    /// nonce_sequence)`, the new transaction replaces it **only** if its
    /// `max_priority_fee_per_gas` meets the configured price bump.
    ///
    /// **Counting rules:** a self-pay transaction (payer == sender) increments
    /// the payer counter only. A sponsored transaction increments the sender's
    /// sender counter and the payer's payer counter.
    #[allow(clippy::too_many_arguments)]
    pub fn add_transaction(
        &self,
        id: Eip8130TxId,
        transaction: T,
        payer: Address,
        origin: TransactionOrigin,
        nonce_storage_slot: B256,
        expiry: u64,
        check_tier: &dyn Fn(Address) -> TierCheckResult,
    ) -> Result<AddOutcome, Eip8130PoolError> {
        let hash = *transaction.hash();
        let priority_fee = transaction.max_priority_fee_per_gas().unwrap_or_default();
        let mut inner = self.inner.write();

        if inner.by_hash.contains_key(&hash) {
            return Err(Eip8130PoolError::DuplicateHash(hash));
        }

        let sender = id.sender;
        let is_self_pay = payer == sender;
        let seq_id = id.sequence_id();

        // --- Replacement check ---
        // Use `get` (not `entry().or_default()`) to avoid creating phantom
        // empty SequenceState entries when the check results in an error.
        let replaced_hash: Option<B256> = if let Some(seq) = inner.sequences.get(&seq_id) {
            if let Some(existing) = seq.txs.get(&id.nonce_sequence) {
                if !Self::meets_price_bump(
                    existing.priority_fee,
                    priority_fee,
                    self.config.price_bump_percent,
                ) {
                    return Err(Eip8130PoolError::ReplacementUnderpriced {
                        existing_fee: existing.priority_fee,
                        new_fee: priority_fee,
                    });
                }
                Some(*existing.transaction.hash())
            } else {
                None
            }
        } else {
            None
        };

        // Remove the replaced tx (if any) before capacity checks.
        if let Some(old_hash) = replaced_hash {
            Self::remove_from_inner(&mut inner, &old_hash);
        }

        // --- Capacity / account limit checks (only for new insertions) ---
        if replaced_hash.is_none() {
            if inner.by_hash.len() >= self.config.max_pool_size && !Self::try_evict_one(&mut inner)
            {
                return Err(Eip8130PoolError::PoolFull);
            }

            if !is_self_pay {
                let sender_count = inner.sender_txs.get(&sender).copied().unwrap_or(0);
                if sender_count >= self.config.default_max_sender_txs {
                    let tier = self.resolve_tier(&mut inner, sender, check_tier);
                    if sender_count >= self.config.max_sender_txs_for_tier(tier) {
                        return Err(Eip8130PoolError::AccountCapacityExceeded(sender));
                    }
                }
            }

            let payer_count = inner.payer_txs.get(&payer).copied().unwrap_or(0);
            if payer_count >= self.config.default_max_payer_txs {
                let tier = self.resolve_tier(&mut inner, payer, check_tier);
                if payer_count >= self.config.max_payer_txs_for_tier(tier) {
                    return Err(Eip8130PoolError::AccountCapacityExceeded(payer));
                }
            }

            let seq = inner.sequences.entry(seq_id.clone()).or_default();
            if seq.txs.len() >= self.config.max_txs_per_sequence {
                return Err(Eip8130PoolError::SequenceFull);
            }
        }

        // --- Insert ---
        let sub_id = inner.next_submission_id;
        inner.next_submission_id += 1;

        let entry = PooledEntry {
            id: id.clone(),
            transaction,
            origin,
            timestamp: Instant::now(),
            is_pending: false,
            submission_id: sub_id,
            priority_fee,
            expiry,
        };

        let seq = inner.sequences.entry(seq_id.clone()).or_default();
        seq.txs.insert(id.nonce_sequence, entry);

        // Promotion scan.
        let pending_before: HashSet<u64> = seq
            .txs
            .iter()
            .filter_map(|(n, e)| if e.is_pending { Some(*n) } else { None })
            .collect();
        Self::promote_sequence(seq);
        let newly_pending: Vec<B256> = seq
            .txs
            .iter()
            .filter(|(n, e)| e.is_pending && !pending_before.contains(n))
            .map(|(_, e)| *e.transaction.hash())
            .collect();

        inner.by_hash.insert(hash, id);
        inner.payer_by_hash.insert(hash, payer);
        inner.slot_to_seq.entry(nonce_storage_slot).or_insert_with(|| seq_id.clone());
        inner.seq_to_slot.entry(seq_id).or_insert(nonce_storage_slot);
        inner.by_eviction_order.insert(EvictionKey { priority_fee, submission_id: sub_id, hash });

        // Always increment counters — `remove_from_inner` already decremented
        // the old tx's counters for replacements, so we need to re-add for the
        // new tx to maintain the invariant: every live tx contributes exactly
        // one payer count and, if sponsored, one sender count.
        *inner.payer_txs.entry(payer).or_insert(0) += 1;
        if !is_self_pay {
            *inner.sender_txs.entry(sender).or_insert(0) += 1;
        }

        // Notify P2P listeners about newly pending transactions.
        for pending_hash in &newly_pending {
            let _ = self.pending_tx_sender.send(*pending_hash);
        }

        Ok(if replaced_hash.is_some() { AddOutcome::Replaced } else { AddOutcome::Added })
    }

    /// Resolves the throughput tier for `account`, using the cache when fresh.
    fn resolve_tier(
        &self,
        inner: &mut PoolInner<T>,
        account: Address,
        check_tier: &dyn Fn(Address) -> TierCheckResult,
    ) -> ThroughputTier {
        let now = Instant::now();
        if let Some(cached) = inner.account_tiers.get(&account)
            && now < cached.expires_at
        {
            return cached.tier;
        }
        let result = check_tier(account);
        let ttl = result
            .cache_for
            .map_or(self.config.tier_cache_ttl, |d| d.min(self.config.tier_cache_ttl));
        inner
            .account_tiers
            .insert(account, CachedTier { tier: result.tier, expires_at: now + ttl });
        result.tier
    }

    /// Returns the validated pool transaction for the given hash, if present.
    pub fn get(&self, hash: &B256) -> Option<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        let id = inner.by_hash.get(hash)?;
        let seq_id = id.sequence_id();
        let entry = inner.sequences.get(&seq_id)?.txs.get(&id.nonce_sequence)?;
        Some(Self::wrap_entry(entry))
    }

    /// Returns `(pending, queued)` transaction counts.
    pub fn pending_and_queued_count(&self) -> (usize, usize) {
        let inner = self.inner.read();
        let mut pending = 0;
        let mut queued = 0;
        for seq in inner.sequences.values() {
            for entry in seq.txs.values() {
                if entry.is_pending {
                    pending += 1;
                } else {
                    queued += 1;
                }
            }
        }
        (pending, queued)
    }

    /// Returns how many pool transactions list `account` as the sender.
    pub fn sender_tx_count(&self, account: &Address) -> usize {
        self.inner.read().sender_txs.get(account).copied().unwrap_or(0)
    }

    /// Returns how many pool transactions list `account` as the payer.
    pub fn payer_tx_count(&self, account: &Address) -> usize {
        self.inner.read().payer_txs.get(account).copied().unwrap_or(0)
    }

    /// Returns all transactions from a specific sender across all nonce lanes.
    pub fn get_transactions_by_sender(&self, sender: &Address) -> Vec<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        inner
            .sequences
            .iter()
            .filter(|(seq_id, _)| &seq_id.sender == sender)
            .flat_map(|(_, state)| state.txs.values().map(|e| Self::wrap_entry(e)))
            .collect()
    }

    /// Returns all pending (ready) transactions.
    pub fn pending_transactions(&self) -> Vec<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        inner
            .sequences
            .values()
            .flat_map(|seq| seq.txs.values().filter(|e| e.is_pending).map(|e| Self::wrap_entry(e)))
            .collect()
    }

    /// Returns all queued (not yet ready) transactions.
    pub fn queued_transactions(&self) -> Vec<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        inner
            .sequences
            .values()
            .flat_map(|seq| seq.txs.values().filter(|e| !e.is_pending).map(|e| Self::wrap_entry(e)))
            .collect()
    }

    /// Returns all validated transactions in the pool.
    pub fn all_transactions(&self) -> Vec<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        inner
            .sequences
            .values()
            .flat_map(|seq| seq.txs.values().map(|e| Self::wrap_entry(e)))
            .collect()
    }

    /// Wraps a pool entry in a `ValidPoolTransaction` for external consumption.
    ///
    /// The `SenderId` is derived by XOR-folding the 20-byte sender address
    /// into a `u64`, which reduces collision risk versus a simple truncation.
    fn wrap_entry(entry: &PooledEntry<T>) -> Arc<ValidPoolTransaction<T>>
    where
        T: Clone,
    {
        let addr = entry.id.sender.as_slice();
        // XOR-fold 20 bytes into 8 bytes for a collision-resistant u64.
        let mut buf = [0u8; 8];
        for (i, &b) in addr.iter().enumerate() {
            buf[i % 8] ^= b;
        }
        let sender_id_val = u64::from_be_bytes(buf);
        Arc::new(ValidPoolTransaction {
            transaction: entry.transaction.clone(),
            transaction_id: TransactionId::new(
                SenderId::from(sender_id_val),
                entry.id.nonce_sequence,
            ),
            propagate: true,
            timestamp: entry.timestamp,
            origin: entry.origin,
            authority_ids: None,
        })
    }
}

impl<T: PoolTransaction + Clone> Eip8130Pool<T> {
    /// Snapshots the ready (executable) transactions across all sequences.
    ///
    /// Returns a priority-ordered iterator that respects intra-lane nonce
    /// ordering: only the head (lowest pending nonce) of each lane is
    /// eligible at any time. After a head is consumed, its successor in the
    /// same lane is promoted into the heap.
    pub fn best_transactions(&self) -> BestEip8130Transactions<T> {
        let inner = self.inner.read();
        let mut heap = BinaryHeap::new();
        let mut successors: HashMap<Eip8130SequenceId, Vec<Arc<ValidPoolTransaction<T>>>> =
            HashMap::new();
        let mut hash_to_lane: HashMap<B256, Eip8130SequenceId> = HashMap::new();

        for (seq_id, seq) in &inner.sequences {
            let pending: Vec<_> = seq
                .txs
                .iter()
                .filter(|(_, e)| e.is_pending)
                .map(|(_, e)| Self::wrap_entry(e))
                .collect();

            // Register all pending txs for this lane in the hash→lane index.
            for tx in &pending {
                hash_to_lane.insert(*tx.hash(), seq_id.clone());
            }

            if let Some((head, rest)) = pending.split_first() {
                let prio = head.transaction.max_priority_fee_per_gas().unwrap_or_default();
                heap.push(PrioritizedTx {
                    priority_fee: prio,
                    lane: seq_id.clone(),
                    tx: head.clone(),
                });
                if !rest.is_empty() {
                    successors.insert(seq_id.clone(), rest.to_vec());
                }
            }
        }

        BestEip8130Transactions { heap, successors, invalid_lanes: HashSet::new(), hash_to_lane }
    }
}

/// Shared handle to an [`Eip8130Pool`].
pub type SharedEip8130Pool<T> = Arc<Eip8130Pool<T>>;

// ── BestEip8130Transactions ──────────────────────────────────────────

/// Wrapper for priority-ordered heap entries, carrying the lane identity.
struct PrioritizedTx<T: PoolTransaction> {
    priority_fee: u128,
    lane: Eip8130SequenceId,
    tx: Arc<ValidPoolTransaction<T>>,
}

impl<T: PoolTransaction> PartialEq for PrioritizedTx<T> {
    fn eq(&self, other: &Self) -> bool {
        self.priority_fee == other.priority_fee && self.tx.hash() == other.tx.hash()
    }
}

impl<T: PoolTransaction> Eq for PrioritizedTx<T> {}

impl<T: PoolTransaction> PartialOrd for PrioritizedTx<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: PoolTransaction> Ord for PrioritizedTx<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority_fee.cmp(&other.priority_fee).then_with(|| self.tx.hash().cmp(other.tx.hash()))
    }
}

/// Iterator over ready 2D-nonce transactions, sorted by priority.
///
/// Respects intra-lane nonce ordering: only the head (lowest pending nonce)
/// of each sequence lane is in the heap. When a head is consumed, its
/// successor in the same lane is promoted. This ensures the block builder
/// receives transactions in valid execution order within each lane.
///
/// `mark_invalid` invalidates at the lane level `(sender, nonce_key)`,
/// not at the sender level, preserving independent lane execution.
pub struct BestEip8130Transactions<T: PoolTransaction> {
    heap: BinaryHeap<PrioritizedTx<T>>,
    /// Remaining pending txs per lane, in nonce order (index 0 = next successor).
    successors: HashMap<Eip8130SequenceId, Vec<Arc<ValidPoolTransaction<T>>>>,
    /// Lanes that have been marked invalid — `(sender, nonce_key)`.
    invalid_lanes: HashSet<Eip8130SequenceId>,
    /// Reverse map: tx hash → lane identity, for `mark_invalid` lookups.
    hash_to_lane: HashMap<B256, Eip8130SequenceId>,
}

impl<T: PoolTransaction> Iterator for BestEip8130Transactions<T> {
    type Item = Arc<ValidPoolTransaction<T>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = self.heap.pop()?;
            let tx = entry.tx;
            let lane = entry.lane;

            if self.invalid_lanes.contains(&lane) {
                // Drop all successors for this invalidated lane.
                self.successors.remove(&lane);
                continue;
            }

            // Promote the next successor from this lane, if any.
            if let Some(remaining) = self.successors.get_mut(&lane) {
                if !remaining.is_empty() {
                    let next = remaining.remove(0);
                    let prio = next.transaction.max_priority_fee_per_gas().unwrap_or_default();
                    self.heap.push(PrioritizedTx {
                        priority_fee: prio,
                        lane: lane.clone(),
                        tx: next,
                    });
                }
                if remaining.is_empty() {
                    self.successors.remove(&lane);
                }
            }

            return Some(tx);
        }
    }
}

impl<T: PoolTransaction> reth_transaction_pool::BestTransactions for BestEip8130Transactions<T> {
    fn mark_invalid(&mut self, transaction: &Self::Item, _kind: &InvalidPoolTransactionError) {
        let hash = *transaction.hash();
        if let Some(lane) = self.hash_to_lane.get(&hash) {
            self.invalid_lanes.insert(lane.clone());
        }
    }

    fn no_updates(&mut self) {}

    fn skip_blobs(&mut self) {}

    fn set_skip_blobs(&mut self, _skip: bool) {}
}

// ── Eip8130PoolError ─────────────────────────────────────────────────

/// Errors returned by [`Eip8130Pool::add_transaction`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum Eip8130PoolError {
    /// Transaction hash already exists in the pool.
    #[error("duplicate transaction hash {0}")]
    DuplicateHash(B256),
    /// The sequence lane `(sender, nonce_key)` has too many transactions.
    #[error("sequence lane is full")]
    SequenceFull,
    /// A transaction at the same 2D nonce exists and the replacement does
    /// not meet the minimum price bump.
    #[error("replacement underpriced: existing_fee={existing_fee}, new_fee={new_fee}")]
    ReplacementUnderpriced {
        /// The priority fee of the existing transaction.
        existing_fee: u128,
        /// The priority fee of the proposed replacement.
        new_fee: u128,
    },
    /// Pool has reached its maximum capacity.
    #[error("2D nonce pool is full")]
    PoolFull,
    /// Account (sender or payer) already has the maximum number of transactions.
    #[error("account {0} exceeded per-account capacity")]
    AccountCapacityExceeded(Address),
}

impl reth_transaction_pool::error::PoolTransactionError for Eip8130PoolError {
    fn is_bad_transaction(&self) -> bool {
        false
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use alloy_consensus::Transaction;
    use alloy_primitives::{Address, U256};
    use reth_transaction_pool::PoolTransaction;

    use super::*;
    use crate::test_utils::{default_tier_result, make_id, make_slot, make_test_tx};

    type TestPool = Eip8130Pool<crate::test_utils::MockTransaction>;

    #[test]
    fn empty_pool_properties() {
        let pool = TestPool::new();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
        assert_eq!(pool.pending_and_queued_count(), (0, 0));
        assert!(pool.all_hashes().is_empty());
    }

    #[test]
    fn add_single_transaction() {
        let pool = TestPool::new();
        let id = make_id(0x01, 1, 0);
        let tx = make_test_tx(0x01, 0, 10);
        let hash = *tx.hash();
        let slot = make_slot(0x01, 1);

        let result = pool.add_transaction(
            id,
            tx,
            Address::repeat_byte(0x01),
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        );
        assert_eq!(result.unwrap(), AddOutcome::Added);
        assert_eq!(pool.len(), 1);
        assert!(pool.contains(&hash));
    }

    #[test]
    fn add_duplicate_hash_rejected() {
        let pool = TestPool::new();
        let tx = make_test_tx(0x01, 0, 10);
        let id = make_id(0x01, 1, 0);
        let slot = make_slot(0x01, 1);
        let sender = Address::repeat_byte(0x01);

        pool.add_transaction(
            id.clone(),
            tx.clone(),
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();
        let result = pool.add_transaction(
            id,
            tx,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        );
        assert!(matches!(result, Err(Eip8130PoolError::DuplicateHash(_))));
    }

    #[test]
    fn replacement_underpriced_rejected() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);
        let sender = Address::repeat_byte(0x01);

        let tx1 = make_test_tx(0x01, 0, 100);
        let id1 = make_id(0x01, 1, 0);
        pool.add_transaction(
            id1,
            tx1,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        // 105 < 110 required (10% bump of 100)
        let tx2 = make_test_tx(0x01, 100, 105);
        let id2 = make_id(0x01, 1, 0);
        let result = pool.add_transaction(
            id2,
            tx2,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        );
        assert!(matches!(result, Err(Eip8130PoolError::ReplacementUnderpriced { .. })));
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn replacement_with_sufficient_bump_succeeds() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);
        let sender = Address::repeat_byte(0x01);

        let tx1 = make_test_tx(0x01, 0, 100);
        let hash1 = *tx1.hash();
        let id1 = make_id(0x01, 1, 0);
        pool.add_transaction(
            id1,
            tx1,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        let tx2 = make_test_tx(0x01, 100, 110);
        let hash2 = *tx2.hash();
        let id2 = make_id(0x01, 1, 0);
        let result = pool
            .add_transaction(
                id2,
                tx2,
                sender,
                TransactionOrigin::External,
                slot,
                0,
                &default_tier_result,
            )
            .unwrap();
        assert_eq!(result, AddOutcome::Replaced);
        assert_eq!(pool.len(), 1);
        assert!(!pool.contains(&hash1));
        assert!(pool.contains(&hash2));
    }

    #[test]
    fn nonce_ordering_within_lane() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);
        let sender = Address::repeat_byte(0x01);

        // Add nonces 0, 1, 2 — all should become pending (contiguous from 0)
        for seq in 0..3 {
            let tx = make_test_tx(0x01, seq, 10);
            let id = make_id(0x01, 1, seq);
            pool.add_transaction(
                id,
                tx,
                sender,
                TransactionOrigin::External,
                slot,
                0,
                &default_tier_result,
            )
            .unwrap();
        }

        let (pending, queued) = pool.pending_and_queued_count();
        assert_eq!(pending, 3);
        assert_eq!(queued, 0);
    }

    #[test]
    fn nonce_gap_creates_queued() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);
        let sender = Address::repeat_byte(0x01);

        // Add nonces 0 and 2 (skip 1) — nonce 2 should be queued
        let tx0 = make_test_tx(0x01, 0, 10);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx0,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        let tx2 = make_test_tx(0x01, 2, 10);
        pool.add_transaction(
            make_id(0x01, 1, 2),
            tx2,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        let (pending, queued) = pool.pending_and_queued_count();
        assert_eq!(pending, 1);
        assert_eq!(queued, 1);
    }

    #[test]
    fn filling_gap_promotes_queued() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);
        let sender = Address::repeat_byte(0x01);

        // Add 0 and 2, then fill 1 — all should become pending
        let tx0 = make_test_tx(0x01, 0, 10);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx0,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        let tx2 = make_test_tx(0x01, 2, 10);
        pool.add_transaction(
            make_id(0x01, 1, 2),
            tx2,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        let tx1 = make_test_tx(0x01, 1, 10);
        pool.add_transaction(
            make_id(0x01, 1, 1),
            tx1,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        let (pending, queued) = pool.pending_and_queued_count();
        assert_eq!(pending, 3);
        assert_eq!(queued, 0);
    }

    #[test]
    fn multiple_nonce_keys_independent() {
        let pool = TestPool::new();
        let sender = Address::repeat_byte(0x01);

        // Add to key=1 and key=2 — independent lanes
        let tx_a = make_test_tx(0x01, 0, 10);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx_a,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 1),
            0,
            &default_tier_result,
        )
        .unwrap();

        let tx_b = make_test_tx(0x01, 1, 20);
        pool.add_transaction(
            make_id(0x01, 2, 0),
            tx_b,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 2),
            0,
            &default_tier_result,
        )
        .unwrap();

        assert_eq!(pool.len(), 2);
        let (pending, queued) = pool.pending_and_queued_count();
        assert_eq!(pending, 2);
        assert_eq!(queued, 0);
    }

    #[test]
    fn eviction_by_lowest_priority() {
        let config = Eip8130PoolConfig { max_pool_size: 2, ..Default::default() };
        let pool = Eip8130Pool::<crate::test_utils::MockTransaction>::with_config(config);
        let sender = Address::repeat_byte(0x01);

        // Fill pool with 2 txs
        let tx_low = make_test_tx(0x01, 0, 5);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx_low,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 1),
            0,
            &default_tier_result,
        )
        .unwrap();

        let tx_high = make_test_tx(0x01, 1, 50);
        pool.add_transaction(
            make_id(0x01, 1, 1),
            tx_high,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 1),
            0,
            &default_tier_result,
        )
        .unwrap();

        // Adding a 3rd tx should evict the lowest priority (queued first, then by fee)
        let tx_mid = make_test_tx(0x01, 2, 30);
        pool.add_transaction(
            make_id(0x01, 2, 0),
            tx_mid,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 2),
            0,
            &default_tier_result,
        )
        .unwrap();

        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn sweep_expired_removes_expired_txs() {
        let pool = TestPool::new();
        let sender = Address::repeat_byte(0x01);

        let tx1 = make_test_tx(0x01, 0, 10);
        let hash1 = *tx1.hash();
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx1,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 1),
            100,
            &default_tier_result,
        )
        .unwrap();

        let tx2 = make_test_tx(0x01, 1, 20);
        let hash2 = *tx2.hash();
        pool.add_transaction(
            make_id(0x01, 2, 0),
            tx2,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 2),
            0,
            &default_tier_result,
        )
        .unwrap();

        // Sweep at timestamp 101 — tx1 (expiry=100) should be removed
        let removed = pool.sweep_expired(101);
        assert_eq!(removed.len(), 1);
        assert!(removed.contains(&hash1));
        assert!(pool.contains(&hash2));
    }

    #[test]
    fn update_sequence_nonce_prunes_stale() {
        let pool = TestPool::new();
        let sender = Address::repeat_byte(0x01);
        let slot = make_slot(0x01, 1);

        for seq in 0..3 {
            let tx = make_test_tx(0x01, seq, 10);
            pool.add_transaction(
                make_id(0x01, 1, seq),
                tx,
                sender,
                TransactionOrigin::External,
                slot,
                0,
                &default_tier_result,
            )
            .unwrap();
        }
        assert_eq!(pool.len(), 3);

        let seq_id = Eip8130SequenceId { sender, nonce_key: U256::from(1) };
        let removed = pool.update_sequence_nonce(&seq_id, 2);
        assert_eq!(removed.len(), 2);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn sequence_full_rejected() {
        let config = Eip8130PoolConfig {
            max_txs_per_sequence: 2,
            trusted_max_payer_txs: 128,
            ..Default::default()
        };
        let pool = Eip8130Pool::<crate::test_utils::MockTransaction>::with_config(config);
        let sender = Address::repeat_byte(0x01);
        let slot = make_slot(0x01, 1);

        for seq in 0..2 {
            let tx = make_test_tx(0x01, seq, 10);
            pool.add_transaction(
                make_id(0x01, 1, seq),
                tx,
                sender,
                TransactionOrigin::External,
                slot,
                0,
                &|_| TierCheckResult {
                    tier: ThroughputTier::LockedTrustedBytecode,
                    cache_for: None,
                },
            )
            .unwrap();
        }

        let tx = make_test_tx(0x01, 2, 10);
        let result = pool.add_transaction(
            make_id(0x01, 1, 2),
            tx,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &|_| TierCheckResult { tier: ThroughputTier::LockedTrustedBytecode, cache_for: None },
        );
        assert!(matches!(result, Err(Eip8130PoolError::SequenceFull)));
    }

    #[test]
    fn pool_full_triggers_eviction() {
        let config = Eip8130PoolConfig { max_pool_size: 1, ..Default::default() };
        let pool = Eip8130Pool::<crate::test_utils::MockTransaction>::with_config(config);
        let sender = Address::repeat_byte(0x01);

        let tx1 = make_test_tx(0x01, 0, 10);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx1,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 1),
            0,
            &default_tier_result,
        )
        .unwrap();

        // Pool is full (size=1), adding another should evict the existing one
        let tx2 = make_test_tx(0x01, 1, 20);
        pool.add_transaction(
            make_id(0x01, 2, 0),
            tx2,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 2),
            0,
            &default_tier_result,
        )
        .unwrap();

        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn best_transactions_sorted_by_priority() {
        let pool = TestPool::new();
        let sender = Address::repeat_byte(0x01);

        let tx_low = make_test_tx(0x01, 0, 5);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx_low,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 1),
            0,
            &default_tier_result,
        )
        .unwrap();

        let tx_high = make_test_tx(0x01, 1, 50);
        pool.add_transaction(
            make_id(0x01, 2, 0),
            tx_high,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 2),
            0,
            &default_tier_result,
        )
        .unwrap();

        let best: Vec<_> = pool.best_transactions().collect();
        assert_eq!(best.len(), 2);
        let prio0 = best[0].transaction.max_priority_fee_per_gas().unwrap_or_default();
        let prio1 = best[1].transaction.max_priority_fee_per_gas().unwrap_or_default();
        assert!(prio0 >= prio1, "expected descending priority order");
    }

    #[test]
    fn remove_transaction_cleans_up() {
        let pool = TestPool::new();
        let sender = Address::repeat_byte(0x01);
        let tx = make_test_tx(0x01, 0, 10);
        let hash = *tx.hash();
        let slot = make_slot(0x01, 1);

        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();
        assert_eq!(pool.len(), 1);

        pool.remove_transaction(&hash);
        assert_eq!(pool.len(), 0);
        assert!(!pool.contains(&hash));
    }

    #[test]
    fn replacement_preserves_payer_counter() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);
        let sender = Address::repeat_byte(0x01);

        let tx1 = make_test_tx(0x01, 0, 100);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx1,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();
        assert_eq!(pool.payer_tx_count(&sender), 1);

        // Replace with sufficient bump.
        let tx2 = make_test_tx(0x01, 100, 110);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx2,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();
        // Counter must still be 1 after replacement, not 0.
        assert_eq!(pool.payer_tx_count(&sender), 1);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn sponsored_tx_tracks_sender_and_payer_counters() {
        let pool = TestPool::new();
        let sender = Address::repeat_byte(0x01);
        let payer = Address::repeat_byte(0x02);
        let slot = make_slot(0x01, 1);

        let tx = make_test_tx(0x01, 0, 10);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx,
            payer,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        // Sponsored: payer counter incremented, sender counter also incremented.
        assert_eq!(pool.payer_tx_count(&payer), 1);
        assert_eq!(pool.sender_tx_count(&sender), 1);
        // Payer should NOT have a sender count.
        assert_eq!(pool.sender_tx_count(&payer), 0);
    }

    #[test]
    fn best_transactions_preserves_intra_lane_order() {
        let pool = TestPool::new();
        let sender = Address::repeat_byte(0x01);
        let slot = make_slot(0x01, 1);

        // Add nonces 0, 1, 2 in a single lane with different fees.
        // Nonce 0 has LOW fee, nonce 1 has HIGH fee, nonce 2 has MID fee.
        let tx0 = make_test_tx(0x01, 0, 5);
        let hash0 = *tx0.hash();
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx0,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        let tx1 = make_test_tx(0x01, 1, 100);
        let hash1 = *tx1.hash();
        pool.add_transaction(
            make_id(0x01, 1, 1),
            tx1,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        let tx2 = make_test_tx(0x01, 2, 50);
        let hash2 = *tx2.hash();
        pool.add_transaction(
            make_id(0x01, 1, 2),
            tx2,
            sender,
            TransactionOrigin::External,
            slot,
            0,
            &default_tier_result,
        )
        .unwrap();

        // All three are pending (contiguous from 0).
        let (pending, queued) = pool.pending_and_queued_count();
        assert_eq!(pending, 3);
        assert_eq!(queued, 0);

        // best_transactions must return them in nonce order within the lane.
        let best: Vec<_> = pool.best_transactions().collect();
        assert_eq!(best.len(), 3);
        assert_eq!(*best[0].hash(), hash0, "first must be nonce 0");
        assert_eq!(*best[1].hash(), hash1, "second must be nonce 1");
        assert_eq!(*best[2].hash(), hash2, "third must be nonce 2");
    }

    #[test]
    fn best_transactions_cross_lane_priority_interleaving() {
        let pool = TestPool::new();
        let sender = Address::repeat_byte(0x01);

        // Lane 1: nonce 0 with fee 10
        let tx_a = make_test_tx(0x01, 0, 10);
        pool.add_transaction(
            make_id(0x01, 1, 0),
            tx_a,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 1),
            0,
            &default_tier_result,
        )
        .unwrap();

        // Lane 2: nonce 0 with fee 50
        let tx_b = make_test_tx(0x01, 1, 50);
        pool.add_transaction(
            make_id(0x01, 2, 0),
            tx_b,
            sender,
            TransactionOrigin::External,
            make_slot(0x01, 2),
            0,
            &default_tier_result,
        )
        .unwrap();

        let best: Vec<_> = pool.best_transactions().collect();
        assert_eq!(best.len(), 2);
        // Lane 2's head (fee=50) should come before Lane 1's head (fee=10).
        let prio0 = best[0].transaction.max_priority_fee_per_gas().unwrap_or_default();
        let prio1 = best[1].transaction.max_priority_fee_per_gas().unwrap_or_default();
        assert!(prio0 >= prio1, "cross-lane: higher priority first");
    }
}
