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
       ├── Select transactions from pool (BestFlashblocksTxs)
       ├── Execute transactions via EVM (FlashblocksBuilderCtx)
       ├── Check bridge intercept (intercept_bridge_transaction_if_need)
       │   └── If intercepted: skip transaction, continue
       ├── Inject builder transactions (tip, number contract)
       ├── Assemble flashblock payload
       │
       ▼
4. FlashblocksPayloadHandler receives built payload
       │
       ├── Broadcast via P2P (libp2p streams)
       ├── Broadcast via WebSocket publisher
       └── Update metrics
       │
       ▼
5. After block time elapses:
       ├── Finalize block with state root
       └── FlashBlockConsensusClient submits:
           ├── engine_newPayload (if state root available)
           └── engine_forkChoiceUpdated
```

### Flashblock Timing

- Block time: configurable (e.g., 2 seconds)
- Flashblock interval: configurable (e.g., 200ms)
- `flashblocks_per_block()` = block_time / flashblock_interval
- First flashblock (index 0) is the "base" — contains L1 info and deposit transactions
- Subsequent flashblocks (index 1..N) contain user transactions

## Follower/RPC Side (Service)

```
1. Receive flashblocks via WebSocket stream (WsFlashBlockStream)
       │
       ▼
2. FlashBlockService event loop:
       │
       ├── Validate sequence ordering (FlashblockSequenceValidator)
       ├── Accumulate into FlashBlockPendingSequence
       ├── On sequence complete: finalize → FlashBlockCompleteSequence
       │
       ▼
3. SequenceManager determines build strategy:
       │
       ├── next_buildable_args() selects target sequence
       ├── Issue BuildTicket
       │
       ▼
4. FlashBlockBuilder executes:
       │
       ├── Check TransactionCache for resumable prefix
       ├── Execute remaining transactions
       ├── Build PendingFlashBlock with canonical_anchor_hash
       ├── Store in PendingStateRegistry (for speculative chaining)
       │
       ▼
5. FlashBlockConsensusClient submits to engine API
       │
       ▼
6. On canonical block arrival:
       ├── CanonicalBlockReconciler determines strategy
       ├── Increment state epoch (invalidates stale builds)
       └── Clean up completed sequences
```

## Subscription Flow (PubSub)

```
Client subscribes: {"method": "eth_subscribe", "params": ["flashblocks", {filter}]}
       │
       ▼
FlashblocksPubSub creates subscription:
       │
       ├── Watch pending_block_rx for new PendingFlashBlocks
       ├── Convert to FlashblockStreamEvent (Header or Transaction)
       ├── Apply address filter (if configured)
       ├── Optionally enrich with full tx data and receipts
       └── Stream events to subscriber via WebSocket
```

## Key Invariants

1. Flashblock indices are strictly sequential within a block (0, 1, 2, ...)
2. New blocks always start at index 0 with a new payload ID
3. Transaction cache reuse requires strict prefix matching
4. Speculative builds use canonical_anchor_hash for state lookups
5. State epoch invalidation discards stale build results after reorgs
