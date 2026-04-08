---
name: "knowledge-base"
description: "Highest-authority rules — all Skills defer to this on conflicts"
---
# Knowledge Base — Authoritative Rules and Constraints

> This is the highest-weight file in the knowledge base. All Skills that support
> context-kg defer to this file when conflicts arise with general AI knowledge.

This document defines the highest-priority rules governing the X Layer Reth codebase. When in conflict with other documents, this file takes precedence.

## Data Type Constraints

[Rule] `bin/node/src/main.rs`: XLayerEngineValidatorBuilder must be set with engine_validator OnceLock before extending RPC modules — Reason: flashblocks RPC service requires initialized engine_validator
[Rule] `crates/flashblocks/src/cache/mod.rs`: FlashblockStateCache must use Arc<RwLock> for thread-safe access across async tasks — Reason: multiple handlers (incoming, execution, canonical) access cache concurrently
[Rule] `crates/flashblocks/src/execution/mod.rs`: PrefixExecutionMeta must be keyed by payload_id; new payload_id invalidates cache — Reason: different payloads have different transaction sets
[Rule] `crates/builder/src/flashblocks/utils/cache.rs`: Parent hash must be set once (from first base payload) and never overwritten; validate sequential payload indexes with explicit None return on gaps — Reason: ensures sequence integrity

## Naming Constraints

[Rule] `crates/rpc/src/default.rs`: RPC extension traits must use jsonrpsee `#[rpc(server, namespace = "...")]` macro with `#[method(name = "...")]` for each endpoint — Reason: consistent RPC registration pattern
[Rule] `crates/rpc/src/helper.rs`: All conversion helpers must be pub(crate) functions using `to_rpc_*` naming prefix and returning Result types — Reason: consistent conversion function naming
[Rule] `crates/builder/src/traits.rs`: All node type bounds must be defined as empty trait impls wrapping FullNodeTypes/FullNodeComponents — Reason: centralized trait bound management

## Dependency Constraints

[Rule] Extend, Never Fork Upstream: All customization must be done through trait implementations, composition, and extension points — never by modifying upstream Reth/OP-Reth code directly — Reason: maintainability across upstream version updates
[Rule] `bin/node/src/main.rs`: FlashblocksRpcService.spawn_rpc() must be called after payload builder setup to ensure engine_validator is initialized — Reason: OnceLock ordering dependency
[Rule] `crates/flashblocks/src/service.rs`: FlashblocksRpcService requires engine_validator to be fully initialized and OverlayProviderFactory to be available on Provider type — Reason: type bounds and generic constraints
[Rule] `crates/builder/src/broadcast/mod.rs`: Message broadcast to P2P peers must complete before WebSocket publish — Reason: ordering constraint for consistency
[Rule] `crates/builder/src/broadcast/mod.rs`: Node must never wait for acknowledgment from peers; failures are logged but don't block — Reason: non-blocking broadcast for latency
[Rule] `bin/node/src/payload.rs`: XLayerPayloadServiceBuilder must only apply bridge_config to Flashblocks variant; Default variant ignores intercept config — Reason: bridge intercept is sequencer-only

## Security Constraints

[Rule] `crates/legacy-rpc/src/service.rs`: parse_block_param() must validate block hashes using is_valid_32_bytes_string() to prevent JSON injection attacks — Reason: block hash values are interpolated into JSON-RPC strings
[Rule] `crates/intercept/src/lib.rs`: intercept_bridge_transaction_if_need() must check config.enabled first; must validate log address matches bridge_contract_address before parsing BridgeEvent — Reason: early returns for safety
[Rule] Bridge Intercept Is Payload-Builder-Only: Bridge transaction interception operates only within the payload builder (sequencer mode). Does not affect transaction pool, RPC forwarding, or block validation on followers — Reason: intercepted transactions are silently excluded from built payload only
[Rule] Monitor Events Are Fire-and-Forget: The monitoring system must never affect node behavior. Monitor failures must be logged and ignored — Reason: observability must not impact consensus
[Rule] No Panics at Runtime: All runtime code must use Result/Option. Only `.expect("validated")` after successful validation is acceptable — Reason: stability
[Rule] WebSocket publisher has no authentication — relies on network-level access control — Reason: designed for internal/trusted network deployment

## Flashblocks Invariants

[Rule] `crates/flashblocks/src/cache/mod.rs`: Pending height must equal confirm_height + 1; gaps cause rejection — Reason: height continuity ensures cache coherence
[Rule] `crates/flashblocks/src/cache/mod.rs`: On reorg or hash mismatch: full flush (pending=None, confirm cleared, confirm_height=canon_height, task_queue flushed) — Reason: stale data must be completely purged
[Rule] `crates/flashblocks/src/execution/engine.rs`: XLayerEngineValidator must hold Mutex preventing concurrent state-root computation between engine and flashblocks validators — Reason: prevent race conditions in validation
[Rule] `crates/flashblocks/src/execution/engine.rs`: XLayerEngineValidator uses blocking_lock() on dedicated OS thread — Reason: prevent deadlock, ensure single-thread execution for engine validation
[Rule] `crates/rpc/src/default.rs`: DefaultRpcExt.flashblocks_enabled() must check cache is not None AND confirm_height > 0 — Reason: dual condition ensures flashblocks state is actively receiving data

## Hardfork Rules

[Rule] `crates/chainspec/src/lib.rs`: Hardfork activation order is sacred — must maintain strict ordering matching upstream OP-Reth — Reason: consensus compatibility
[Rule] All hardforks up to Isthmus are activated at genesis for X Layer networks. Only Jovian and future forks have non-zero activation timestamps — Reason: chain specification

## Configuration Rules

[Rule] `crates/version/src/lib.rs`: init_version!() must be called exactly once at the start of every binary's main() function — Reason: global version metadata initialization
[Rule] `bin/node/src/args.rs`: XLayerArgs.validate() must be called before creating payload builder — Reason: all configurations must be consistent before use
[Rule] Task Spawning Discipline: Critical tasks use spawn_critical (node shuts down on panic); background tasks use spawn (failures logged but non-fatal). All spawned tasks must be named — Reason: operational safety
