---
name: "upstream-component-type-pinning"
description: "op-reth node-component wrapping (pool/executor/EVM) is blocked by upstream type-pinning; extend node features via builder-field threading instead"
---
# Pitfalls: Upstream Component Type-Pinning

## 1. Wrapping Node Components Breaks the op-reth Add-Ons Stack

**Symptom**: A feature that needs per-transaction interception at the pool, executor, or EVM
layer is designed as a *wrapper component* (custom `PoolBuilder` installed via `.pool()`, a custom
`ExecutorBuilder` cascade via `.executor()`, or a wrapper `ConfigureEvm` such as
`XLayerBlacklistEvmConfig` threaded through the payload/validator stack). The crate-local types
look correct, but the full node fails to compile with errors like:

- `E0277: OpAddOns<…>: NodeAddOns<…> is not satisfied … expected OpTransactionValidator<_, OpPooledTransaction, OpEvmConfig>, found <wrapper>` at `with_add_ons`.
- `E0308: expected OpEvmConfig, found XLayerBlacklistEvmConfig<OpEvmConfig>` at `FlashblockSequenceValidator::new`.

**Root Cause**: Upstream op-reth pins concrete types across the add-ons / payload / engine-validator
stack:
- `OpAddOns`'s `NodeAddOns` impl is bound to the **exact** `OpTransactionValidator` pool type, so
  wrapping the validator changes `N::Pool` and the entire RPC/engine add-ons stack stops resolving.
- The RPC-node payload variant is `BasicPayloadServiceBuilder<OpPayloadBuilder>`, hardcoded to
  `OpEvmConfig` (`bin/node/src/payload.rs:108/117`, `crates/builder/src/flashblocks/service.rs`,
  `default/service.rs`, `bin/node/src/main.rs`). A wrapper EVM type cannot be threaded through it.
- `FlashblockSequenceValidator`'s `EvmConfig` is pinned by the `XLayerEngineValidator` instance
  (`OpEvmConfig`); wrapping triggers an engine-validator-builder cascade, not a local change.

**Mechanism**: These are *compiler-proven* structural blockers, not a-priori caution. They only
surface after compiling the full op-reth tree (~1197 crates), which OOM-kills a memory-limited
stage — so a design/spec written without a successful full-tree compile will not catch them.

**Rule**: To add per-tx interception or any node-wide behavior, do **not** wrap or replace upstream
node components (pool/executor/EVM type). Instead **thread your config/runtime context as a plain
builder field**, mirroring the shipped `xlayer_bridge_intercept` pattern: a config field on
`FlashblocksBuilderCtx` (`bridge_intercept_config`), set via a `with_*` builder method
(`with_bridge_config`), and consumed at a single post-execution hook in
`crates/builder/src/flashblocks/context.rs`. XLOP-1100 followed this: `blacklist_ctx:
Option<BlacklistRuntimeCtx>` threaded `payload.rs → service.rs → builder.rs → context.rs`, consumed
right after the bridge check in `execute_best_transactions`. This compiles and ships; the
component-wrapper approach does not. See [[module-blacklist]] and the bridge-intercept flow.

**Source**: review-finding F-01 (A-09 Test & Fix Report §2/§3, XLOP-1100). **Date**: 2026-06-11.
**Hit count**: 1.
