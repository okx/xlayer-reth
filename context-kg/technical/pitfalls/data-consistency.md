# Pitfalls: Data Consistency

## 1. Transaction Cache Prefix Mismatch

**Symptom**: Stale or incorrect pending block state after flashblock updates.

**Root Cause**: The `TransactionCache` uses prefix matching — if incoming transactions do NOT strictly extend the cached prefix (e.g., a reorg replaced earlier transactions), the cache produces wrong results.

**Rule**: `matching_prefix_len()` compares transaction hashes one-by-one from index 0. If any hash differs, the cache is invalidated from that point. The `get_resumable_state()` method returns `None` when the prefix doesn't match.

**Guard**: Always check the return value of `get_resumable_state()`. A `None` means full re-execution is required.

## 2. Canonical Anchor Hash vs Block Hash

**Symptom**: State lookups fail or return wrong data during speculative builds.

**Root Cause**: Speculative builds create pending blocks whose hash is NOT in the state database. The `canonical_anchor_hash` — the hash of the last canonical block the state is rooted in — must be used for state DB lookups.

**Rule**: Never use `pending_block.hash()` for state database queries during speculative builds. Always use `canonical_anchor_hash` from `PendingFlashBlock` or `PendingBlockState`.

## 3. Legacy RPC Response Merging

**Symptom**: Duplicate or unordered logs in `eth_getLogs` responses.

**Root Cause**: Hybrid `eth_getLogs` queries split the block range at the cutoff. If the cutoff block itself has logs, both the legacy and local responses may include them.

**Rule**: `merge_eth_get_logs_responses()` deduplicates by sorting on `(blockNumber, logIndex)`. However, block hash mismatches between legacy and local chains (due to different genesis) could cause subtle issues if not handled.

## 4. Genesis Generation State Snapshot

**Symptom**: Inconsistent genesis file if the node is running during export.

**Root Cause**: `gen-genesis` holds a long-lived read transaction. If the node processes blocks concurrently, the account state being read may be from different points in time (MDBX MVCC gives a consistent snapshot, but new writes expand the DB file since freed pages can't be reclaimed).

**Rule**: Always stop the node before running `gen-genesis`. The tool logs a warning about this.

## 5. Migration Receipt Gap

**Symptom**: Missing receipts after legacy migration.

**Root Cause**: If receipt pruning is active (`prune_modes.receipts` or `prune_modes.receipts_log_filter`), the MDBX receipt table may be incomplete. Migrating it to static files would create gaps.

**Rule**: The migration checks `can_migrate_receipts` and skips receipt migration if pruning is active. V2 storage settings are NOT written in this case, keeping the node on v1 routing.

## 6. Sequence Manager Ring Buffer Eviction

**Symptom**: Build job completes but its sequence was evicted from the ring buffer.

**Root Cause**: The `SequenceManager` has capacity 3. If flashblock production is faster than build jobs complete, old sequences get evicted.

**Rule**: `BuildTicket` includes a sequence identifier. When applying build results, the manager validates the ticket matches a current sequence. Mismatches are logged and the result is discarded.
