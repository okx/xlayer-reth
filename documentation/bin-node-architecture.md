# `bin/node` Architecture & Optimization Guide

## Overview

`bin/node` is the entry point for the X Layer Reth execution client. Its job is strictly **orchestration**: parse CLI arguments, validate configuration, wire together the crate-level building blocks (payload builder, RPC middleware, monitoring), and hand control to the Reth node launcher.

The binary intentionally stays thin. Every distinct concern — flashblocks, legacy RPC routing, monitoring, chain spec — lives in a dedicated crate under `crates/`. This mirrors the pattern used by projects such as [base/base](https://github.com/base/base/tree/main/bin/node), where `main.rs` is kept under ~200 lines and delegates all heavy logic to separate libraries.

```
bin/node/src/
├── main.rs      — Node wiring & launch sequence
├── args.rs      — CLI argument structs (XLayerArgs, LegacyRpcArgs)
└── payload.rs   — Payload builder strategy (flashblocks vs. default)
```

---

## Key Components

### 1. `main.rs` — The Orchestration Layer

`main.rs` performs the following steps in order:

| Step | What Happens |
|------|--------------|
| Version init | `xlayer_version::init_version!()` stamps build metadata |
| Signal handler | `sigsegv_handler::install()` captures segfault backtraces |
| Backtrace | `RUST_BACKTRACE=1` is set if not already present |
| Arg validation | `XLayerArgs::validate_init_command()` catches misuse of `init` subcommand |
| CLI parse + run | `Cli::<XLayerChainSpecParser, Args>::parse().run(…)` enters the async context |
| Config validation | `args.xlayer_args.validate()` checks URLs, timeouts, etc. |
| Tracer init | Global tracer initialized when full-link monitor is enabled |
| Node construction | `OpNode`, payload builder, RPC middleware layers assembled |
| RPC extension | Flashblocks subscription and `XlayerRpcExt` merged into module registry |
| Launch | `EngineNodeLauncher` (or `DebugNodeLauncher` in dev mode) launches the node |
| Monitor handle | `start_monitor_handle` attached to running node tasks |

### 2. `args.rs` — Argument Structs

`XLayerArgs` is a flat `clap::Args` struct that aggregates sub-argument groups via `#[command(flatten)]`:

```
XLayerArgs
├── BuilderArgs      (xlayer-builder) — flashblocks / sequencer config
├── LegacyRpcArgs    — legacy endpoint URL + timeout
├── FullLinkMonitorArgs (xlayer-monitor) — monitoring toggle & output path
├── enable_flashblocks_subscription — bool
├── flashblocks_subscription_max_addresses — usize
└── sequencer_mode   — bool
```

`LegacyRpcArgs` includes runtime validation (URL parse check, zero-timeout guard) exercised via `validate()`.

### 3. `payload.rs` — Payload Builder Strategy

`XLayerPayloadServiceBuilder` uses an internal enum to select the right strategy at startup:

```
XLayerPayloadServiceBuilderInner
├── Flashblocks(FlashblocksServiceBuilder)   — sequencer producing flashblocks
└── Default(BasicPayloadServiceBuilder)      — follower / RPC node
```

This strategy pattern cleanly separates sequencer from follower behaviour without scattering `if flashblocks.enabled` branches throughout the codebase.

### 4. RPC Middleware Stack

Two `Tower`-compatible layers are stacked onto the OP node's RPC server:

```
Incoming RPC request
      │
      ▼
RpcMonitorLayer      — records latency, method counts, full-link trace data
      │
      ▼
LegacyRpcRouterLayer — forwards pre-genesis requests to legacy endpoint
      │
      ▼
Standard Reth RPC handlers
```

Order matters: the monitor wraps the router so every request (including those proxied to legacy) is tracked.

---

## Architecture Optimization Recommendations

### 1. Keep `main.rs` as a Pure Wiring File

**Current state:** `main.rs` is ~210 lines and already well-scoped.

**Optimization:** As new features land, resist the temptation to embed initialization logic directly in `main.rs`. Instead, encapsulate each concern in its own function or module:

```rust
// Good — delegates to a builder function
let monitor = build_monitor(&xlayer_args);
let payload_builder = build_payload_service(&args)?;
let add_ons = build_add_ons(&op_node, monitor.clone(), legacy_config);
```

This keeps `main.rs` readable as a high-level flow diagram of the node startup sequence.

### 2. Extract the `extend_rpc_modules` Closure

The `extend_rpc_modules` closure in `main.rs` currently performs multiple responsibilities:

- Subscribes to flashblocks
- Initialises `FlashblocksService` and spawns it
- Configures `FlashblocksPubSub`
- Registers `XlayerRpcExt`

**Optimization:** Move this into a dedicated function (or a small struct) in `bin/node/src/rpc.rs`:

```rust
// bin/node/src/rpc.rs
pub fn register_xlayer_rpc_modules<Node>(
    ctx: RpcContext<'_, Node>,
    xlayer_args: &XLayerArgs,
    rollup_args: &RollupArgs,
    datadir: DataDirPath,
) -> eyre::Result<()> {
    // flashblocks service, pubsub, and xlayer ext registration
}
```

This makes unit testing the RPC registration logic straightforward and reduces the argument capture complexity of the closure.

### 3. Consolidate Conditional Logic Around `flashblocks.enabled`

The `flashblocks.enabled` flag is checked in multiple places:

- `payload.rs` (which builder strategy to use)
- `main.rs` inside `extend_rpc_modules` (whether to set up the service)
- `XLayerMonitor::new` receives `flashblocks.enabled` as a plain bool

**Optimization:** Introduce a node-mode enum to replace scattered boolean checks:

```rust
pub enum XLayerNodeMode {
    FlashblocksSequencer,
    FlashblocksFollower,
    Standard,
}

impl XLayerNodeMode {
    pub fn from_args(args: &XLayerArgs) -> Self {
        match (args.sequencer_mode, args.builder.flashblocks.enabled) {
            (true, true)  => Self::FlashblocksSequencer,
            (false, true) => Self::FlashblocksFollower,
            _             => Self::Standard,
        }
    }
}
```

This makes every conditional branch on mode explicit and self-documenting.

### 4. Lazy / Conditional Tracer Initialization

The global tracer is currently initialised with a `PathBuf` allocation and a log message inside the async closure, after argument parsing:

```rust
if args.xlayer_args.monitor.enable {
    let output_path = PathBuf::from(&args.xlayer_args.monitor.output_path);
    init_global_tracer(true, Some(output_path));
}
```

**Optimization:** Move tracer initialization to before `.run()` (i.e., synchronously in `main()`) so that any spans emitted during CLI parsing or early async setup are also captured:

```rust
fn main() {
    xlayer_version::init_version!();
    reth_cli_util::sigsegv_handler::install();
    // ... backtrace setup ...

    let raw_args = XLayerArgs::parse_raw(); // peek at monitor flag without full parse
    if raw_args.monitor_enabled() {
        init_global_tracer(true, Some(raw_args.monitor_output_path()));
    }

    Cli::<XLayerChainSpecParser, Args>::parse().run(|builder, args| async move {
        // tracer already running
    })
}
```

### 5. Structured Error Propagation in `main()`

The top-level `main()` currently uses `.unwrap()` on the `Cli::run` result:

```rust
Cli::<XLayerChainSpecParser, Args>::parse()
    .run(|builder, args| async move { ... })
    .unwrap();
```

**Optimization:** Use `eyre` for structured error reporting consistent with the rest of the codebase:

```rust
fn main() -> eyre::Result<()> {
    // ...
    Cli::<XLayerChainSpecParser, Args>::parse()
        .run(|builder, args| async move { ... })?;
    Ok(())
}
```

This produces a user-friendly error message with context rather than a panic backtrace on startup failures.

### 6. Group Related `XLayerArgs` Fields

`XLayerArgs` mixes flashblocks subscription configuration directly alongside other flat flags:

```rust
pub enable_flashblocks_subscription: bool,
pub flashblocks_subscription_max_addresses: usize,
```

These are subscription-specific and could be grouped into a `FlashblocksSubscriptionArgs` struct (alongside the existing `BuilderArgs.flashblocks`) to keep related configuration co-located:

```rust
#[command(flatten)]
pub flashblocks_subscription: FlashblocksSubscriptionArgs,
```

### 7. Engine Tree Configuration as a Dedicated Function

The `TreeConfig` setup inside `launch_with_fn` is inline:

```rust
let engine_tree_config = TreeConfig::default()
    .with_persistence_threshold(builder.config().engine.persistence_threshold)
    .with_memory_block_buffer_target(
        builder.config().engine.memory_block_buffer_target,
    );
```

**Optimization:** Extract to `fn build_engine_tree_config(config: &NodeConfig) -> TreeConfig` so it can be tested and reused independently.

### 8. Feature-Flag the Allocator

The global allocator is unconditionally set to `reth_cli_util::allocator::Allocator`. On platforms where jemalloc is not available or desired, this should be gated:

```rust
#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();
```

This avoids silent fallback behaviour and makes allocator selection explicit in build tooling.

---

## Crate Dependency Graph

```
bin/node (xlayer-reth-node)
├── xlayer-builder       — payload building & flashblocks strategy
│   └── xlayer-trace-monitor (external)
├── xlayer-flashblocks   — flashblocks service & pubsub
│   └── xlayer-builder
├── xlayer-chainspec     — chain spec parser & fork config
├── xlayer-legacy-rpc    — proxy layer for pre-genesis RPC calls
├── xlayer-monitor       — full-link latency monitoring
│   └── xlayer-trace-monitor (external)
├── xlayer-rpc           — X Layer-specific RPC extensions
├── xlayer-version       — build & git version metadata
└── xlayer-trace-monitor — distributed tracing (external, okx/xlayer-toolkit)
```

Upstream Reth crates (`reth`, `reth-optimism-node`, `reth-node-builder`, etc.) are consumed as workspace dependencies and are not forked at the source level.

---

## Adding a New Node Feature: Step-by-Step

1. **Create a crate** under `crates/<feature>/` with its own `Cargo.toml` and `src/lib.rs`.
2. **Define CLI args** in the new crate using `clap::Args` and `#[command(flatten)]`.
3. **Add the args** to `XLayerArgs` in `bin/node/src/args.rs` via `#[command(flatten)]`.
4. **Wire initialization** in `main.rs` — keep it to a single function call.
5. **Add validation** in `XLayerArgs::validate()` so configuration errors surface before launch.
6. **Write unit tests** inside the new crate (not in `bin/node`) so they can run without building the full binary.

---

## References

- [Reth Node Builder docs](https://paradigmxyz.github.io/reth/docs/reth_node_builder)
- [base/base bin/node](https://github.com/base/base/tree/main/bin/node) — motivating reference for a lean `main.rs` with delegated crate-level logic
- [reth-optimism-node](https://github.com/paradigmxyz/reth/tree/main/crates/optimism/node) — upstream OP node implementation
