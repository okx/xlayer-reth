---
name: "rpc-api-conventions"
description: "JSON-RPC conventions, response format, flashblock-aware query patterns, error handling"
---
# RPC API Conventions

## Standard Endpoints

X Layer Reth serves all standard Ethereum JSON-RPC and OP-Reth endpoints without modification. These include `eth_*`, `debug_*`, `net_*`, `web3_*`, `txpool_*`, and `engine_*` namespaces.

## Flashblock-Aware Query Overrides

The following standard eth methods are overridden with cache-aware versions via `FlashblocksEthApiOverride` and `FlashblocksFilterOverride`:

| Method | Override Behavior |
|--------|-------------------|
| `eth_blockNumber` | Returns max of canonical and flashblock pending height |
| `eth_getBlockByNumber` | Checks flashblock cache first for pending/latest/numbered blocks |
| `eth_getBlockByHash` | Checks flashblock cache first by hash |
| `eth_getTransactionByHash` | Looks up CachedTxInfo from flashblock state cache |
| `eth_getBlockReceipts` | Returns receipts from flashblock cache for pending/confirmed blocks |
| `eth_getTransactionReceipt` | Returns receipt from CachedTxInfo |
| `eth_getLogs` | Splits query into canonical and flashblock ranges, merges results |

**Cache-first pattern**: Query FlashblockStateCache (pending → confirmed) first, fall back to standard Eth API provider.

## X Layer Custom Endpoints

### `eth_flashblocksEnabled`
- **Namespace**: `eth`
- **Returns**: `bool` — `true` if flashblocks cache is initialized AND confirm_height > 0
- **Registration**: Added via `extend_rpc_modules` in `bin/node/src/main.rs`
- **Implementation**: `crates/rpc/src/default.rs`

## Flashblocks Subscription

### `eth_subscribe("flashblocks", {filter})`
- **Transport**: WebSocket only
- **Filter options**:
  - `header_info: bool` — Include block header in events
  - `sub_tx_filter.subscribe_addresses: [Address]` — Filter transactions by address (sender, recipient, log addresses)
  - `sub_tx_filter.tx_info: bool` — Include full transaction data
  - `sub_tx_filter.tx_receipt: bool` — Include transaction receipt
- **Max addresses**: Configurable via `--xlayer.flashblocks-subscription-max-addresses` (default: 1000)
- **Event types**: `FlashblockStreamEvent::Header`, `FlashblockStreamEvent::Transaction`, or `FlashblockStreamEvent::Receipt`
- **Deduplication**: LRU tx hash cache (MAX_TXHASH_CACHE_SIZE=10000) prevents duplicate events
- **Implementation**: `crates/flashblocks/src/subscription/rpc.rs`

## RPC Middleware Stack

Requests pass through middleware in this order:
1. **RpcMonitorLayer** (outer) — Intercepts `eth_sendRawTransaction` / `eth_sendTransaction` for monitoring
2. **LegacyRpcRouterLayer** (inner) — Routes historical requests to legacy endpoint

## Legacy-Routable Methods

The following 21 methods support transparent legacy routing when a legacy endpoint is configured:

```
eth_getBlockByNumber          eth_getBlockByHash
eth_getTransactionByHash      eth_getTransactionReceipt
eth_getTransactionByBlockNumberAndIndex
eth_getTransactionByBlockHashAndIndex
eth_getBlockTransactionCountByNumber
eth_getBlockTransactionCountByHash
eth_getUncleCountByBlockNumber
eth_getUncleCountByBlockHash
eth_getBalance                eth_getCode
eth_getStorageAt              eth_call
eth_getTransactionCount       eth_getProof
eth_getBlockReceipts          eth_getLogs
debug_traceBlockByNumber      debug_traceBlockByHash
debug_traceTransaction
```

## Response Format

- **Success**: `RpcResult<T>` — Direct T serialization with HTTP 200
- **Error**: `ErrorObject` with `code` and `message` fields (standard JSON-RPC)
- **Convention**: Use `RpcResult` wrapper type from jsonrpsee throughout

## Error Handling Pattern

- Standard JSON-RPC error codes are used throughout
- Legacy RPC errors are translated to `ErrorObject::owned(code, message, None::<()>)`
- Legacy endpoint failures return `INTERNAL_ERROR_CODE` with descriptive message
- Parse failures from legacy responses return `INTERNAL_ERROR_CODE`

## Batch Request Support

Both the monitoring and legacy routing middleware support batch JSON-RPC requests. Each request in a batch is processed individually through the same routing logic.

## Pagination

- **Log queries**: Block range-based pagination (fromBlock/toBlock)
- **Flashblock-aware getLogs**: Splits query into canonical range and flashblock range, merges results sorted by block number
- **Legacy routing for getLogs**: If fromBlock < cutoff_block, routes to legacy RPC endpoint
