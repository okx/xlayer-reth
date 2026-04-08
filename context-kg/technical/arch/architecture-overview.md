# Architecture Overview

## System Context

X Layer Reth is an Optimism execution client that extends upstream Reth/OP-Reth. It produces two binaries:

1. **`xlayer-reth-node`** — The main node binary (sequencer or RPC follower)
2. **`xlayer-reth-tools`** — Offline utilities (import, export, genesis generation, migration)

## High-Level Architecture

```
                                    ┌─────────────────────────────┐
                                    │     External Systems        │
                                    │  ┌───────┐  ┌───────────┐  │
                                    │  │OP Node│  │Legacy RPC │  │
                                    │  │(CL)   │  │(Erigon)   │  │
                                    │  └───┬───┘  └─────┬─────┘  │
                                    └──────┼────────────┼────────┘
                                           │            │
                    ┌──────────────────────┼────────────┼─────────────────────┐
                    │  xlayer-reth-node     │            │                     │
                    │                      ▼            │                     │
                    │  ┌──────────────────────┐         │                     │
                    │  │   Engine API Layer    │         │                     │
                    │  │  (FCU + newPayload)   │         │                     │
                    │  └──────────┬───────────┘         │                     │
                    │             │                      │                     │
                    │  ┌──────────▼───────────┐  ┌──────▼──────────────────┐  │
                    │  │  Payload Builder      │  │  RPC Layer              │  │
                    │  │  ┌─────────────────┐  │  │  ┌────────────────────┐│  │
                    │  │  │ Flashblocks     │  │  │  │ X Layer RPC Ext   ││  │
                    │  │  │ Service Builder │  │  │  │ (flashblocks_     ││  │
                    │  │  │ (sequencer)     │  │  │  │  enabled)         ││  │
                    │  │  ├─────────────────┤  │  │  ├────────────────────┤│  │
                    │  │  │ Basic Payload   │  │  │  │ Legacy RPC Router ││  │
                    │  │  │ Builder (RPC)   │  │  │  │ (Tower middleware) ││  │
                    │  │  ├─────────────────┤  │  │  ├────────────────────┤│  │
                    │  │  │ Bridge          │  │  │  │ RPC Monitor       ││  │
                    │  │  │ Intercept       │  │  │  │ (Tower middleware) ││  │
                    │  │  └─────────────────┘  │  │  └────────────────────┘│  │
                    │  └──────────────────────┘  └────────────────────────┘  │
                    │                                                         │
                    │  ┌──────────────────────┐  ┌────────────────────────┐  │
                    │  │  Flashblocks Crate    │  │  Monitor Crate         │  │
                    │  │  - FlashBlockService  │  │  - XLayerMonitor       │  │
                    │  │  - Consensus Client   │  │  - Event tracking      │  │
                    │  │  - PubSub / WS stream │  │  - Trace output        │  │
                    │  │  - Pending State Reg  │  │                        │  │
                    │  └──────────────────────┘  └────────────────────────┘  │
                    │                                                         │
                    │  ┌──────────────────────────────────────────────────┐  │
                    │  │  Chain Spec (xlayer_chainspec)                    │  │
                    │  │  Mainnet (196) | Testnet (1952) | Devnet (195)   │  │
                    │  └──────────────────────────────────────────────────┘  │
                    └─────────────────────────────────────────────────────────┘
```

## Component Summary

### Node Binary (`bin/node/`)
- **Entry point**: `main.rs` — Parses CLI, validates config, assembles and launches the node
- **`payload.rs`**: `XLayerPayloadServiceBuilder` — Delegates to either flashblocks or standard payload builder
- **`args.rs`**: `XLayerArgs` — All X Layer-specific CLI arguments (builder, legacy, monitor, intercept)

### Crate Responsibilities

| Crate | Role | Mode |
|-------|------|------|
| `xlayer_builder` | Flashblock building, P2P broadcast, metrics | Sequencer |
| `xlayer_flashblocks` | Flashblock orchestration, consensus, subscriptions | Both |
| `xlayer_chainspec` | Chain specifications and hardfork definitions | Both |
| `xlayer_bridge_intercept` | Bridge transaction filtering | Sequencer |
| `xlayer_legacy_rpc` | Historical RPC routing middleware | Both |
| `xlayer_monitor` | Transaction lifecycle monitoring | Both |
| `xlayer_rpc` | Custom RPC extensions | Both |
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
- Receives flashblocks via WebSocket subscription
- Runs `FlashblocksService` handler for received flashblocks
- Optionally serves `FlashblocksPubSub` subscriptions to downstream clients
- Monitor tracks: RpcReceiveTxEnd, RpcBlockReceiveEnd, RpcBlockInsertEnd

## Extension Points

The node is assembled in `main.rs` using Reth's builder pattern:

```
builder
  .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
  .with_components(op_node.components().payload(payload_builder))  // Custom payload
  .with_add_ons(add_ons)                                          // RPC middleware
  .extend_rpc_modules(move |ctx| { ... })                         // Custom RPC
  .launch_with_fn(|builder| { ... })                              // Engine launcher
```

RPC middleware is stacked as a tuple: `(RpcMonitorLayer, LegacyRpcRouterLayer)` — monitor executes first, then legacy routing.
