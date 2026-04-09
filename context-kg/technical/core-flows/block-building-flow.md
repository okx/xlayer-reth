# Core Flow: Standard Block Building

## Overview

Standard block building is used in **RPC mode** (non-sequencer). It uses upstream OP-Reth's `BasicPayloadServiceBuilder` with `OpPayloadBuilder`.

## Flow

```
OP Node (CL)
    │
    ▼
engine_forkChoiceUpdated (with PayloadAttributes)
    │
    ▼
OpPayloadBuilder::build_payload()
    ├── Select transactions from pool
    ├── Execute transactions via EVM
    ├── Build execution payload
    └── Return PayloadBuilderHandle
    │
    ▼
engine_getPayload (from OP Node)
    │
    ▼
Return built payload to CL
```

## Selection in XLayerPayloadServiceBuilder

`bin/node/src/payload.rs` — `XLayerPayloadServiceBuilder` delegates based on config:

```rust
if xlayer_builder_args.flashblocks.enabled {
    // Sequencer: FlashblocksServiceBuilder
    XLayerPayloadServiceBuilderInner::Flashblocks(...)
} else {
    // Follower/RPC: BasicPayloadServiceBuilder with OpPayloadBuilder
    XLayerPayloadServiceBuilderInner::Default(...)
}
```

Both implement `PayloadServiceBuilder<Node, Pool, OpEvmConfig>`.

## Configuration

- `compute_pending_block`: Whether to compute a pending block for `eth_getBlock("pending")` queries
- `OpDAConfig`: Data availability configuration (blob gas limits)
- `OpGasLimitConfig`: Gas limit constraints

## Key Points

1. Standard building uses upstream OP-Reth code unchanged — no X Layer modifications.
2. Bridge interception is NOT applied in standard mode (only in flashblocks builder).
3. The `with_bridge_config()` call on `XLayerPayloadServiceBuilder` is a no-op for the Default variant.
