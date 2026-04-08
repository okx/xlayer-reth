---
name: "dependency"
description: "Upstream callers, inter-module deps, storage/middleware, external services"
---
# Dependency Map

## Internal Module Dependencies

```
bin/node
  ├── xlayer_builder        (BuilderArgs, FlashblocksServiceBuilder, BuilderConfig)
  ├── xlayer_chainspec      (XLayerChainSpecParser)
  ├── xlayer_flashblocks    (FlashblocksRpcService, FlashblocksPubSub, XLayerEngineValidatorBuilder, FlashblockStateCache)
  ├── xlayer_bridge_intercept (BridgeInterceptConfig)
  ├── xlayer_legacy_rpc     (LegacyRpcRouterLayer, LegacyRpcRouterConfig)
  ├── xlayer_monitor        (XLayerMonitor, RpcMonitorLayer, start_monitor_handle)
  ├── xlayer_rpc            (DefaultRpcExt, FlashblocksEthApiOverride, FlashblocksFilterOverride)
  └── xlayer_version        (init_version!)

bin/tools
  ├── xlayer_chainspec      (XLayerChainSpecParser)
  └── xlayer_version        (init_version!)

xlayer_builder
  ├── xlayer_bridge_intercept (BridgeInterceptConfig, intercept_bridge_transaction_if_need)
  └── xlayer_flashblocks      (FlashblockReceipt, BuildArgs types)

xlayer_flashblocks
  └── xlayer_builder          (XLayerFlashblockMessage, WebSocketPublisher, FlashblockPayloadsCache)

xlayer_rpc
  └── xlayer_flashblocks      (FlashblockStateCache, PendingSequence, CachedTxInfo — for cache-aware RPC)

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

## Upstream Callers

| Caller | Protocol | Entry Point |
|--------|----------|------------|
| WebSocket clients | HTTP/WebSocket | Subscribe to flashblocks via WebSocketPublisher TCP listener |
| libp2p P2P peers | libp2p/Stream | Dial/listen on configured multiaddrs for flashblock broadcasts |
| RPC clients | HTTP | eth_sendRawTransaction, eth_sendTransaction monitored by RpcMonitorLayer |

## Dependency Direction Rules

1. **Binaries depend on crates, not the reverse.** `bin/node` and `bin/tools` are the top-level assembly points.
2. **`xlayer_builder` ↔ `xlayer_flashblocks` is the only bidirectional dependency.** Builder provides message types and WebSocket publisher; flashblocks provides execution types and receipt traits. Both deal with flashblock building from different angles (builder = sequencer production, flashblocks = RPC-side execution/cache).
3. **Standalone crates have zero internal dependencies.** `intercept`, `legacy_rpc`, `monitor`, `chainspec`, and `version` are independently compilable.
4. **RPC crate depends on flashblocks for cache-aware queries.** This is a read-only dependency (reading FlashblockStateCache).
5. **engine_validator is shared via OnceLock.** Set by payload builder, read by flashblocks RPC service — ordering enforced in main.rs initialization sequence.

## Inter-Module Dependencies

| From | To | Mechanism | Description |
|------|----|-----------|-------------|
| xlayer-reth-node → xlayer-builder | Direct | Payload builder with P2P broadcast |
| xlayer-reth-node → xlayer-flashblocks | Direct | Flashblocks stream handling, execution, cache |
| xlayer-reth-node → xlayer-monitor | Direct | Transaction and block monitoring via RPC interception |
| xlayer-reth-node → xlayer-legacy-rpc | Direct | Legacy RPC endpoint compatibility |
| xlayer-builder → xlayer-bridge-intercept | Function call | Bridge transaction filtering during payload building |
| xlayer-flashblocks → xlayer-builder | Direct import | XLayerFlashblockMessage, WebSocketPublisher, FlashblockPayloadsCache |
| xlayer-rpc → xlayer-flashblocks | Direct import | FlashblockStateCache for cache-aware RPC queries |
| xlayer-monitor → xlayer-trace-monitor | Function call | tracer.log_transaction, tracer.log_block for full-link tracing |

## Storage and Middleware

| Component | Type | Usage |
|-----------|------|-------|
| FlashblockStateCache | In-memory (Arc<RwLock>) | Three-layer cache: pending/confirmed/canonical for flashblock queries |
| FlashblockPayloadsCache | In-memory + disk | Caches pending flashblocks, flushes every 5s to datadir (atomic temp+rename) |
| RawFlashblocksCache | In-memory ring buffer | Capacity 50, accumulates raw incoming flashblocks for execution |
| ExecutionTaskQueue | In-memory BTreeSet | Async task queue with Notify signaling for execution scheduling |
| tokio::sync::broadcast | In-memory channel | Routes serialized flashblocks to WebSocket subscribers |
| HashMap<PeerId, Stream> | In-memory | Tracks active libp2p streams to each connected peer |
| MDBX | On-disk KV store | Primary key-value store (accounts, storage, history tables) |
| Static Files | On-disk segments | Immutable segments: TransactionSenders, ChangeSets, Receipts |
| Legacy RPC Endpoint | External HTTP | Pre-genesis historical data |
| OP Node (CL) | External | Engine API calls (FCU, newPayload, attributes) |

## External Services

| Service | SDK/Client | Purpose |
|---------|-----------|---------|
| libp2p swarm (tcp+noise+yamux) | crates/builder/broadcast/ | P2P networking, peer discovery (mDNS), stream multiplexing |
| tokio_tungstenite WebSocket | WebSocketPublisher | WebSocket server for flashblock client connections |
| xlayer-trace-monitor | xlayer-toolkit (git) | Global tracer for transaction/block lifecycle events |
| brotli compression | flashblocks/ws/decoding | Decompresses brotli-encoded flashblock messages |

## Prohibited Patterns

[Rule] P2P broadcast must not wait for acknowledgment from peers; failures logged but don't block WebSocket publish
[Rule] WebSocketPublisher.publish() must serialize to JSON before broadcast; client errors logged but node continues
[Rule] RpcMonitorLayer only intercepts eth_sendRawTransaction and eth_sendTransaction methods
[Rule] Bridge intercept check must parse BridgeEvent signature and validate origin address match before blocking
[Rule] FlashblockPayloadsCache persistence must use atomic file writes (temp file + rename)

## Feature Flags

| Flag | Effect |
|------|--------|
| `testing` | Enables test utilities in `xlayer_builder` (mock factories, test framework) |
