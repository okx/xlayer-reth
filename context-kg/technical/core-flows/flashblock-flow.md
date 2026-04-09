---
name: "flashblock-flow"
description: "Core flow: flashblocks building, distribution, ingestion, execution, and subscription"
---
# Core Flow: Flashblock Building and Distribution

## Overview

Flashblocks provide sub-block-time transaction confirmation. The sequencer incrementally builds "flashblocks" (partial payloads) during a block's build interval and distributes them to followers before the canonical block is finalized.

## Sequencer Side (Builder)

```
1. engine_forkChoiceUpdated (with PayloadAttributes)
       │
       ▼
2. BlockPayloadJobGenerator creates BlockPayloadJob
       │
       ▼
3. FlashblocksBuilder runs build loop:
       │
       ├── FlashblockScheduler calculates target_flashblocks and send times
       ├── Scheduler sends sync_channel signals at calculated times
       ├── Select transactions from pool (BestFlashblocksTxs)
       ├── Execute transactions via EVM (FlashblocksBuilderCtx)
       ├── Check bridge intercept (intercept_bridge_transaction_if_need)
       │   └── If intercepted: skip transaction, continue
       ├── Inject builder transactions (tip, number contract)
       ├── Build flashblock payload (OpFlashblockPayload with diff)
       │
       ▼
4. FlashblocksPayloadHandler receives built payload
       │
       ├── Wrap as XLayerFlashblockPayload (with target_index)
       ├── Broadcast via P2P (libp2p streams) — all peers concurrently
       ├── Broadcast via WebSocket publisher — after P2P completes
       ├── On external flashblock: execute and verify hash match
       └── Update metrics
       │
       ▼
5. After block time elapses or target flashblocks reached:
       ├── Send XLayerFlashblockEnd (payload_id) to signal sequence completion
       ├── Resolve best payload with state root
       └── Async state root computation if needed (fallback if zero)
```

### Flashblock Timing

- Block time: configurable (e.g., 2 seconds)
- Flashblock interval: configurable (e.g., 200ms)
- `flashblocks_per_block()` = block_time / flashblock_interval
- First flashblock (index 0) is the "base" — contains L1 info and deposit transactions
- Subsequent flashblocks (index 1..N) contain user transactions
- FlashblockScheduler accounts for late FCUs by using block_time as fallback duration

## Follower/RPC Side (Service)

```
1. Receive flashblocks via WebSocket stream (WsFlashBlockStream)
       │  Supports JSON (text) and brotli-compressed (binary) messages
       │
       ▼
2. handle_incoming_flashblocks():
       │
       ├── Batch process all immediately available messages
       ├── Route to RawFlashblocksCache (ring buffer, capacity 50)
       ├── Enqueue height to ExecutionTaskQueue
       ├── Broadcast to subscribers via broadcast channel
       │
       ▼
3. handle_execution_tasks():
       │
       ├── Poll ExecutionTaskQueue.next().await (BTreeSet ordered by height)
       ├── Fetch buildable args from RawFlashblocksCache
       ├── Call XLayerEngineValidator::execute_sequence()
       │   ├── FlashblockSequenceValidator builds PendingSequence
       │   ├── Check PrefixExecutionMeta for warm cache reuse
       │   ├── Execute remaining transactions (incremental or fresh)
       │   ├── Compute state root (3 strategies: StateRootTask/Parallel/Synchronous)
       │   └── Store result in FlashblockStateCache
       │
       ▼
4. FlashblockStateCache manages three-layer state:
       │
       ├── PendingSequence (single in-progress, height = confirm_height + 1)
       ├── ConfirmCache (BTreeMap of completed-but-ahead-of-canonical blocks)
       └── CanonicalInMemoryState (Reth's engine state)
       │
       ▼
5. handle_canonical_stream():
       │
       ├── Watch CanonStateNotificationStream for canonical block commits
       ├── On normal advancement: evict confirm_cache blocks <= canonical_height
       ├── On reorg or hash mismatch: full flush (pending=None, confirm cleared)
       └── Optional debug state comparison (spawned on blocking thread)
```

## Subscription Flow (PubSub)

```
Client subscribes: {"method": "eth_subscribe", "params": ["flashblocks", {filter}]}
       │
       ▼
FlashblocksPubSub creates subscription:
       │
       ├── Watch pending_sequence_rx (tokio::sync::watch) for new PendingSequence
       ├── Convert to FlashblockStreamEvent (Header, Transaction, Receipt)
       ├── Apply address filter (if configured, max 1000 addresses)
       ├── LRU txhash_cache (10000) prevents duplicate events
       └── Stream events to subscriber via WebSocket (pipe_from_flashblocks_stream)
```

## RPC Query Flow (Cache-Aware)

```
Client queries: eth_getBlockByNumber / eth_getTransactionByHash / eth_getLogs / etc.
       │
       ▼
FlashblocksEthApiOverride / FlashblocksFilterOverride:
       │
       ├── Check FlashblockStateCache first (pending → confirmed → canonical)
       ├── If found: convert to RPC response via helper functions
       └── If not found: delegate to standard Eth API provider
```

## Persistence Flow

```
handle_persistence():
       │
       ├── Watch broadcast channel for incoming flashblocks
       ├── Accumulate in FlashblockPayloadsCache
       └── Flush to disk every 5 seconds on dirty flag (atomic temp-file + rename)

handle_relay_flashblocks():
       │
       ├── Watch broadcast channel for incoming flashblocks
       └── Relay to downstream WebSocket subscribers via WebSocketPublisher
```

## Key Invariants

1. Flashblock indices are strictly sequential within a block (0, 1, 2, ...)
2. New blocks always start at index 0 with a new payload ID
3. XLayerFlashblockEnd signals sequence completion — no more flashblocks for current block
4. PendingSequence height must equal confirm_height + 1; gaps/mismatches cause rejection
5. Engine validation guarded by Mutex — prevents concurrent state-root computation races
6. Prefix execution caching enables warm starts — keyed by payload_id
7. Reorg detection triggers full cache flush and task queue drain
8. P2P broadcast to all peers completes before WebSocket publish (ordering guarantee)

## Exception Branches

| Scenario | State Change | Compensation |
|----------|-------------|-------------|
| Late FCU (timestamp in past) | FlashblockScheduler uses block_time as fallback | Reduced flashblocks logged |
| block_cancel mid-flashblock | resolve_best_payload called | Loop exits cleanly |
| Tx exceeds gas/DA limits | mark_invalid, skip tx | Continue to next tx |
| Hash mismatch on external flashblock | Payload rejected | Error logged, metrics incremented |
| Height gap (pending > confirm+1) | No state change | Error logged, sequence skipped |
| Reorg detected | Full cache flush | task_queue.flush(), confirm_height reset |
| State root timeout | Sequential fallback spawned | First result used, other discarded |
