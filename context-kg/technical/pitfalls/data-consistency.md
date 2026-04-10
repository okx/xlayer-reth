---
name: "data-consistency"
description: "Data consistency pitfalls: cache coherence, reorg handling, height gaps, state management"
---
# Pitfalls: Data Consistency

## 1. Height Continuity Validation

**Symptom**: Flashblock sequence rejected despite valid data.

**Root Cause**: If pending sequence exists at same height as incoming, mismatched `payload_id` causes rejection. If heights match and `sequence_end` not set but pending flashblock index >= incoming, early exit triggers.

**Rule**: Strict validation gates incremental vs fresh builds. Requires careful coordination of payload_id and flashblock indexing. Pending height must always equal confirm_height + 1.

## 2. Incremental Build Revert Flattening

**Symptom**: State inconsistency after incremental flashblock build.

**Root Cause**: For incremental builds at same height, bundle accumulates one revert entry per flashblock index. Flattening to single revert entry requires merging storage slots across transitions, keeping earliest per-slot values.

**Rule**: Complex revert merging logic with potential for state inconsistency if SELFDESTRUCT or wipe_storage flags conflict between transitions. Test thoroughly when modifying revert handling.

## 3. Pending Block Deadline Hardcoded

**Symptom**: Pending block expires too early or stays stale too long.

**Root Cause**: Pending block deadline set to 1 second (`Duration::from_secs(1)`) matching default blocktime without configurability.

**Rule**: If blocktime < 1 second, pending block expires prematurely. If > 1 second, stale blocks remain in cache longer. Be aware of this when changing block time configuration.

## 4. Legacy RPC Response Merging

**Symptom**: Duplicate or unordered logs in `eth_getLogs` responses.

**Root Cause**: Hybrid `eth_getLogs` queries split the block range at the cutoff. Log merge sorts only by block number; logs with identical block numbers from legacy and local responses may have inconsistent ordering.

**Rule**: `merge_eth_get_logs_responses()` deduplicates by sorting on `(blockNumber, logIndex)`. However, block hash mismatches between legacy and local chains could cause subtle issues.

## 5. Prefix Transaction Caching

**Symptom**: Transaction duplication or gaps in flashblock execution.

**Root Cause**: Cached transaction count from prefix execution (`cached_tx_count`) used to slice incoming transactions. Off-by-one errors in slicing would cause deduplication or gaps.

**Rule**: `PrefixExecutionMeta` caches by payload_id. New payload_id invalidates cache entirely. Always verify cached_tx_count matches actual executed count before slicing.

## 6. Flashblock Sequence Continuity

**Symptom**: Incomplete flashblock sequence accepted silently.

**Root Cause**: Pre-validation checks height and payload_id but assumes incoming flashblock index monotonically increases within same sequence. No explicit check that incoming_last_index > previous_last_index in incremental builds.

**Rule**: Sequential index without gaps is enforced by `get_flashblocks_sequence_txs()` which checks expected_index == payload.index, returning None on mismatch.

## 7. Non-Contiguous Overlay Blocks

**Symptom**: State provider creation fails for flashblock execution.

**Root Cause**: `get_executed_blocks_up_to_height()` requires all heights from canon_height+1 to target to be present in confirm cache. A gap causes error.

**Rule**: Cache validates with intermediate window check. If gap detected, returns error. Ensure canonical advancement doesn't create gaps in confirm cache.

## 8. Genesis Generation State Snapshot

**Symptom**: Inconsistent genesis file if the node is running during export.

**Root Cause**: `gen-genesis` holds a long-lived read transaction. Concurrent block processing doesn't affect snapshot consistency (MDBX MVCC) but prevents freed page reclamation.

**Rule**: Always stop the node before running `gen-genesis`.

## 9. Extra Data Format Validation

**Symptom**: Entire flashblock rejected due to single byte mismatch.

**Root Cause**: Jovian fork expects 17-byte extra_data, Holocene expects 9-byte, pre-Holocene expects 0-byte. Mismatch causes `bail!()`.

**Rule**: Strict validation with no recovery mechanism. Ensure extra_data format matches the active hardfork.

## 10. Monitor Flashblock Event Tracking

**Symptom**: Missing transaction execution events when flashblocks enabled.

**Root Cause**: `on_tx_commit()` only tracks transactions when flashblocks_disabled. When flashblocks enabled, no SeqTxExecutionEnd event logged for individual transactions.

**Rule**: Be aware of incomplete monitoring coverage in flashblocks mode. Block-level events are still tracked.
