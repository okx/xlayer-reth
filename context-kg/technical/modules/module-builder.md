# Module: xlayer_builder

**Path**: `crates/builder/`
**Purpose**: Core flashblock building pipeline for sequencer mode

## Sub-modules

### `args/` — CLI Configuration
- `BuilderArgs`: Builder-specific CLI args (secret key, block time, gas limits, pool logging, deadlines)
- `FlashblocksArgs`: Flashblocks config (enabled, port, address, block time, state root, P2P settings)
- `FlashblocksP2pArgs`: P2P network configuration (listen address, port, peer ID, topic)

### `broadcast/` — P2P Network
- `Node` / `NodeBuilder`: libp2p swarm for flashblock distribution
- `Behaviour`: Composite of mdns, identify, ping, autonat, and stream behaviors
- `StreamsHandler`: Manages outgoing streams to peers for message broadcast; open_stream is non-blocking with retry on failure
- `WebSocketPublisher`: TCP-based WebSocket server for flashblock streaming to subscribers
- `payload.rs`: `XLayerFlashblockPayload` (wraps OpFlashblockPayload + target_index), `XLayerFlashblockEnd` (end-of-sequence signal), `XLayerFlashblockMessage` (enum of Payload | PayloadEnd)
- **Protocol**: Stream protocol "/flashblocks/2.0.0", agent version "flashblock-builder/2.0.0"

### `flashblocks/` — Flashblock Building Core
- `BuilderConfig` / `FlashblocksConfig`: Runtime configuration converted from CLI args
- `FlashblocksServiceBuilder`: Implements `PayloadServiceBuilder` — spawns P2P node, payload handler, and background tasks
- `FlashblocksBuilderCtx`: EVM execution context with hardfork-aware configuration
- `FlashblocksPayloadHandler`: Processes built flashblock payloads, handles external flashblock execution
- `FlashblocksBuilder`: Core builder logic — transaction selection, execution, payload assembly
- `BestFlashblocksTxs`: Transaction iterator wrapper that tracks committed transactions across flashblocks
- `BlockPayloadJobGenerator` / `BlockPayloadJob`: Payload job lifecycle management
- `FlashblocksBuilderTx`: Builder-injected transactions (base tip, number contract increment)

### `flashblocks/utils/` — Support Utilities
- `FlashblockPayloadsCache`: In-memory + optional disk persistence for flashblock payloads
- `ExecutionInfo` / `TxnExecutionResult`: Execution tracking and DA limit validation
- Monitor integration for transaction execution tracing
- `MockFbTransactionFactory`: Test mock implementing pool transaction traits

### `metrics/` — Observability
- `BuilderMetrics`: Comprehensive metric fields (block building, flashblock timing, tx pool, state)
- `TokioTaskMetricsRecorder` / `TokioRuntimeMetricsRecorder`: Tokio runtime monitoring
- `FlashblocksTaskMetrics`: Per-task metrics for all critical background tasks

### `signer.rs` — Transaction Signing
- `Signer` struct for ECDSA signing of builder transactions
- Supports `FromStr` for key parsing and random signer generation for testing

### `traits.rs` — Trait Bounds
- `NodeBounds`, `PoolBounds`, `ClientBounds`, `PayloadTxsBounds`: Specialized OP trait bounds used across the builder

## Key Design Decisions

1. **Payload builder strategy pattern**: `XLayerPayloadServiceBuilder` in `bin/node/src/payload.rs` delegates to either `FlashblocksServiceBuilder` (sequencer) or `BasicPayloadServiceBuilder` (follower) based on `--xlayer.flashblocks.enabled`.

2. **P2P + WebSocket dual distribution**: Flashblocks are distributed via both libp2p streams (peer-to-peer, reliable) and WebSocket broadcast (pub-sub, scalable). The WebSocket publisher has configurable subscriber limits.

3. **Builder transactions are post-user-txs**: Builder-injected transactions (tip, number contract) execute after all user transactions in a flashblock, ensuring they don't consume user gas budget.

4. **Cache-resume execution**: The payload handler supports resuming execution from a cached prefix — if the new flashblock extends the previous one, only the delta transactions are executed against cached state.

5. **End-of-sequence signaling**: `XLayerFlashblockEnd` sent after last flashblock in a block, preventing processing of additional flashblocks after sequence completion.

6. **Non-blocking stream connections**: P2P `open_stream` operations are non-blocking with configurable retry intervals (DEFAULT_PEER_RETRY_INTERVAL=1s). Failed peers are evicted and reconnected on next retry tick.
