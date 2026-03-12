# `bin/node` Architecture & Optimization Guide

This document describes how `bin/node` is structured, where each concern lives, and how to evolve the code toward a cleaner, more testable architecture. It includes concrete step-by-step examples for porting the existing RPC code.

---

## Table of Contents

1. [File Responsibilities](#file-responsibilities)
2. [Startup Sequence](#startup-sequence)
3. [RPC Middleware Stack](#rpc-middleware-stack)
4. [Optimization Recommendations](#optimization-recommendations)
5. [RPC Porting Examples](#rpc-porting-examples)

---

## File Responsibilities

| File | Role |
|------|------|
| `main.rs` | Entry point: wires CLI args → node builder → launch |
| `args.rs` | All `clap`-derived CLI flag structs (`XLayerArgs`, `LegacyRpcArgs`) |
| `payload.rs` | `XLayerPayloadServiceBuilder` — selects flashblocks vs. default block builder |

### Dependency graph

```
main.rs
├── args.rs           (XLayerArgs, LegacyRpcArgs)
├── payload.rs        (XLayerPayloadServiceBuilder)
└── external crates
    ├── xlayer-monitor     (RpcMonitorLayer, XLayerMonitor)
    ├── xlayer-legacy-rpc  (LegacyRpcRouterLayer, LegacyRpcRouterConfig)
    ├── xlayer-rpc         (XlayerRpcExt, XlayerRpcExtApiServer)
    └── xlayer-flashblocks (FlashblocksService, FlashblocksPubSub)
```

---

## Startup Sequence

```
main()
  │
  ├─ 1. init_version!, sigsegv_handler, RUST_BACKTRACE
  ├─ 2. XLayerArgs::validate_init_command()          ← guards `init` sub-command
  │
  └─ Cli::parse().run(|builder, args| async {
       ├─ 3. args.xlayer_args.validate()             ← validates URLs, timeouts
       ├─ 4. init_global_tracer()  (if monitor on)  ← OpenTelemetry setup
       ├─ 5. Build LegacyRpcRouterConfig
       ├─ 6. Build XLayerMonitor
       ├─ 7. op_node.add_ons().with_rpc_middleware(  ← attach Tower layers
       │        RpcMonitorLayer,
       │        LegacyRpcRouterLayer
       │      )
       ├─ 8. XLayerPayloadServiceBuilder::new()
       │
       └─ builder
            .with_types_and_provider()
            .with_components(…payload…)
            .with_add_ons(add_ons)
            .extend_rpc_modules(|ctx| {              ← register JSON-RPC handlers
                FlashblocksService / FlashblocksPubSub
                XlayerRpcExt
            })
            .launch_with_fn(|builder| {              ← pick launcher
                EngineNodeLauncher or DebugNodeLauncher
            })
            .await?
     })
```

---

## RPC Middleware Stack

Requests flow through Tower layers **before** they reach the jsonrpsee method handlers.
The order in `with_rpc_middleware` is execution order (first listed = outermost):

```
Incoming JSON-RPC request
        │
        ▼
┌──────────────────────┐
│   RpcMonitorLayer    │  ← intercepts eth_sendRawTransaction / eth_sendTransaction
│                      │    records tx hash for full-link monitor tracing
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ LegacyRpcRouterLayer │  ← checks block param; routes pre-genesis requests
│                      │    to the legacy Erigon endpoint
└──────────┬───────────┘
           │
           ▼
   jsonrpsee dispatch
   (XlayerRpcExt, FlashblocksPubSub, standard eth_ methods …)
```

**Why this order matters:** The monitor must see the *final* response (including the tx hash) that the client will receive. If `LegacyRpcRouterLayer` reroutes a write call, the monitor would record the wrong hash if it were placed after the legacy layer.

### How each layer is implemented

Both layers implement the `tower::Layer` + `jsonrpsee::RpcServiceT` traits:

```rust
// Pattern shared by RpcMonitorLayer and LegacyRpcRouterLayer
impl<S> Layer<S> for MyLayer {
    type Service = MyService<S>;
    fn layer(&self, inner: S) -> MyService<S> { … }
}

impl<S: RpcServiceT> RpcServiceT for MyService<S> {
    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + 'a {
        // inspect req, maybe short-circuit, otherwise delegate to self.inner.call(req)
    }
}
```

---

## Optimization Recommendations

### 1 — Keep `main.rs` as a pure wiring file

`main.rs` should read like a table of contents: create config objects, wire them together, and call `.await`. Business logic belongs in dedicated modules or crates.

### 2 — Extract `extend_rpc_modules` into `bin/node/src/rpc.rs`

The closure passed to `.extend_rpc_modules()` already spans ~40 lines and owns several conditional branches. Moving it to a standalone function makes it independently testable and keeps `main.rs` readable.

### 3 — Replace scattered `flashblocks.enabled` booleans with a `XLayerNodeMode` enum

```rust
pub enum XLayerNodeMode { Sequencer, Rpc }
```

A single enum value is easier to match exhaustively and prevents silent drift between flags.

### 4 — Move tracer initialization before `.run()`

The global tracer (`init_global_tracer`) is currently initialized inside the async closure. Moving it before `Cli::parse().run(…)` ensures every span—including CLI parsing—is captured.

### 5 — Use `eyre::Result<()>` in `main()`

Replacing `.unwrap()` with `?` propagation and `eyre::Result` gives structured error output on startup failure.

### 6 — Group flashblocks subscription flags into `FlashblocksSubscriptionArgs`

```rust
#[derive(Debug, Clone, Args, Default)]
pub struct FlashblocksSubscriptionArgs {
    pub enabled: bool,
    pub max_addresses: usize,
}
```

### 7 — Extract engine tree config into a helper

```rust
fn build_engine_tree_config(cfg: &NodeConfig) -> TreeConfig { … }
```

This makes it easy to unit-test different persistence/buffer combinations.

### 8 — Feature-flag the global allocator

```toml
[features]
jemalloc = ["reth-cli-util/jemalloc"]
```

```rust
#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: …
```

---

## RPC Porting Examples

The examples below show how to apply the recommendations above to the actual code in `bin/node/src/main.rs`.

---

### Example 1 — Extract `extend_rpc_modules` into `bin/node/src/rpc.rs`

**Before** (`main.rs`, inline closure, ~40 lines):

```rust
.extend_rpc_modules(move |ctx| {
    let new_op_eth_api = Arc::new(ctx.registry.eth_api().clone());

    if !args.xlayer_args.builder.flashblocks.enabled {
        if let Some(flashblock_rx) = new_op_eth_api.subscribe_received_flashblocks() {
            let service = FlashblocksService::new(
                ctx.node().clone(),
                flashblock_rx,
                args.xlayer_args.builder.flashblocks,
                args.rollup_args.flashblocks_url.is_some(),
                datadir,
            )?;
            service.spawn();
            info!(target: "reth::cli", "xlayer flashblocks service initialized");
        }

        if xlayer_args.enable_flashblocks_subscription
            && let Some(pending_blocks_rx) = new_op_eth_api.pending_block_rx()
        {
            let eth_pubsub = ctx.registry.eth_handlers().pubsub.clone();
            let flashblocks_pubsub = FlashblocksPubSub::new(
                eth_pubsub,
                pending_blocks_rx,
                Box::new(ctx.node().task_executor().clone()),
                new_op_eth_api.converter().clone(),
                xlayer_args.flashblocks_subscription_max_addresses,
            );
            ctx.modules.add_or_replace_if_module_configured(
                RethRpcModule::Eth,
                flashblocks_pubsub.into_rpc(),
            )?;
            info!(target: "reth::cli", "xlayer eth pubsub initialized");
        }
    }

    let xlayer_rpc = XlayerRpcExt { backend: new_op_eth_api };
    ctx.modules.merge_configured(XlayerRpcExtApiServer::<Optimism>::into_rpc(xlayer_rpc))?;
    info!(target: "reth::cli", "xlayer rpc extension enabled");

    Ok(())
})
```

**After** — create `bin/node/src/rpc.rs`:

```rust
// bin/node/src/rpc.rs

use std::sync::Arc;
use std::path::PathBuf;

use op_alloy_network::Optimism;
use reth::rpc::eth::EthApiTypes;
use reth_node_api::FullNodeComponents;
use reth_rpc_server_types::RethRpcModule;
use tracing::info;

use xlayer_flashblocks::{handler::FlashblocksService, subscription::FlashblocksPubSub};
use xlayer_rpc::xlayer_ext::{XlayerRpcExt, XlayerRpcExtApiServer};

use crate::args::XLayerArgs;

/// Registers all X Layer RPC modules onto `ctx.modules`.
///
/// Call this function from the `.extend_rpc_modules()` builder step.
pub fn register_xlayer_rpc<N>(
    ctx: &reth::builder::RpcContext<'_, N>,
    xlayer_args: &XLayerArgs,
    datadir: PathBuf,
    flashblocks_url_provided: bool,
) -> eyre::Result<()>
where
    N: FullNodeComponents,
    N::Provider: reth::rpc::eth::EthApiTypes,
{
    let eth_api = Arc::new(ctx.registry.eth_api().clone());

    register_flashblocks(ctx, &eth_api, xlayer_args, datadir, flashblocks_url_provided)?;
    register_xlayer_ext(ctx, eth_api)?;

    info!(message = "X Layer RPC modules initialized");
    Ok(())
}

fn register_flashblocks<N>(
    ctx: &reth::builder::RpcContext<'_, N>,
    eth_api: &Arc<N::EthApi>,
    xlayer_args: &XLayerArgs,
    datadir: PathBuf,
    flashblocks_url_provided: bool,
) -> eyre::Result<()>
where
    N: FullNodeComponents,
{
    // Flashblocks service runs on RPC nodes only (not on the sequencer).
    if xlayer_args.builder.flashblocks.enabled {
        return Ok(());
    }

    if let Some(flashblock_rx) = eth_api.subscribe_received_flashblocks() {
        let service = FlashblocksService::new(
            ctx.node().clone(),
            flashblock_rx,
            xlayer_args.builder.flashblocks.clone(),
            flashblocks_url_provided,
            datadir,
        )?;
        service.spawn();
        info!(target: "reth::cli", "xlayer flashblocks service initialized");
    }

    if xlayer_args.enable_flashblocks_subscription {
        if let Some(pending_blocks_rx) = eth_api.pending_block_rx() {
            let pubsub = FlashblocksPubSub::new(
                ctx.registry.eth_handlers().pubsub.clone(),
                pending_blocks_rx,
                Box::new(ctx.node().task_executor().clone()),
                eth_api.converter().clone(),
                xlayer_args.flashblocks_subscription_max_addresses,
            );
            ctx.modules.add_or_replace_if_module_configured(
                RethRpcModule::Eth,
                pubsub.into_rpc(),
            )?;
            info!(target: "reth::cli", "xlayer eth pubsub initialized");
        }
    }

    Ok(())
}

fn register_xlayer_ext<N>(
    ctx: &reth::builder::RpcContext<'_, N>,
    eth_api: Arc<N::EthApi>,
) -> eyre::Result<()>
where
    N: FullNodeComponents,
{
    let xlayer_rpc = XlayerRpcExt { backend: eth_api };
    ctx.modules.merge_configured(XlayerRpcExtApiServer::<Optimism>::into_rpc(xlayer_rpc))?;
    info!(target: "reth::cli", "xlayer rpc extension enabled");
    Ok(())
}
```

**Updated `main.rs`** (the closure shrinks to 4 lines):

```rust
// add `mod rpc;` at the top of main.rs
mod rpc;

// inside Cli::parse().run(…):
let datadir = builder.config().datadir().clone();
let xlayer_args_clone = args.xlayer_args.clone();
let flashblocks_url = args.rollup_args.flashblocks_url.is_some();

builder
    // … other builder steps …
    .extend_rpc_modules(move |ctx| {
        rpc::register_xlayer_rpc(ctx, &xlayer_args_clone, datadir, flashblocks_url)
    })
```

---

### Example 2 — Add a new custom RPC method

The `xlayer-rpc` crate owns the `XlayerRpcExtApi` trait. Adding a new method follows the same pattern as `eth_flashblocksEnabled`.

**Step 1 — Extend the trait** (`crates/rpc/src/xlayer_ext.rs`):

```rust
#[rpc(server, namespace = "eth", server_bounds(…))]
pub trait XlayerRpcExtApi<Net: RpcTypes> {
    #[method(name = "flashblocksEnabled")]
    async fn flashblocks_enabled(&self) -> RpcResult<bool>;

    // NEW: expose the sequencer URL if the node is in sequencer mode
    #[method(name = "sequencerEndpoint")]
    async fn sequencer_endpoint(&self) -> RpcResult<Option<String>>;
}
```

**Step 2 — Implement the new method** (`crates/rpc/src/xlayer_ext.rs`):

```rust
#[async_trait]
impl<T, Net> XlayerRpcExtApiServer<Net> for XlayerRpcExt<T>
where
    T: PendingFlashBlockProvider + SequencerClientProvider + Send + Sync + 'static,
    Net: RpcTypes + Send + Sync + 'static,
{
    async fn flashblocks_enabled(&self) -> RpcResult<bool> {
        Ok(self.backend.has_pending_flashblock())
    }

    async fn sequencer_endpoint(&self) -> RpcResult<Option<String>> {
        Ok(self.backend
            .sequencer_client()
            .map(|c| c.endpoint().to_string()))
    }
}
```

**Step 3 — No changes needed in `main.rs`**: `register_xlayer_ext` already calls `merge_configured`, so new methods are automatically included.

---

### Example 3 — Port the legacy RPC middleware to a standalone module

Currently `LegacyRpcRouterConfig` is built inline in `main.rs`. Move that logic into `bin/node/src/rpc.rs`:

**Before** (`main.rs`):

```rust
let legacy_config = LegacyRpcRouterConfig {
    enabled: xlayer_args.legacy.legacy_rpc_url.is_some(),
    legacy_endpoint: xlayer_args.legacy.legacy_rpc_url.unwrap_or_default(),
    cutoff_block: genesis_block,
    timeout: xlayer_args.legacy.legacy_rpc_timeout,
};
```

**After** — add a factory function to `rpc.rs`:

```rust
use xlayer_legacy_rpc::{LegacyRpcRouterConfig, LegacyRpcRouterLayer};
use crate::args::LegacyRpcArgs;

/// Builds the legacy RPC Tower layer from CLI args and genesis block number.
pub fn build_legacy_layer(legacy: &LegacyRpcArgs, cutoff_block: u64) -> LegacyRpcRouterLayer {
    let config = LegacyRpcRouterConfig {
        enabled: legacy.legacy_rpc_url.is_some(),
        legacy_endpoint: legacy.legacy_rpc_url.clone().unwrap_or_default(),
        cutoff_block,
        timeout: legacy.legacy_rpc_timeout,
    };
    LegacyRpcRouterLayer::new(config)
}
```

**Updated `main.rs`**:

```rust
let legacy_layer = rpc::build_legacy_layer(&xlayer_args.legacy, genesis_block);

let add_ons = op_node.add_ons().with_rpc_middleware((
    RpcMonitorLayer::new(monitor.clone()),
    legacy_layer,
));
```

---

### Example 4 — Writing a new Tower middleware layer

Use this template to add a new RPC middleware (e.g., rate limiting, request logging):

```rust
// crates/my-middleware/src/lib.rs

use jsonrpsee::{
    core::middleware::{Batch, Notification},
    server::middleware::rpc::RpcServiceT,
    types::Request,
    MethodResponse,
};
use std::future::Future;
use tower::Layer;

/// Configuration for the new middleware.
#[derive(Clone)]
pub struct MyMiddlewareConfig {
    pub enabled: bool,
    // … your fields
}

/// Tower Layer — produces a `MyMiddlewareService<S>` wrapping any inner service.
#[derive(Clone)]
pub struct MyMiddlewareLayer {
    config: MyMiddlewareConfig,
}

impl MyMiddlewareLayer {
    pub fn new(config: MyMiddlewareConfig) -> Self {
        Self { config }
    }
}

impl<S> Layer<S> for MyMiddlewareLayer {
    type Service = MyMiddlewareService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MyMiddlewareService { inner, config: self.config.clone() }
    }
}

/// The actual middleware service.
#[derive(Clone)]
pub struct MyMiddlewareService<S> {
    inner: S,
    config: MyMiddlewareConfig,
}

impl<S> RpcServiceT for MyMiddlewareService<S>
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        // Fast path: bypass if disabled
        if !self.config.enabled {
            return futures::future::Either::Left(self.inner.call(req));
        }

        let inner = self.inner.clone();
        futures::future::Either::Right(async move {
            // Pre-processing logic here
            let response = inner.call(req).await;
            // Post-processing logic here
            response
        })
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        self.inner.batch(req)
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}
```

**Register it in `main.rs`** (or in `rpc::build_middleware_stack`):

```rust
let add_ons = op_node.add_ons().with_rpc_middleware((
    RpcMonitorLayer::new(monitor.clone()),    // outermost
    MyMiddlewareLayer::new(my_config),        // middle
    LegacyRpcRouterLayer::new(legacy_config), // innermost
));
```

Layers are applied outermost → innermost in the order they are listed.

---

### Example 5 — Replace scattered booleans with `XLayerNodeMode`

This shows how recommendation 3 would change the flashblocks guard in the extracted `register_flashblocks` function.

**Define the enum** (e.g., in `bin/node/src/args.rs` or a new `bin/node/src/mode.rs`):

```rust
/// Describes the operational role of this X Layer node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XLayerNodeMode {
    /// Full sequencer: builds and seals blocks; runs flashblocks producer.
    Sequencer,
    /// RPC node: consumes blocks; may run flashblocks subscriber.
    Rpc,
}

impl XLayerNodeMode {
    pub fn from_args(xlayer_args: &XLayerArgs) -> Self {
        if xlayer_args.sequencer_mode {
            Self::Sequencer
        } else {
            Self::Rpc
        }
    }
}
```

**Use it in `rpc.rs`**:

```rust
pub fn register_xlayer_rpc<N>(
    ctx: &RpcContext<'_, N>,
    xlayer_args: &XLayerArgs,
    mode: XLayerNodeMode,
    datadir: PathBuf,
    flashblocks_url_provided: bool,
) -> eyre::Result<()>
where
    N: FullNodeComponents,
{
    let eth_api = Arc::new(ctx.registry.eth_api().clone());

    match mode {
        XLayerNodeMode::Rpc => {
            register_flashblocks(ctx, &eth_api, xlayer_args, datadir, flashblocks_url_provided)?;
        }
        XLayerNodeMode::Sequencer => {
            // Sequencer does not subscribe to flashblocks; it produces them.
        }
    }

    register_xlayer_ext(ctx, eth_api)?;
    Ok(())
}
```

**In `main.rs`**:

```rust
let mode = XLayerNodeMode::from_args(&args.xlayer_args);

builder
    .extend_rpc_modules(move |ctx| {
        rpc::register_xlayer_rpc(ctx, &xlayer_args_clone, mode, datadir, flashblocks_url)
    })
```

---

## Summary of the New `bin/node` Layout

After applying the examples above, `bin/node/src/` would look like:

```
bin/node/src/
├── main.rs        ← ~80 lines: wiring only
├── args.rs        ← CLI structs (XLayerArgs, LegacyRpcArgs, FlashblocksSubscriptionArgs)
├── mode.rs        ← XLayerNodeMode enum
├── payload.rs     ← XLayerPayloadServiceBuilder
└── rpc.rs         ← register_xlayer_rpc, build_legacy_layer, build_middleware_stack
```

Each file has a single clear responsibility, is independently importable, and (for `rpc.rs` and `mode.rs`) is unit-testable without spinning up a full node.
