---
name: "module-rpc"
description: "Module design for xlayer_rpc: Eth API extensions with flashblock cache overlay"
---
# Module: xlayer_rpc

**Path**: `crates/rpc/`
**Purpose**: X Layer-specific RPC extensions providing flashblock-aware Eth API overrides

## Components

### Default RPC Extension (`default.rs`)
- `DefaultRpcExtApi` trait: Defines `eth_flashblocksEnabled` RPC method via jsonrpsee `#[rpc(server, namespace = "eth")]`
- `DefaultRpcExt<T>` implementation: Checks cache is not None AND confirm_height > 0 to ensure flashblocks state is actively receiving data
- Returns `true` when flashblocks cache is initialized and actively receiving data

### Flashblocks Eth API Override (`eth.rs`)
- `FlashblocksEthApiOverride<Eth, N>`: Overrides 6 core eth methods with cache-aware versions:
  - `block_number()` — Returns max of canonical and flashblock pending height
  - `block_by_number()` — Checks flashblock cache first for pending/latest/numbered blocks
  - `block_by_hash()` — Checks flashblock cache first by hash
  - `transaction_by_hash()` — Looks up CachedTxInfo from flashblock state cache
  - `block_receipts()` — Returns receipts from flashblock cache for pending/confirmed blocks
  - `transaction_receipt()` — Returns receipt from CachedTxInfo
- **Cache-first pattern**: Query FlashblockStateCache first, fall back to standard Eth API provider

### Flashblocks Filter Override (`filter.rs`)
- `FlashblocksFilterOverride<F, N>`: Overrides `eth_getLogs` with flashblock state cache overlay
  - Splits query into canonical range and flashblock range
  - Merges results from both sources sorted by block number
  - Handles pending block log queries from flashblock cache

### Helper Functions (`helper.rs`)
- `to_rpc_block()` — Converts PendingBlock to RPC Block response
- `to_block_receipts()` — Converts block receipts to RPC format
- `to_rpc_transaction()` — Converts transaction to RPC format
- All helpers are `pub(crate)` functions returning Result types

### Provider Traits (`lib.rs`)
- `FlashblocksEthApiExt`: Extended Eth API trait combining all flashblock overrides
- `FlashblocksEthFilterExt`: Extended Filter API trait with flashblock overlay
- Generic over `NodePrimitives` for type safety

## RPC Registration

Registered in `bin/node/src/main.rs` via `extend_rpc_modules`:
- DefaultRpcExt for `eth_flashblocksEnabled`
- FlashblocksEthApiOverride for block/tx/receipt queries
- FlashblocksFilterOverride for log queries

## Key Design Points

1. **Cache-first architecture**: All query methods check FlashblockStateCache before delegating to standard provider
2. **Three-layer lookup**: Pending → Confirmed → Canonical (via standard provider)
3. **Trait-based composition**: Uses FlashblocksEthApiExt and FlashblocksEthFilterExt traits for testability and modularity
4. **Conversion helpers**: All RPC response conversions centralized in `helper.rs` using `RpcConvert` trait
5. **No state mutation**: RPC layer is purely read-only; all state changes flow through execution pipeline

## NOT Responsible For
- Flashblock state management (that's `xlayer_flashblocks::cache`)
- Flashblock execution (that's `xlayer_flashblocks::execution`)
- Subscription streaming (that's `xlayer_flashblocks::subscription`)
