# Core Flow: Legacy RPC Routing

## Overview

X Layer chains were migrated from a legacy chain at a specific block height (the genesis block). Historical data before this block lives on a separate legacy RPC endpoint. The `LegacyRpcRouterService` transparently routes requests to the correct backend.

## Request Flow

```
User RPC Request
       │
       ▼
RpcMonitorLayer (intercepts eth_sendRawTransaction)
       │
       ▼
LegacyRpcRouterService
       │
       ├── Is method legacy-routable? ──No──▶ Forward to local Reth
       │       │
       │      Yes
       │       │
       │       ▼
       ├── Method type?
       │       │
       │       ├── need_try_local_then_legacy (eth_getTransactionByHash, etc.)
       │       │       ├── Try local first
       │       │       ├── If result non-empty → return
       │       │       └── If empty → forward to legacy
       │       │
       │       ├── need_parse_block (eth_getBlockByNumber, eth_getBalance, etc.)
       │       │       ├── Parse block parameter
       │       │       ├── If block < cutoff → forward to legacy
       │       │       ├── If block hash → resolve via eth_getBlockByHash
       │       │       │       ├── If resolved < cutoff → forward to legacy
       │       │       │       └── Otherwise → forward to local
       │       │       └── If latest/pending/safe/finalized → forward to local
       │       │
       │       └── eth_getLogs (special handling)
       │               ├── Parse filter params (block range or block hash)
       │               ├── Pure legacy (entire range < cutoff)
       │               ├── Pure local (entire range >= cutoff)
       │               └── Hybrid (split at cutoff, merge results)
       │
       ▼
Response returned to user
```

## Routing Rules

| Block Parameter | Routing |
|-----------------|---------|
| `"latest"`, `"pending"`, `"safe"`, `"finalized"` | Local |
| `"earliest"` | Legacy (always — local has no pre-genesis data) |
| Hex number < cutoff | Legacy |
| Hex number >= cutoff | Local |
| Block hash | Resolve to number first via `eth_getBlockByHash`, then route by number |

## Hybrid eth_getLogs

When a log filter spans the cutoff boundary:

```
[fromBlock=100, toBlock=500] with cutoff=300

Split into:
  Legacy request:  [fromBlock=100, toBlock=299]
  Local request:   [fromBlock=300, toBlock=500]

Results merged: sorted by (blockNumber, logIndex), deduplicated
```

## Configuration

- `--rpc.legacy-url`: Legacy endpoint URL (enables routing when set)
- `--rpc.legacy-timeout`: HTTP timeout (default: 30s, requires legacy URL)
- Cutoff block: Automatically set to the chain's genesis block number

## Key Points

1. Routing is completely transparent to the user — same endpoint serves both historical and current data.
2. 21 RPC methods support legacy routing (see `module-legacy-rpc.md` for full list).
3. Methods not in the routable list are always served locally.
4. Input validation prevents JSON injection via `is_valid_32_bytes_string()`.
