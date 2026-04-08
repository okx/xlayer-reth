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

## 3. Event Loop Service Pattern

Long-running services with a `tokio::select!` event loop:

```rust
impl MyService {
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(event) = self.receiver.recv() => { ... }
                _ = self.shutdown.recv() => break,
            }
        }
    }
}
```

**Instances**: `FlashBlockService::run()`, `FlashBlockConsensusClient::run()`, `WebSocketPublisher`

## 4. Task Spawning Pattern

```rust
// Critical task — node shuts down on panic
ctx.task_executor.spawn_critical("task-name", Box::pin(async move { ... }));

// Background task — failure logged but non-fatal
ctx.task_executor.spawn("task-name", Box::pin(async move { ... }));
```

**Rule**: Payload building, consensus, and monitoring are critical. Metrics and WebSocket publishing are background.

## 5. Builder Context Pattern

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

## 6. Watch/Broadcast Channel Pattern

For state distribution between producer and consumers:

```rust
// Latest-value (watch) — pending block state
let (tx, rx) = tokio::sync::watch::channel(None);

// Fan-out (broadcast) — flashblock sequences
let (tx, _) = tokio::sync::broadcast::channel(capacity);

// Bounded MPSC — canonical block notifications
let (tx, rx) = tokio::sync::mpsc::channel(capacity);
```

**Convention**: Watch for "current state" (overwrite is fine), broadcast for "events" (all consumers see all events), MPSC for "commands" (single consumer).

## 7. Configuration Validation Pattern

```rust
impl MyArgs {
    pub fn validate(&self) -> Result<(), String> {
        // Check preconditions
        if self.enabled && self.required_field.is_none() {
            return Err("field is required when enabled".to_string());
        }
        // Validate formats
        if let Some(addr) = &self.address {
            addr.parse::<Address>().map_err(|_| format!("Invalid address: {addr}"))?;
        }
        Ok(())
    }
}
```

**Called in**: `XLayerArgs::validate()` chains all sub-arg validations. Called in `main.rs` before any configuration is used.

## 8. Ring Buffer / LRU Cache Pattern

For bounded-memory caching:

```rust
// Ring buffer (fixed size, oldest evicted)
SequenceManager<T> — capacity 3 for completed sequences

// LRU cache (configurable max, least-recently-used evicted)
PendingStateRegistry<N> — max 64 entries for pending block states
```

**Rule**: Always handle the "entry was evicted" case gracefully (return `None`, log, continue).
