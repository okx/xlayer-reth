# Pitfalls: Concurrency Issues

## 1. State Epoch Invalidation

**Symptom**: Build results applied to wrong chain state after a reorg.

**Root Cause**: Build jobs are spawned asynchronously. By the time a job completes, the canonical chain may have changed (reorg). Applying the build result would corrupt state.

**Mechanism**: The `FlashBlockService` maintains a `state_epoch` counter. When a canonical block arrives that triggers reconciliation, the epoch increments. Build jobs carry the epoch at spawn time. Results with stale epochs are discarded.

**Rule**: Always compare `build_result.epoch` against the current `state_epoch` before applying results.

## 2. Channel Backpressure

**Symptom**: Flashblock events are lost or delayed.

**Channels used**:
- `tokio::sync::watch` for pending block state (latest-value semantics — old values overwritten)
- `tokio::sync::broadcast` for flashblock sequences (bounded — slow receivers get `Lagged`)
- `tokio::sync::mpsc` for canonical block notifications (bounded — senders block on full)

**Rule**:
- Watch channels are safe from backpressure (always latest value).
- Broadcast receivers MUST handle `RecvError::Lagged` by logging and continuing.
- MPSC senders should use `send().await` and handle `SendError` (receiver dropped).

## 3. WebSocket Publisher Subscriber Limits

**Symptom**: New WebSocket connections rejected.

**Root Cause**: `WebSocketPublisher` has a configurable max subscriber limit. Exceeding it rejects new connections.

**Rule**: Monitor subscriber count metrics. The publisher logs warnings when approaching limits.

## 4. Parallel Static File Migration

**Symptom**: Data corruption if multiple segments write to overlapping block ranges.

**Root Cause**: Static file segments are migrated in parallel via rayon. Each segment has its own writer, but they share the same block range.

**Guard**: Each `StaticFileSegment` uses independent writers (`get_writer()` / `latest_writer()`) that operate on separate file paths. The parallelism is safe because segments don't share files.

## 5. RPC Middleware Ordering

**Symptom**: Monitor misses transactions that get routed to legacy, or legacy routing sees monitor-modified requests.

**Root Cause**: Tower middleware executes in stack order. The middleware tuple `(RpcMonitorLayer, LegacyRpcRouterLayer)` means monitor runs first (outer), then legacy routing (inner).

**Current behavior**: Monitor intercepts `eth_sendRawTransaction` before legacy routing. Since `eth_sendRawTransaction` is never routed to legacy (it's not in the routable methods list), this ordering is correct.

**Rule**: If adding new middleware, consider the execution order carefully. Outer middleware sees the request first and the response last.

## 6. Flashblock Consensus Client Race

**Symptom**: `engine_newPayload` submitted for a block that was already canonical.

**Root Cause**: The `FlashBlockConsensusClient` processes completed sequences asynchronously. A sequence might complete and trigger `submit_new_payload()` after the canonical chain already advanced past that block.

**Guard**: The engine API is idempotent for `newPayload` — submitting a known payload is a no-op. `forkChoiceUpdated` always advances to the latest known head.

## 7. PendingStateRegistry LRU Eviction Under Load

**Symptom**: Speculative build fails because parent state was evicted.

**Root Cause**: `PendingStateRegistry` has max 64 entries. Under high flashblock throughput with many concurrent builds, recently built states may be evicted before dependent builds consume them.

**Rule**: `get_state_for_parent()` returns `None` when evicted. The builder falls back to canonical-only execution (no speculative chaining).
