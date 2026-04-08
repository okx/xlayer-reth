# Conventions: Common Tools and Utilities

## Build System

### Justfile (`just`)
Primary task runner. Key commands:
- `just build` — Release build
- `just check` — Full quality gate (format + clippy + tests)
- `just test` — Unit tests via `cargo nextest`
- `just fix` — Auto-fix formatting and clippy warnings
- `just build-docker` — Docker image build

### Cargo Workspace
- Root `Cargo.toml` defines the workspace with all crates
- `deps/` directory contains local dependency overrides (e.g., `okx/optimism` dev branch)
- `rust-toolchain.toml` pins the Rust version
- `rustfmt.toml` configures formatting rules

## Shared Utilities

### Version Initialization
```rust
xlayer_version::init_version!();
```
Must be called first in every binary's `main()`. Initializes global version metadata.

### Signer (`crates/builder/src/signer.rs`)
```rust
let signer = Signer::from_str("0x...")?;   // From hex private key
let signer = Signer::random();              // For testing
let signed_tx = signer.sign_transaction(tx)?;
```

### Trait Bounds (`crates/builder/src/traits.rs`)
Centralized trait bound aliases for OP-specific generics:
- `NodeBounds` — Full node type requirements
- `PoolBounds` — Transaction pool requirements
- `ClientBounds` — Provider/client requirements
- `PayloadTxsBounds` — Payload transaction iterator requirements

### Metrics
```rust
use xlayer_builder::metrics::BuilderMetrics;
let metrics = BuilderMetrics::default();
metrics.set_builder_metrics(block_number, gas_used, ...);
```

Tokio runtime metrics via `TokioRuntimeMetricsRecorder`.

### Test Utilities (`crates/flashblocks/src/test_utils.rs`)
```rust
let factory = TestFlashBlockFactory::default();
let fb0 = factory.flashblock_at(0);          // Base flashblock
let fb1 = factory.flashblock_after(&fb0);    // Next in sequence
let fb_next = factory.flashblock_for_next_block(&fb1); // New block
```

### Mock Transaction Factory (`crates/builder/src/flashblocks/utils/mock.rs`)
```rust
let factory = MockFbTransactionFactory::new();
let tx = factory.create_legacy_tx(nonce, gas_price, value);
```
Implements full `PoolTransaction` and `EthPoolTransaction` traits for testing.

## Logging Conventions

### Target Prefixes
| Target | Usage |
|--------|-------|
| `reth::cli` | CLI operations (import, export, migration) |
| `xlayer::monitor` | Monitor events |
| `xlayer::intercept` | Bridge intercept configuration and events |
| `xlayer::import` | Block import tool |
| `xlayer::export` | Block export tool |
| `payload_builder` | Payload building (including intercept debug logs) |

### Structured Logging
```rust
info!(
    target: "reth::cli",
    ?segment,
    from = start,
    to = to_block,
    total_blocks,
    "Migrating"
);
```

Use structured fields for machine-parseable data. Use the message string for human context.

## Error Handling Patterns

### CLI/Startup Errors
```rust
if let Err(e) = args.xlayer_args.validate() {
    eprintln!("X Layer configuration error: {e}");
    std::process::exit(1);
}
```

### Runtime Errors
```rust
// Use eyre for chain-able errors
.wrap_err("Failed to get latest block number")?;
.wrap_err_with(|| format!("Failed to create output file: {}", path.display()))?;
```

### Graceful Shutdown
```rust
let shutdown = Arc::new(AtomicBool::new(false));
ctrlc::set_handler(move || { shutdown.store(true, Ordering::SeqCst); })?;
// Check in hot loops:
if shutdown.load(Ordering::SeqCst) { break; }
```

## Docker

- `DockerfileOp` — Node binary image
- `DockerfileTools` — Tools binary image
