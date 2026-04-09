---
name: "architecture-overview"
description: "Layer definitions, allowed/prohibited call directions, service responsibilities"
---
# Architecture Overview

## System Context

X Layer Reth is an Optimism execution client that extends upstream Reth/OP-Reth. It produces two binaries:

1. **`xlayer-reth-node`** — The main node binary (sequencer or RPC follower)
2. **`xlayer-reth-tools`** — Offline utilities (import, export, genesis generation, migration)

## High-Level Architecture

```
                              ┌───────────┐
                              │  OP Node  │
                              │   (CL)    │
                              └─────┬─────┘
                                    │
                    ┌───────────────┼──────────────────────────────┐
                    │  xlayer-reth-node                             │
                    │               ▼                               │
                    │  ┌──────────────────────┐                    │
                    │  │   Engine API Layer    │                    │
                    │  │  (FCU + newPayload)   │                    │
                    │  │  XLayerEngineValidator │                   │
                    │  └──────────┬───────────┘                    │
                    │             │                                 │
                    │  ┌──────────▼───────────┐  ┌──────────────┐  │
                    │  │  Payload Builder      │  │  RPC Layer   │  │
                    │  │  ┌─────────────────┐  │  │  ┌─────────┐│  │
                    │  │  │ Flashblocks     │  │  │  │ Eth API  ││  │
                    │  │  │ Service Builder │  │  │  │ Override ││  │
                    │  │  │ (sequencer)     │  │  │  ├─────────┤│  │
                    │  │  ├─────────────────┤  │  │  │ Filter  ││  │
                    │  │  │ Basic Payload   │  │  │  │ Override ││  │
                    │  │  │ Builder (RPC)   │  │  │  ├─────────┤│  │
                    │  │  ├─────────────────┤  │  │  │ Legacy  ││  │  ┌ ─ ─ ─ ─ ─ ─ ┐
                    │  │  │ Bridge          │  │  │  │ RPC     ├┼──┼───▶ Legacy RPC
                    │  │  │ Intercept       │  │  │  │ Router  ││  │  │  (pre-genesis │
                    │  │  └─────────────────┘  │  │  ├─────────┤│  │     block query)
                    │  └──────────────────────┘  │  │ RPC     ││  │  └ ─ ─ ─ ─ ─ ─ ┘
                    │                             │  │ Monitor ││  │
                    │                             │  └─────────┘│  │
                    │                             └──────────────┘  │
                    │                                               │
                    │  ┌────────────────────┐  ┌────────────────┐  │
                    │  │  Flashblocks Crate  │  │  Monitor Crate │  │
                    │  │  ┌───────────────┐ │  │  - XLayerMonitor│ │
                    │  │  │ Cache Layer   │ │  │  - Event tracking│ │
                    │  │  │ Pending/Confirm│ │  │  - Trace output│  │
                    │  │  ├───────────────┤ │  └────────────────┘  │
                    │  │  │ Execution     │ │                      │
                    │  │  │ Engine+Validator│ │                     │
                    │  │  ├───────────────┤ │                      │
                    │  │  │ State Handlers│ │                      │
                    │  │  │ PubSub / WS   │ │                      │
                    │  │  │ Persist/Debug │ │                      │
                    │  │  └───────────────┘ │                      │
                    │  └────────────────────┘                      │
                    │                                               │
                    │  ┌────────────────────────────────────────┐  │
                    │  │  Chain Spec (xlayer_chainspec)          │  │
                    │  │  Mainnet (196) | Testnet (1952) | Devnet (195) │
                    │  └────────────────────────────────────────┘  │
                    └──────────────────────────────────────────────┘
```

The Legacy RPC endpoint (dashed box) is an optional, external HTTP service used solely for querying pre-genesis historical blocks. It is not a core system dependency — the node functions fully without it.

## Component Summary

