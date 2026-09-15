# Module: xlayer_legacy_rpc

**Path**: `crates/legacy-rpc/`
**Purpose**: Transparent routing of historical RPC requests to a legacy chain endpoint

## Components

### Configuration
- `LegacyRpcRouterConfig`: `enabled`, `legacy_endpoint` (URL), `cutoff_block` (genesis block number), `timeout`

### Tower Middleware
- `LegacyRpcRouterLayer`: Creates `LegacyRpcRouterService<S>` middleware
- Initializes reqwest HTTP client with configured timeout

### Service (`service.rs`)
Core routing logic implementing `RpcServiceT`:

**Method Categories**:
1. **`is_legacy_routable()`** — 21 methods: `eth_getBlockByNumber`, `eth_getBlockByHash`, `eth_getTransactionByHash`, `eth_getTransactionReceipt`, `eth_getTransactionByBlockNumberAndIndex`, `eth_getTransactionByBlockHashAndIndex`, `eth_getBlockTransactionCountByNumber`, `eth_getBlockTransactionCountByHash`, `eth_getUncleCountByBlockNumber`, `eth_getUncleCountByBlockHash`, `eth_getBalance`, `eth_getCode`, `eth_getStorageAt`, `eth_call`, `eth_getTransactionCount`, `eth_getProof`, `eth_getBlockReceipts`, `eth_getLogs`, `debug_traceBlockByNumber`, `debug_traceBlockByHash`, `debug_traceTransaction`

2. **`need_parse_block()`** — Methods requiring block parameter extraction for routing decisions

2b. **Debug trace methods** — `debug_traceBlockByNumber`, `debug_traceBlockByHash`, `debug_traceTransaction`, `debug_traceCall` route deterministically by the target block's height vs. the cutoff (NOT try-local-then-legacy):
   - `debug_traceBlockByNumber`: block number/tag at `params[0]`
   - `debug_traceCall`: block number/tag at `params[1]` (absent ⇒ local)
   - `debug_traceBlockByHash`: block hash at `params[0]`, resolved to a height locally, then compared
   - `debug_traceTransaction`: the transaction's containing-block height (resolved locally via `eth_getTransactionByHash` → `blockNumber`), then compared — routes to legacy directly for historical txs, NOT conditioned on local trace success

3. **`need_try_local_then_legacy()`** — Methods to try local first, fall back to legacy: `eth_getTransactionByHash`, `eth_getTransactionReceipt` (and the eth_* by-hash family). `debug_traceTransaction` is NO LONGER in this set — it uses the deterministic height-based handler above.

**Routing Logic**:
- Parse block parameter → determine if below cutoff → route to legacy or local
- Block hash parameters trigger `eth_getBlockByHash` to resolve block number first
- `eth_getLogs` has special hybrid handling (see `get_logs.rs`)

### Hybrid eth_getLogs (`get_logs.rs`)
- Parses filter parameters to extract block ranges
- Three strategies: Pure Legacy, Pure Local, Hybrid (splits the request at cutoff)
- `merge_eth_get_logs_responses()`: Merges and deduplicates results sorted by block number

### Input Validation (`lib.rs`)
- `is_valid_32_bytes_string()`: Validates 32-byte hex strings (0x prefix + 64 hex chars)
- `parse_block_param()`: Extracts block identifiers from JSON-RPC params (handles string tags, hex numbers, hashes, object format)
- Prevents JSON injection by validating all hex characters

### Forward Logic (`lib.rs`)
- `forward_to_legacy()`: Constructs JSON-RPC request, sends via HTTP, parses response
- `call_eth_get_block_by_hash()`: Internal helper to resolve block hash to number
- `get_transaction_by_hash()`: Internal helper for try-local-then-legacy pattern

## Key Design Points

1. **Cutoff = genesis block**: The cutoff block is the genesis block number of the X Layer chain. All blocks before it belong to the legacy chain.
2. **Tag routing**: For `eth_*` methods, `latest`/`pending`/`safe`/`finalized` are NEVER routed to legacy, and `earliest` IS routed to legacy (via `parse_block_param`, which maps `earliest`→`"0"`). For the four `debug` trace methods, ALL FIVE special tags — `latest`/`pending`/`safe`/`finalized`/**`earliest`** — are served LOCALLY, via a debug-only `is_special_block_tag` pre-check performed before any height comparison. This debug-vs-eth_* divergence on `earliest` is intentional (XLOP-1206): `parse_block_param` is NOT modified, so the eth_* `earliest`→legacy semantics are preserved.
3. **Security**: All block hash inputs are validated via `is_valid_32_bytes_string()` before use in string interpolation to prevent JSON injection attacks.
4. **Batch support**: The service handles batch requests by processing each request individually with the same routing logic.
