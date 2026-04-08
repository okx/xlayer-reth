---
name: "service-patterns"
description: "Service patterns: lock-protected state, async handlers, persistence, event loops"
---
# Conventions: Service Patterns

## 1. PayloadServiceBuilder Pattern

The core extension point for customizing block building. Implement `PayloadServiceBuilder<Node, Pool, EvmConfig>`:

```rust
impl<Node, Pool> PayloadServiceBuilder<Node, Pool, OpEvmConfig> for XLayerPayloadServiceBuilder
where
    Node: NodeBounds,
    Pool: PoolBounds,
{
    async fn spawn_payload_builder_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: OpEvmConfig,
    ) -> eyre::Result<PayloadBuilderHandle<...>> { ... }
}
```

**Usage**: `bin/node/src/payload.rs` delegates to flashblocks or standard builder.

## 2. Tower Middleware Layer Pattern

Used for RPC request interception:

```rust
// Layer creates the service
pub struct MyLayer { config: MyConfig }
impl<S> tower::Layer<S> for MyLayer {
    type Service = MyService<S>;
    fn layer(&self, inner: S) -> Self::Service { ... }
}

// Service wraps inner and intercepts calls
pub struct MyService<S> { inner: S, config: Arc<MyConfig> }
impl<S: RpcServiceT> RpcServiceT for MyService<S> {
    async fn call(&self, req: Request<'_>) -> MethodResponse { ... }
}
```

**Instances**: `LegacyRpcRouterLayer`, `RpcMonitorLayer`

**Middleware stacking**: `(OuterLayer, InnerLayer)` — outer executes first for requests, last for responses.

## 3. Async Handler Pattern (NEW — replaces Event Loop Service)

Independent async handler functions spawned as separate tasks:

```rust
// Three independent handlers for flashblocks RPC
handle_incoming_flashblocks(ws_stream, raw_cache, task_queue, broadcast_tx)
handle_execution_tasks(task_queue, raw_cache, engine_validator, state_cache)
handle_canonical_stream(canon_notifications, state_cache, raw_cache, task_queue)
```

**Benefit**: Each handler is independently testable and has clear input/output boundaries. Replaces the monolithic event loop pattern.

**Instances**: `FlashblocksRpcService` spawns 5 handlers: incoming, execution, canonical, persistence, relay.

## 4. Three-Layer Cache Pattern

For flashblock state management:

```rust
FlashblockStateCache<N> {
    pending_cache: Option<PendingSequence<N>>,     // Single in-progress
    confirm_cache: ConfirmCache<N>,                // BTreeMap(height → block)
    canonical: CanonicalInMemoryState<N>,          // Reth engine state
    confirm_height: u64,                           // Highest confirmed
}
```

**Query pattern**: Check pending → confirmed → canonical (via standard provider).
**Promotion**: sequence_end=true → pending promotes to confirm, pending cleared.
**Flush**: On reorg → clear pending, clear confirm, reset confirm_height.

## 5. Task Spawning Pattern

```rust
// Critical task — node shuts down on panic
ctx.task_executor.spawn_critical("task-name", Box::pin(async move { ... }));

// Background task — failure logged but non-fatal
ctx.task_executor.spawn("task-name", Box::pin(async move { ... }));
```

**Rule**: Payload building, consensus, and monitoring are critical. Metrics and WebSocket publishing are background.

## 6. Builder Context Pattern

Encapsulate EVM execution state in a context struct:

```rust
pub struct FlashblocksBuilderCtx<EvmConfig> {
    pub evm_config: EvmConfig,
    pub chain_spec: Arc<OpChainSpec>,
    pub initialized_cfg: CfgEnvWithHandlerCfg,
    pub initialized_block_env: BlockEnv,
    // ... hardfork flags, execution state
}
```

The context provides methods for transaction execution, receipt building, and hardfork checks.

## 7. Watch/Broadcast Channel Pattern

For state distribution between producer and consumers:

```rust
// Latest-value (watch) — pending sequence state
let (tx, rx) = tokio::sync::watch::channel(None);

// Fan-out (broadcast) — flashblock messages
let (tx, _) = tokio::sync::broadcast::channel(capacity);

// Bounded MPSC — canonical block notifications
let (tx, rx) = tokio::sync::mpsc::channel(capacity);

// Blocking sync channel — scheduler signals
let (tx, rx) = std::sync::mpsc::sync_channel(capacity);
```

**Convention**: Watch for "current state" (overwrite is fine), broadcast for "events" (all consumers see all events), MPSC for "commands" (single consumer), sync_channel for blocking scheduling triggers.

## 8. Atomic Persistence Pattern

For durable state storage:

```rust
// Write to temp file, then atomic rename
let tmp_path = dir.join(".flashblocks.tmp");
std::fs::write(&tmp_path, serialized)?;
std::fs::rename(&tmp_path, &final_path)?;
```

**Instance**: `FlashblockPayloadsCache` persistence — flushes every 5 seconds on dirty flag.

## 9. Configuration Validation Pattern

```rust
impl MyArgs {
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && self.required_field.is_none() {
            return Err("field is required when enabled".to_string());
        }
        Ok(())
    }
}
```

**Called in**: `XLayerArgs::validate()` chains all sub-arg validations. Called in `main.rs` before any configuration is used.

## 10. Mutex-Guarded Unified Validator Pattern

For preventing concurrent validation races:

```rust
pub struct XLayerEngineValidator {
    inner: Mutex<XLayerEngineValidatorInner>,
}
// Engine validation and flashblocks validation share the mutex
// blocking_lock() used from dedicated OS thread
```

**Rule**: Both engine validation and flashblocks sequence execution lock the same mutex. Ensures state-root computation is never concurrent.
