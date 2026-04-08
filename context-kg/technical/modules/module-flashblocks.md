# Module: xlayer_flashblocks

**Path**: `crates/flashblocks/`
**Purpose**: Flashblock orchestration, consensus integration, caching, and subscription services

## Sub-modules

### Core Types
- **`payload.rs`**: `FlashBlock` (alias for `OpFlashblockPayload`), `PendingFlashBlock<N>` (pending block with flashblock metadata including `canonical_anchor_hash`, `last_flashblock_index`, `computed_state_root`)
- **`sequence.rs`**: `FlashBlockPendingSequence` (mutable BTree collection), `FlashBlockCompleteSequence` (immutable validated sequence), `SequenceExecutionOutcome` (block_hash + state_root)

### Service Layer
- **`service.rs`**: `FlashBlockService<N, S, EvmConfig, Provider>` — Main event loop orchestrator
  - Handles: job completion, flashblock reception, canonical block reconciliation
  - Features: speculative building via `PendingStateRegistry`, transaction caching, state epoch invalidation
  - `FlashBlockBuildInfo`: Tracks current build progress
  - `FlashBlockServiceMetrics`: Histograms, gauges, counters for service-level observability

### Worker
- **`worker.rs`**: `FlashBlockBuilder<EvmConfig, Provider>` — Executes flashblock sequences to build pending blocks
  - `BuildArgs<I, N>`: Input (base payload, transactions, optional cached state)
  - `BuildResult<N>`: Output (pending_flashblock, cached_reads, pending_state)
  - Supports both canonical and speculative execution modes
  - Cache-resume: skips already-cached prefix transactions using `CachedPrefixExecutionResult`

### State Management
- **`pending_state.rs`**: `PendingStateRegistry<N>` — LRU cache (max 64) of recently built pending block states for speculative chaining
- **`cache.rs`**: `SequenceManager<T>` — Ring buffer (size 3) tracking completed sequences
  - `SequenceId`: Stable identity (block_number, payload_id, parent_hash)
  - `BuildTicket`: Opaque build target identifier
  - `next_buildable_args()`: Selects which sequence to build based on canonical tip
  - `process_canonical_block()`: Reconciliation with reorg detection
- **`tx_cache.rs`**: `TransactionCache<N>` — Per-block execution state cache
  - Prefix matching for cache reuse across flashblock increments
  - Parent-hash-scoped for speculative builds

### Consensus
- **`consensus.rs`**: `FlashBlockConsensusClient<P>` — Submits completed sequences to engine API
  - `submit_new_payload()`: Sends `engine_newPayload` if state_root available
  - `submit_forkchoice_update()`: Always sends `engine_forkChoiceUpdated`
  - Falls back to parent_hash when state_root is zero

### Validation
- **`validation.rs`**: Stateless validation utilities
  - `FlashblockSequenceValidator`: Validates flashblock ordering (NextInSequence, FirstOfNextBlock, Duplicate, etc.)
  - `ReorgDetector`: Compares block fingerprints to detect chain reorgs
  - `CanonicalBlockReconciler`: Determines reconciliation strategy (CatchUp, HandleReorg, Continue, etc.)

### WebSocket / Streaming
- **`ws/stream.rs`**: `WsFlashBlockStream<Stream, Sink, Connector>` — Async stream with auto-reconnection
- **`ws/decoding.rs`**: `FlashBlockDecoder` — Supports JSON and brotli-compressed flashblock decoding

### Subscriptions
- **`subscription.rs`**: `FlashblocksPubSub<Eth, N>` — Extends standard ETH pubsub
  - Custom `flashblocks` subscription kind
  - Address-based filtering with configurable max addresses
  - Optional transaction data and receipt enrichment
  - LRU tx hash cache to avoid duplicate events
- **`handler.rs`**: `FlashblocksService<Node>` — Publishes received flashblocks to WebSocket subscribers with optional disk persistence

### Testing
- **`test_utils.rs`**: `TestFlashBlockFactory` / `TestFlashBlockBuilder` — Fluent API for creating test flashblocks

## Key Invariants

1. **Sequence ordering**: Index 0 is base; subsequent indices are sequential. New blocks start at index 0 with a new payload ID.
2. **Ring buffer capacity**: Max 3 sequences in `SequenceManager` — oldest evicted on overflow.
3. **Epoch invalidation**: Build results from a stale epoch are discarded (reorg detected).
4. **Prefix matching**: Transaction cache reuse requires the incoming tx list to be a strict extension of the cached prefix.
5. **Canonical anchor**: Speculative builds use `canonical_anchor_hash` for state DB lookups, not the pending parent hash.
