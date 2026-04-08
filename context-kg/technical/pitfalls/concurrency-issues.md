---
name: "concurrency-issues"
description: "Concurrency pitfalls: mutex locking, race conditions, task scheduling, channel backpressure"
---
# Pitfalls: Concurrency Issues

## 1. Engine Validator Mutex and Blocking Lock

**Symptom**: Deadlock or concurrent state-root computation between CL/EL sync and flashblocks stream.

**Root Cause**: Two sources (CL/EL sync and incoming flashblocks stream) can race during payload validation. Both attempt concurrent state-root computation.

**Mechanism**: `XLayerEngineValidator` uses `blocking_lock()` on `Mutex<XLayerEngineValidatorInner>` within engine tree's dedicated OS thread. Both `execute_sequence()` and engine validation hold the mutex for full duration.

**Rule**: The `blocking_lock()` is correct but requires strict adherence to calling from single dedicated thread only. SAFETY comments in `engine.rs` document this constraint.

## 2. Flashblocks Initialization Race Window

**Symptom**: Panic or incorrect state during node startup.

**Root Cause**: During startup, `set_flashblocks()` uses `block_in_place()` to lock from async context. `get_changeset_cache()` and `get_payload_processor()` also use `block_in_place()`. Initialization must complete atomically before validation begins.

**Rule**: `engine_validator` OnceLock in `main.rs` must be set by payload builder before `FlashblocksRpcService.spawn_rpc()` is called. This ordering is enforced by the initialization sequence in `main.rs`.

## 3. Channel Backpressure

**Symptom**: Flashblock events are lost or delayed.

**Channels used**:
- `tokio::sync::watch` for pending sequence state (latest-value semantics — old values overwritten)
- `tokio::sync::broadcast` for flashblock messages (bounded — slow receivers get `Lagged`)
- `tokio::sync::mpsc` for canonical block notifications (bounded — senders block on full)
- `std::sync::mpsc::sync_channel` for scheduler signals (blocking)

**Rule**:
- Watch channels are safe from backpressure (always latest value).
- Broadcast receivers MUST handle `RecvError::Lagged` by logging and continuing.
- MPSC senders should use `send().await` and handle `SendError` (receiver dropped).
- WebSocket broadcast channel capacity=100; if subscribers lag, all new publish attempts fail.

## 4. WebSocket Publisher Subscriber Limits

**Symptom**: New WebSocket connections rejected with CloseCode::Again.

**Root Cause**: `WebSocketPublisher` has a configurable max subscriber limit. Exceeding it sends CloseFrame and rejects connection before broadcast_loop spawns.

**Rule**: Monitor subscriber count metrics. The publisher logs warnings when approaching limits.

## 5. State Root Computation Fallback Race

**Symptom**: Two state root computations complete for the same flashblock.

**Root Cause**: If configured timeout elapses during concurrent state root computation, sequential fallback is spawned. Both computations may complete and send results to a unified channel.

**Mechanism**: Whichever finishes first is used; other result is discarded. Three strategies (Synchronous, Parallel, StateRootTask) have different code paths. Parallel and Synchronous use incremental suffix execution while StateRootTask requires full re-execution.

**Rule**: This is by design — the race is intentional for timeout resilience. No action needed but be aware when debugging state root timing.

## 6. RPC Middleware Ordering

**Symptom**: Monitor misses transactions that get routed to legacy, or legacy routing sees monitor-modified requests.

**Root Cause**: Tower middleware executes in stack order. The middleware tuple `(RpcMonitorLayer, LegacyRpcRouterLayer)` means monitor runs first (outer), then legacy routing (inner).

**Current behavior**: Monitor intercepts `eth_sendRawTransaction` before legacy routing. Since `eth_sendRawTransaction` is never routed to legacy, this ordering is correct.

**Rule**: If adding new middleware, consider the execution order carefully.

## 7. P2P Broadcast Concurrent Send

**Symptom**: Slowest peer blocks WebSocket publish.

**Root Cause**: `broadcast_message()` sends to all peers concurrently via FuturesUnordered but awaits all completions before WebSocket publish.

**Mechanism**: Failed peers are evicted and retry queued. But a slow peer's TCP send delays WebSocket notification.

**Rule**: Trade-off between consistency (all peers sent before subscribers notified) and latency. Consider time-bound sends if latency critical.

## 8. Message Broadcast Ignores Send Errors

**Symptom**: Peers or subscribers miss flashblocks during node shutdown.

**Root Cause**: Payload handler ignores send errors: `let _ = tx.send(...)` with comment "ignore send error (broadcast node may have shut down)".

**Rule**: Silently dropped messages during shutdown is by design. Be aware when debugging missing flashblocks.

## 9. Concurrent Flashblocks and Engine Validation

**Symptom**: Cache hash mismatch when flashblocks is enabled.

**Root Cause**: Execution cache keyed to flashblocks-built hash (not canonical hash) during parent state pre-warm causes cache miss on next block.

**Mechanism**: Re-keying cache to canonical hash via `on_inserted_executed_block()` is self-healing for subsequent blocks but causes transient performance loss.

**Rule**: Transient cache misses after flashblock builds are expected. Performance will recover on next block.
