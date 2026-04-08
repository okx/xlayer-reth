# Module: xlayer_rpc

**Path**: `crates/rpc/`
**Purpose**: X Layer-specific RPC extensions

## Components

### XLayer Extension (`xlayer_ext.rs`)
- `XlayerRpcExtApi<Net>` trait: Defines `xlayer_flashblocksEnabled()` RPC method
- `XlayerRpcExt<T>` implementation: Checks if a pending flashblock exists and is not expired
- Returns `true` when a valid (non-expired) pending flashblock is present

### Provider Traits (`lib.rs`)
- `SequencerClientProvider`: Trait for accessing the sequencer HTTP client
  - Implemented for `OpEthApi<N, Rpc>` — delegates to `self.sequencer_client()`
- `PendingFlashBlockProvider`: Trait for checking pending flashblock availability
  - Implemented for `OpEthApi<N, Rpc>` — checks `pending_block_rx()` for non-expired flashblock
  - Expiration is checked via `Instant::now() < pending_flashblock.expires_at`

## RPC Registration

Registered in `bin/node/src/main.rs` via `extend_rpc_modules`:
```rust
let xlayer_rpc = XlayerRpcExt { backend: new_op_eth_api };
ctx.modules.merge_configured(XlayerRpcExtApiServer::<Optimism>::into_rpc(xlayer_rpc))?;
```

## Key Design Points

1. **Minimal surface**: Only one custom RPC method (`xlayer_flashblocksEnabled`). All standard ETH/OP methods come from upstream.
2. **Expiration-aware**: Flashblock detection includes a time-based expiration check to avoid reporting stale flashblocks.
3. **Trait-based abstraction**: Uses `PendingFlashBlockProvider` and `SequencerClientProvider` traits for testability.
