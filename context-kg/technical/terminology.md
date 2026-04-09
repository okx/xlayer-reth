---
name: "terminology"
description: "Domain term glossary for unified terminology across backend skills"
---
# Terminology

## Core Concepts

| Term | Definition |
|------|-----------|
| **Flashblock** | A partial/incremental block (`OpFlashblockPayload`) produced during a single slot before the full block is finalized; contains a subset of transactions. |
| **Flashblock Sequence** | An ordered collection of flashblocks (index 0..N) sharing the same block number and payload ID. Managed as `PendingSequence` (in-progress) or stored in `ConfirmCache` (completed). |
| **Canonical Block** | A block that has been finalized by the consensus engine and is part of the canonical chain. Distinct from pending/speculative blocks. |
| **Pending Block** | A block being built from accumulated flashblocks. Represented as `PendingSequence<N>` wrapping Reth's `PendingBlock` with flashblock metadata. |
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
| **FlashblockStateCache** | Top-level controller state cache managing pending and confirmed flashblock sequences with Arc<RwLock> thread safety. Pure data store, not a provider. Lives in `crates/flashblocks/src/cache/mod.rs`. |
| **PendingSequence** | In-progress flashblock sequence being built from incoming OpFlashblockPayload deltas. Wraps `PendingBlock` with HashMap tx_index for O(1) lookups. At most one active at a time. |
| **ConfirmCache** | Completed flashblock sequences committed but still ahead of canonical chain. Stored in BTreeMap keyed by block number. Enables O(log n) range splits for efficient flush. |
| **RawFlashblocksCache** | Ring buffer cache (capacity 50) accumulating raw incoming flashblocks before execution. Tracks next buildable sequence and detects reorgs. |
| **ExecutionTaskQueue** | BTreeSet-backed async task queue with Arc<Notify> signaling. Enqueues heights for execution; evicts lowest height on capacity overflow. |
| **FlashblockSequenceValidator** | Builds PendingSequences from accumulated flashblock transactions. Supports 3 state root computation strategies (StateRootTask, Parallel, Synchronous). Uses Reth's PayloadProcessor for optimal execution. |
| **XLayerEngineValidator** | Unified controller holding both engine validator and flashblocks validator. Guards against payload validation races from CL/EL sync and flashblocks stream. Uses `blocking_lock()` Mutex for engine thread safety. |
| **PrefixExecutionMeta** | Cached prefix execution data for resuming builds. Stores payload_id, cached_reads, tx_count, gas_used. Keyed by payload_id — new payload_id invalidates cache. |
| **StateRootStrategy** | Enum describing state root computation method: StateRootTask (concurrent), Parallel, Synchronous (fallback). |
| **XLayerFlashblockPayload** | OpFlashblockPayload wrapped with target_index indicating builder target. Default target_index=0 for base payload. Enables incremental flashblock building. |
| **XLayerFlashblockMessage** | Untagged enum representing either Flashblock Payload or PayloadEnd signal. Used in P2P broadcast. Handles end-of-sequence signaling. |
| **XLayerFlashblockEnd** | Payload ID marking end-of-sequence. No more flashblocks for current block after this signal. |
| **CachedTxInfo** | Per-transaction cache entry containing block context (number, hash, tx_index), receipt, and transaction data for O(1) lookups by hash. |
| **FlashblockScheduler** | Schedules and triggers flashblock builds at predetermined times during a block slot. Calculates target flashblocks and send times. |
| **Builder Transaction** | Special transaction injected by the builder (e.g., flashblock number increment via contract call). Types: `Base` and `NumberContract`. |
| **MemoryOverlayStateProvider** | State provider that overlays in-memory changes on disk state for flashblock execution without disk writes. |

## Infrastructure Terms

| Term | Definition |
|------|-----------|
| **FCU** | Fork Choice Updated — engine API call (`engine_forkChoiceUpdated`) that drives chain head progression. |
| **newPayload** | Engine API call (`engine_newPayload`) that submits a new execution payload for validation. |
| **P2P Broadcast** | libp2p-based network for distributing flashblock payloads between sequencer and followers using stream protocol "/flashblocks/2.0.0". |
| **WebSocket Publisher** | TCP-based WebSocket server that broadcasts flashblock payloads to subscribers (e.g., RPC nodes). |
| **Tower Layer** | Middleware pattern from the Tower crate used for RPC request interception (legacy routing, monitoring). |
| **Static Files** | Reth's immutable segment storage for historical data (transactions, receipts, change sets). |
| **MDBX** | The default key-value database engine used by Reth (Lightning Memory-Mapped Database). |
| **BridgeEvent** | Log event emitted by bridge contract with signature 0x50178120... containing leafType, originNetwork, originAddress, amount. |
| **OpDAConfig** | Data Availability configuration for payload building; defines max DA tx/block sizes. |
| **OpGasLimitConfig** | Gas limit configuration for payload building. |

## Hardfork Names

| Hardfork | Origin | Notes for X Layer |
|----------|--------|-----------------|
| **Bedrock** | OP Stack | First OP hardfork; activated at genesis for X Layer |
| **Regolith** through **Isthmus** | OP Stack | All activated at genesis (timestamp 0) |
| **Jovian** | OP Stack | First hardfork with non-zero activation timestamp on X Layer networks. Mainnet: 1764691201, Testnet: 1764327600. Activates DA footprint scalar computation. |
| **Prague** | Ethereum L1 | EVM changes inherited via OP Stack; activated at genesis for X Layer |
