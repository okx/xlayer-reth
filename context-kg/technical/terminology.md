# Terminology

## Core Concepts

| Term | Definition |
|------|-----------|
| **Flashblock** | A sub-block payload (`OpFlashblockPayload`) produced incrementally during a block's build interval. Multiple flashblocks form a sequence that represents a single canonical block. |
| **Flashblock Sequence** | An ordered collection of flashblocks (index 0..N) sharing the same block number and payload ID. Managed as `FlashBlockPendingSequence` (mutable) or `FlashBlockCompleteSequence` (finalized, immutable). |
| **Canonical Block** | A block that has been finalized by the consensus engine and is part of the canonical chain. Distinct from pending/speculative blocks. |
| **Pending Block** | A block being built from accumulated flashblocks. Represented as `PendingFlashBlock<N>` wrapping a `PendingBlock` with flashblock metadata. |
| **Payload Builder** | Component responsible for constructing execution payloads (blocks). X Layer uses either `FlashblocksServiceBuilder` (sequencer) or `BasicPayloadServiceBuilder` (follower). |
| **Cutoff Block** | The genesis block number of the current X Layer chain. Requests for blocks below this are routed to the legacy RPC endpoint. |
| **Bridge Intercept** | Mechanism to filter out specific bridge transactions during payload building by inspecting `BridgeEvent` logs post-execution. |
| **Legacy RPC** | The pre-migration chain endpoint (e.g., old Erigon/geth node) that serves historical data for blocks before the cutoff. |
| **Full-Link Monitor** | Observability system tracking transaction lifecycle events: receive, execute, commit across sequencer and RPC modes. |
| **Sequencer Mode** | Node operation mode where the node produces blocks and flashblocks (`--xlayer.sequencer-mode`). |
| **RPC Mode** | Default node operation mode where the node follows the chain and serves user queries. |

## Flashblocks-Specific Terms

| Term | Definition |
|------|-----------|
| **Sequence Manager** | Ring buffer (capacity 3) that tracks recently completed flashblock sequences for build scheduling. Lives in `crates/flashblocks/src/cache.rs`. |
| **Build Ticket** | Opaque identifier for a snapshot of a sequence targeted by a build job. Used to match build results to the correct sequence state. |
| **State Epoch** | Monotonically increasing counter that invalidates stale build results when canonical chain state changes (e.g., reorg). |
| **Canonical Anchor Hash** | The hash of the canonical block that a pending/speculative block's state is rooted in. Used for state lookups instead of the pending block hash. |
| **Speculative Build** | Building a new block on top of a pending (not-yet-canonical) parent to avoid waiting for P2P finalization. |
| **Transaction Cache** | Per-block cache (`TransactionCache`) storing cumulative execution state. Enables prefix-based cache reuse when new flashblocks extend the transaction list. |
| **Pending State Registry** | LRU cache (`PendingStateRegistry`, max 64 entries) of recently built pending block states for speculative chaining. |
| **Builder Transaction** | Special transaction injected by the builder (e.g., flashblock number increment via contract call). Types: `Base` and `NumberContract`. |

## Infrastructure Terms

| Term | Definition |
|------|-----------|
| **FCU** | Fork Choice Updated — engine API call (`engine_forkChoiceUpdated`) that drives chain head progression. |
| **newPayload** | Engine API call (`engine_newPayload`) that submits a new execution payload for validation. |
| **P2P Broadcast** | libp2p-based network for distributing flashblock payloads between sequencer and followers using the `FLASHBLOCKS_STREAM_PROTOCOL`. |
| **WebSocket Publisher** | TCP-based WebSocket server that broadcasts flashblock payloads to subscribers (e.g., RPC nodes). |
| **Tower Layer** | Middleware pattern from the Tower crate used for RPC request interception (legacy routing, monitoring). |
| **Static Files** | Reth's immutable segment storage for historical data (transactions, receipts, change sets). |
| **MDBX** | The default key-value database engine used by Reth (Lightning Memory-Mapped Database). |
| **RocksDB** | Alternative storage backend; X Layer migrates specific tables from MDBX to RocksDB for performance. |

## Hardfork Names

| Hardfork | Origin | Notes for X Layer |
|----------|--------|-----------------|
| **Bedrock** | OP Stack | First OP hardfork; activated at genesis for X Layer |
| **Regolith** through **Isthmus** | OP Stack | All activated at genesis (timestamp 0) |
| **Jovian** | OP Stack | First hardfork with non-zero activation timestamp on X Layer networks |
| **Prague** | Ethereum L1 | EVM changes inherited via OP Stack; activated at genesis for X Layer |