### Node Binary (`bin/node/`)
- **Entry point**: `main.rs` — Parses CLI, validates config, assembles and launches the node; shares `engine_validator` OnceLock between payload builder and flashblocks RPC
- **`payload.rs`**: `XLayerPayloadServiceBuilder` — Delegates to either flashblocks or standard payload builder
- **`args.rs`**: `XLayerArgs` — All X Layer-specific CLI arguments (builder, legacy, monitor, intercept)

### Crate Responsibilities

| Crate | Role | Mode |
|-------|------|------|
| `xlayer_builder` | Flashblock building, P2P broadcast, metrics | Sequencer |
| `xlayer_flashblocks` | Flashblock cache, execution, engine validation, subscriptions | Both |
| `xlayer_chainspec` | Chain specifications and hardfork definitions | Both |
| `xlayer_bridge_intercept` | Bridge transaction filtering | Sequencer |
| `xlayer_legacy_rpc` | Historical RPC routing middleware | Both |
| `xlayer_monitor` | Transaction lifecycle monitoring | Both |
| `xlayer_rpc` | Custom RPC extensions with flashblock cache overlay | Both |
| `xlayer_version` | Version metadata | Both |

### Sequencer vs RPC Mode

**Sequencer mode** (`--xlayer.sequencer-mode`):
- Uses `FlashblocksServiceBuilder` for payload building
- Runs P2P broadcast node (libp2p) for flashblock distribution
- Runs WebSocket publisher for flashblock streaming
- Bridge intercept is active
- Monitor tracks: SeqReceiveTxEnd, SeqBlockBuildStart, SeqTxExecutionEnd, SeqBlockBuildEnd

**RPC mode** (default):
- Uses `BasicPayloadServiceBuilder` (standard OP payload builder)
- Receives flashblocks via WebSocket subscription (WsFlashBlockStream)
- Runs `FlashblocksRpcService` coordinating:
  - handle_incoming_flashblocks() — ingestion to RawFlashblocksCache
  - handle_execution_tasks() — FlashblockSequenceValidator execution
  - handle_canonical_stream() — reorg detection and cache flush
  - handle_persistence() / handle_relay_flashblocks() — disk + WS relay
- FlashblockStateCache provides three-layer read overlay for RPC queries
- FlashblocksEthApiOverride / FlashblocksFilterOverride intercept eth_* queries
- Optionally serves `FlashblocksPubSub` subscriptions to downstream clients
- Monitor tracks: RpcReceiveTxEnd, RpcBlockReceiveEnd, RpcBlockInsertEnd

## Layer Definitions

| Layer | Responsible Dirs | Allowed Calls | Prohibited Calls |
|-------|-----------------|---------------|-----------------|
| Engine Validation | `xlayer_flashblocks/execution/engine.rs` | Validate payloads, share PayloadProcessor with builder | Double validation, modify pending state without locks |
| Payload Builder Service | `xlayer_builder/flashblocks/` | Build payloads, manage P2P broadcast, spawn handler tasks | Directly modify canonical state |
| Flashblocks RPC Service | `xlayer_flashblocks/` | Subscribe to pending sequences, execute flashblocks, persist data | Modify engine validator, bypass FlashblockSequenceValidator |
| RPC Middleware | `xlayer_legacy_rpc/`, `xlayer_monitor/` | Forward requests, intercept and route based on block cutoff | Modify original request params without validation |

## Extension Points

The node is assembled in `main.rs` using Reth's builder pattern:

```
builder
  .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
  .with_components(op_node.components().payload(payload_builder))  // Custom payload
  .with_add_ons(add_ons)                                          // RPC middleware
  .extend_rpc_modules(move |ctx| { ... })                         // Custom RPC + flashblocks RPC
  .launch_with_fn(|builder| { ... })                              // Engine launcher
```

Key initialization sequence:
1. `engine_validator` OnceLock shared between builder and flashblocks RPC
2. Payload builder setup sets the engine_validator
3. `FlashblocksRpcService.spawn_rpc()` called after payload builder ensures engine_validator is initialized
4. RPC middleware stacked as tuple: `(RpcMonitorLayer, LegacyRpcRouterLayer)` — monitor executes first, then legacy routing
