# Dependency Map

## Internal Module Dependencies

```
bin/node
  ├── xlayer_builder        (BuilderArgs, FlashblocksServiceBuilder, BuilderConfig)
  ├── xlayer_chainspec      (XLayerChainSpecParser)
  ├── xlayer_flashblocks    (FlashblocksService, FlashblocksPubSub)
  ├── xlayer_bridge_intercept (BridgeInterceptConfig)
  ├── xlayer_legacy_rpc     (LegacyRpcRouterLayer, LegacyRpcRouterConfig)
  ├── xlayer_monitor        (XLayerMonitor, RpcMonitorLayer, start_monitor_handle)
  ├── xlayer_rpc            (XlayerRpcExt, XlayerRpcExtApiServer)
  └── xlayer_version        (init_version!)

bin/tools
  ├── xlayer_chainspec      (XLayerChainSpecParser)
  └── xlayer_version        (init_version!)

xlayer_builder
  ├── xlayer_bridge_intercept (BridgeInterceptConfig, intercept_bridge_transaction_if_need)
  └── xlayer_flashblocks      (FlashBlock types, used in handler/broadcast)

xlayer_flashblocks
  └── xlayer_builder          (BuilderConfig, metrics, cache — via feature gates)

xlayer_rpc
  └── xlayer_flashblocks      (PendingFlashBlock, PendingBlockRx — for flashblock detection)

xlayer_monitor
  └── (standalone — no internal crate dependencies)

xlayer_legacy_rpc
  └── (standalone — no internal crate dependencies)

xlayer_bridge_intercept
  └── (standalone — no internal crate dependencies)

xlayer_chainspec
  └── (standalone — no internal crate dependencies)

xlayer_version
  └── (standalone — no internal crate dependencies)
```

## Dependency Direction Rules

1. **Binaries depend on crates, not the reverse.** `bin/node` and `bin/tools` are the top-level assembly points.
2. **`xlayer_builder` ↔ `xlayer_flashblocks` is the only bidirectional dependency.** This is managed via feature gates (`#[cfg(any(test, feature = "testing"))]`) and type re-exports. Both crates deal with flashblock building from different angles (builder = production, flashblocks = orchestration).
3. **Standalone crates have zero internal dependencies.** `intercept`, `legacy_rpc`, `monitor`, `chainspec`, and `version` are independently compilable.
4. **RPC crate depends on flashblocks for pending block detection.** This is a read-only dependency (checking `PendingBlockRx`).

## External Dependencies (Key Upstream Crates)

| Category | Crates |
|----------|--------|
| **Reth Core** | `reth`, `reth-provider`, `reth-db`, `reth-db-api`, `reth-payload-builder`, `reth-node-builder`, `reth-node-core`, `reth-cli`, `reth-cli-commands` |
| **OP Stack** | `reth-optimism-node`, `reth-optimism-cli`, `reth-optimism-chainspec`, `reth-optimism-evm`, `reth-optimism-payload-builder`, `reth-optimism-rpc`, `reth-optimism-primitives`, `reth-optimism-forks`, `reth-optimism-consensus` |
| **Alloy** | `alloy-primitives`, `alloy-consensus`, `alloy-rlp`, `alloy-genesis`, `alloy-rpc-types-eth`, `op-alloy-rpc-types-engine`, `op-alloy-network` |
| **Networking** | `libp2p` (P2P broadcast), `tokio-tungstenite` (WebSocket), `reqwest` (HTTP client) |
| **RPC** | `jsonrpsee` (JSON-RPC server/client), `jsonrpsee-types` |
| **Serialization** | `serde`, `serde_json`, `alloy-rlp` |
| **Crypto** | `k256` (ECDSA signing in builder) |
| **Storage** | `reth-db` (MDBX), `reth-static-file-types` (static files) |
| **Compression** | `flate2` (gzip for export), `brotli` (flashblock decoding) |

## Middleware / Storage Services

| Service | Usage |
|---------|-------|
| **MDBX** | Primary key-value store (accounts, storage, history tables) |
| **RocksDB** | Secondary store for `TransactionHashNumbers`, `AccountsHistory`, `StoragesHistory` after migration |
| **Static Files** | Immutable segments: `TransactionSenders`, `AccountChangeSets`, `StorageChangeSets`, `Receipts` |
| **Legacy RPC Endpoint** | External HTTP service for pre-genesis historical data |
| **OP Node (Consensus Layer)** | External service providing engine API calls (FCU, newPayload, attributes) |

## Feature Flags

| Flag | Effect |
|------|--------|
| `testing` | Enables test utilities in `xlayer_builder` (mock factories, test framework) |
