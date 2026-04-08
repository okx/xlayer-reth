# Knowledge Base — Authoritative Rules and Constraints

This document defines the highest-priority rules governing the X Layer Reth codebase. When in conflict with other documents, this file takes precedence.

## Rule 1: Extend, Never Fork Upstream

X Layer Reth builds on top of Reth and OP-Reth. All customization MUST be done through trait implementations, composition, and extension points — never by modifying upstream code directly.

- Payload building: implement `PayloadServiceBuilder` trait (see `XLayerPayloadServiceBuilder`)
- RPC extensions: use `extend_rpc_modules` callback, not patching core RPC
- Chain specs: implement `ChainSpecParser` trait (see `XLayerChainSpecParser`)
- Middleware: use Tower layers for RPC interception (see `LegacyRpcRouterLayer`, `RpcMonitorLayer`)

## Rule 2: Hardfork Activation Order Is Sacred

Hardfork definitions in `crates/chainspec/src/lib.rs` MUST maintain strict ordering matching upstream OP-Reth. The Ethereum and OP hardforks follow a fixed sequence:

```
Frontier → Homestead → Tangerine → SpuriousDragon → Byzantium → Constantinople →
Petersburg → Istanbul → MuirGlacier → Berlin → London → ArrowGlacier → GrayGlacier →
Paris → Bedrock → Regolith → Shanghai → Canyon → Cancun → Ecotone → Fjord →
Granite → Holocene → Prague → Isthmus → Jovian
```

All hardforks up to Isthmus are activated at genesis (block 0 / timestamp 0) for X Layer networks. Only Jovian and future forks have non-zero activation timestamps.

## Rule 3: Flashblocks Invariants

- Flashblock sequences MUST be sequential: index 0 is the base, subsequent flashblocks have index+1.
- A new block MUST start with index 0 and a new payload ID.
- The `FlashBlockCompleteSequence` is immutable once finalized — no modifications allowed.
- Transaction caching relies on prefix matching: the incoming transaction list MUST be an extension of the cached prefix for cache reuse.
- Speculative builds use `canonical_anchor_hash` for state lookups, NOT the pending block hash.

## Rule 4: Legacy RPC Cutoff Block Semantics

- The cutoff block equals the genesis block number of the current chain.
- Blocks BELOW the cutoff are routed to the legacy endpoint.
- Blocks AT or ABOVE the cutoff are served locally.
- `latest`, `pending`, `safe`, `finalized` tags are NEVER routed to legacy.
- `earliest` is ALWAYS routed to legacy (local chain has no pre-genesis data).
- Block hash parameters require an extra `eth_getBlockByHash` lookup to determine the block number before routing.

## Rule 5: Bridge Intercept Is Payload-Builder-Only

Bridge transaction interception operates ONLY within the payload builder (sequencer mode). It does NOT affect:
- Transaction pool acceptance
- RPC transaction forwarding
- Block validation on follower nodes

Intercepted transactions are silently excluded from the built payload. The interception checks `BridgeEvent` logs AFTER EVM execution, not before.

## Rule 6: Monitor Events Are Fire-and-Forget

The monitoring system (`XLayerMonitor`) records events for observability but MUST NOT affect node behavior. Monitor failures (e.g., trace file write errors) must be logged and ignored — never propagated as node errors.

## Rule 7: Version Metadata Initialization

`xlayer_version::init_version!()` MUST be called exactly once at the start of every binary's `main()` function, before any Reth CLI parsing. It initializes global version metadata that is used throughout the application lifecycle.

## Rule 8: Configuration Validation Order

X Layer args validation follows a strict order in `XLayerArgs::validate()`:
1. Legacy RPC args (URL format, timeout bounds)
2. Monitor args (path validity)
3. Bridge intercept args (address format, required fields when enabled)

Validation failures cause immediate process exit with a descriptive error message.

## Rule 9: No Panics at Runtime

- CLI argument parsing may panic (via `clap`) — this is acceptable.
- All runtime code MUST use `Result`/`Option` and propagate errors.
- The only exception: `.expect("validated")` after successful validation in `XLayerInterceptArgs::to_bridge_intercept_config()`, which is guarded by a preceding `validate()` call.

## Rule 10: Task Spawning Discipline

- Critical tasks (payload building, consensus, monitoring) use `spawn_critical` — the node shuts down if they panic.
- Background tasks (metrics, WebSocket publishing) use `spawn` — failures are logged but non-fatal.
- All spawned tasks must be named for debugging (e.g., `"xlayer-flashblocks-service"`).
