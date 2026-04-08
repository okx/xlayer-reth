# RPC API Conventions

## Standard Endpoints

X Layer Reth serves all standard Ethereum JSON-RPC and OP-Reth endpoints without modification. These include `eth_*`, `debug_*`, `net_*`, `web3_*`, `txpool_*`, and `engine_*` namespaces.

## X Layer Custom Endpoints

### `xlayer_flashblocksEnabled`
- **Namespace**: `xlayer`
- **Returns**: `bool` — `true` if a valid, non-expired pending flashblock is present
- **Registration**: Added via `extend_rpc_modules` in `bin/node/src/main.rs`
- **Implementation**: `crates/rpc/src/xlayer_ext.rs`

## Flashblocks Subscription

### `eth_subscribe("flashblocks", {filter})`
- **Transport**: WebSocket only
- **Filter options**:
  - `header_info: bool` — Include block header in events
  - `sub_tx_filter.subscribe_addresses: [Address]` — Filter transactions by address (sender, recipient, log addresses)
  - `sub_tx_filter.tx_info: bool` — Include full transaction data
  - `sub_tx_filter.tx_receipt: bool` — Include transaction receipt
- **Max addresses**: Configurable via `--xlayer.flashblocks-subscription-max-addresses` (default: 1000)
- **Event types**: `FlashblockStreamEvent::Header` or `FlashblockStreamEvent::Transaction`
- **Implementation**: `crates/flashblocks/src/subscription.rs`

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

## Error Handling Pattern

- Standard JSON-RPC error codes are used throughout
- Legacy RPC errors are translated to `ErrorObject::owned(code, message, None::<()>)`
- Legacy endpoint failures return `INTERNAL_ERROR_CODE` with descriptive message
- Parse failures from legacy responses return `INTERNAL_ERROR_CODE`

## Batch Request Support

Both the monitoring and legacy routing middleware support batch JSON-RPC requests. Each request in a batch is processed individually through the same routing logic.
