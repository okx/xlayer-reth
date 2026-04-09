---
name: "module-flashblocks"
description: "Module design for xlayer_flashblocks: cache, execution, state, subscription, persistence"
---
# Module: xlayer_flashblocks

**Path**: `crates/flashblocks/`
**Purpose**: Flashblock RPC-side orchestration — cache management, sequence execution, subscription, persistence, and engine validation

## Sub-modules

### Cache Layer (`cache/`)
- **`mod.rs`**: `FlashblockStateCache<N>` — Top-level controller managing three data layers:
  - Pending layer (single in-progress sequence)
  - Confirmed layer (committed sequences ahead of canonical)
  - Canonical layer (Reth's `CanonicalInMemoryState`)
  - `CachedTxInfo`: Per-transaction cache entry (block context, receipt, tx data) for O(1) hash lookups
- **`pending.rs`**: `PendingSequence<N>` — Wraps `PendingBlock` with flashblock metadata and HashMap tx_index for O(1) lookups; at most one active at a time
- **`confirm.rs`**: `ConfirmCache<N>` — BTreeMap (block_number → block) + HashMap (hash → number); supports O(log n) range splits for efficient flush on canonical advancement
- **`raw.rs`**: `RawFlashblocksCache` — Ring buffer (MAX_RAW_CACHE_SIZE=50) accumulating raw incoming flashblocks; tracks next buildable sequence and detects reorgs
- **`task.rs`**: `ExecutionTaskQueue` — BTreeSet-backed async task queue with `Arc<Notify>` signaling; evicts lowest height on capacity overflow
- **`utils.rs`**: Helper utilities for cache operations

### Execution Pipeline (`execution/`)
- **`mod.rs`**: `BuildArgs`, `PrefixExecutionMeta`, `FlashblockReceipt` trait, `StateRootStrategy` enum (StateRootTask/Parallel/Synchronous), `OverlayProviderFactory` trait alias
- **`engine.rs`**: `XLayerEngineValidator` / `XLayerEngineValidatorBuilder` — Unified validator guarding against races between CL/EL sync and flashblocks stream; uses `blocking_lock()` Mutex for engine thread safety; shares PayloadProcessor and changeset cache between engine and flashblocks validators
- **`validator.rs`**: `FlashblockSequenceValidator<N, Provider>` — Builds PendingSequence from accumulated flashblock transactions; supports incremental execution with cached prefix (CachedReads + PrefixExecutionMeta); handles state root computation with 3 strategies and timeout fallback
- **`assemble.rs`**: Block assembly helpers for flashblock execution

### State Management (`state.rs`)
Three async handler functions replacing the old worker module:
1. `handle_incoming_flashblocks()` — Receives XLayerFlashblockMessage stream, routes to RawFlashblocksCache, enqueues to ExecutionTaskQueue, batches consecutive flashblocks
2. `handle_execution_tasks()` — Polls ExecutionTaskQueue, fetches buildable args from RawFlashblocksCache, calls XLayerEngineValidator::execute_sequence()
3. `handle_canonical_stream()` — Monitors canonical block notifications, detects reorgs and flushes cache, optional debug state comparison

### Service Layer (`service.rs`)
- `FlashblocksRpcService<N, Provider>` — Coordinates all flashblocks RPC components
  - Spawns: incoming flashblocks handler, execution tasks, canonical stream watcher, persistence, WebSocket relay
  - `FlashblocksRpcCtx`: canonical state stream + debug flag
  - `FlashblocksPersistCtx`: data directory path for on-disk flashblock persistence

### Persistence (`persist.rs`)
- `handle_persistence()` — Watches broadcast channel, flushes `FlashblockPayloadsCache` to disk every 5 seconds on dirty flag
- `handle_relay_flashblocks()` — Relays incoming flashblocks to downstream WebSocket subscribers

### Debug (`debug.rs`)
- `debug_compare_flashblocks_bundle_states()` — Spawns on blocking thread to compare flashblock-built vs engine-built state; deep field-level mismatch detection for accounts and reverts

### WebSocket / Streaming (`ws/`)
- **`stream.rs`**: `WsFlashBlockStream` — Async stream with auto-reconnection consuming `XLayerFlashblockMessage`
- **`decoding.rs`**: `FlashBlockDecoder` — Supports JSON (text) and brotli-compressed (binary) flashblock decoding

### Subscriptions (`subscription/`)
- **`rpc.rs`**: `FlashblocksPubSub<Eth, N>` — Extends standard ETH pubsub with `flashblocks` subscription kind; address filtering; LRU tx hash cache (MAX_TXHASH_CACHE_SIZE=10000) to avoid duplicates
- **`pubsub.rs`**: `FlashblocksPubSubInner` — Watches `pending_sequence_rx` for new PendingSequence changes; converts to FlashblockStreamEvent vector (Header + Transactions + Receipts)

### Memory Overlay (`state.rs` — `MemoryOverlayStateProvider`)
- Overlays in-memory changes on disk state for flashblock execution without disk writes

### Testing
- **`test_utils.rs`**: `TestFlashBlockFactory` — Fluent builder for test flashblock creation with chainable methods

## Key Invariants

1. **Three-layer cache**: Pending (single in-progress) → Confirmed (BTreeMap ahead of canonical) → Canonical (Reth engine state)
2. **Height continuity**: Pending height must equal confirm_height + 1; gaps cause rejection
3. **Sequence promotion**: When sequence_end=true, pending promotes to confirm cache and pending is cleared
4. **Reorg handling**: Hash mismatch or explicit reorg → full flush (pending=None, confirm cleared, confirm_height=canon_height, task_queue flushed)
5. **Engine validation guard**: XLayerEngineValidator holds Mutex preventing concurrent state-root computation between engine and flashblocks validators
6. **Prefix execution caching**: PrefixExecutionMeta enables warm execution cache reuse; keyed by payload_id — new payload_id invalidates cache
7. **State root fallback**: If configured timeout elapses during concurrent state root computation, sequential fallback spawned; first result used

## Dependencies
- Requires `xlayer_builder` for `XLayerFlashblockMessage`, `WebSocketPublisher`, `FlashblockPayloadsCache`
- Reference `arch/dependency.md` for full dependency details

## NOT Responsible For
- Building flashblock payloads (that's `xlayer_builder`)
- P2P broadcast to peers (that's `xlayer_builder::broadcast`)
- Bridge transaction interception
- Legacy RPC routing
