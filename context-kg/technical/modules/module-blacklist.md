# Module: xlayer_blacklist / xlayer_blacklist_node

**Paths**: `crates/blacklist/` (cross-client core) · `crates/blacklist-node/` (node-side adapters)
**Purpose**: Chain-level "emergency freeze" blacklist interception (XLOP-1100). Addresses on the
list cannot have asset transfers (native ETH, ERC20/721/1155, proxy multi-hop, parameterized
transferFrom/permit, selfdestruct, L1 forced deposit) succeed on L2. op-reth must produce a
state-root- and field-identical-receipt result with op-geth for the same block, or it forks.

> **Status (as of 2026-06-12, xl/blacklist_latest — wired, compiles end-to-end).** All faces are
> now wired and the full node compiles:
> - FR-4 block-head snapshot read: `builder::execute_pre_steps` (sequencer) + the follower
>   executor's `apply_pre_execution_changes` both populate the shared `ArcSwap` snapshot.
> - FR-2 sequencer face: three checks (call/log/balance) live in `flashblocks/context.rs`
>   (inspector mounted via `evm_with_env_and_inspector`, balance reconstructed from the state diff).
> - FR-3 deposit included-as-reverted: sequencer (`execute_sequencer_transactions`) + follower
>   (deps/optimism `OpBlockExecutor` deposit hook), byte-identical post-state.
> - FR-1 ingress: blacklist filter on the standard `OpTransactionValidator` (field, not wrapper).
> - FR-5 cross-client decision **B**: deposits judged on check②+③ only (no check① — follower EVM
>   cannot mount an inspector); op-geth must drop deposit check① to match. See
>   [[project_blacklist-alignment-spec]] and IMPL-blacklist-full-alignment.
>
> The type-pinning wall ([[upstream-component-type-pinning]]) is avoided by adding optional
> `Option<Arc<dyn …>>` fields to the upstream types (OpBlockExecutor / OpTransactionValidator) and
> producing the SAME `OpEvmConfig` / pool types — NOT wrapping them. Remaining: end-to-end e2e
> vector validation (shared adversarial vectors) before mainnet enablement.

## `crates/blacklist/` — Cross-Client Core (single source of truth for consensus constants)

Mirrors `crates/intercept`'s zero-internal-dependency shape. All cross-client consensus constants
are defined here exactly once.

| File | Responsibility |
|------|----------------|
| `mirror.rs` | `chain_id → hardcoded mirror address` (FR-6). 195/devnet defined; 1952/196 are `TODO(XLOP-1071)` placeholders (fail-open no-op until replaced) |
| `abi.rs` | `getBlacklist(uint256,uint256) -> (uint256,address[])` selector + encode/decode (FR-4) |
| `snapshot.rs` | `BlacklistSnapshot` + block-head staticcall paged enumeration + fail-open (FR-4) |
| `inspector.rs` | `BlacklistInspector` — records committed call-frame touches, balance candidates, selfdestruct |
| `eval.rs` | `BlacklistEvaluator` — three checks in fixed order **call > log > balance**; Transfer topic0 set + feeDelta exclusion set (cross-client constants) |
| `deposit.rs` | `apply_included_as_reverted` — keep-mint, nonce N+1, gasUsed=gasLimit, status=0, DepositNonce=N, empty logs (FR-3) |
| `metrics.rs` | `BlacklistMetrics` (`#[metrics(scope = "xlayer_blacklist")]`) |
| `error.rs` | `-32000` pool-reject code + fixed message (FR-7) |

## `crates/blacklist-node/` — Node/revm/reth Adapters

Wrapper *types* grounded in upstream signatures. **Not all are wired** (see Status):
`BlacklistRuntimeCtx` (runtime.rs, FR-6/7 — chain dispatch + ArcSwap snapshot handle + metric emit),
`XLayerBlacklistTxValidator` (validator.rs, FR-1 ingress), `XLayerRevmInspector` (inspector.rs),
`RethMirrorViewCaller` (view.rs, FR-4 live read), `XLayerBlacklistEvmConfig` (evm_config.rs),
`XLayerExecutorBuilder` (executor_builder.rs), `XLayerBlacklistPoolBuilder` (pool_builder.rs).

## Key Design Decisions

1. **Exclusion (deny-list) semantics, anchored on the bridge-intercept framework.** "Any committed
   touch of a listed address → hit" matches the post-execution log-inspection / exclude-tx pattern
   already shipped for bridge intercept (`context.rs:639`). The blacklist reuses that host call-site
   semantics, not any inclusion/eligibility gate.

2. **Wired via builder-field threading, NOT component wrapping.** `blacklist_ctx:
   Option<BlacklistRuntimeCtx>` is a field on `FlashblocksBuilderCtx`, threaded `payload.rs →
   service.rs → builder.rs → context.rs` (set via `with_blacklist_ctx`), exactly paralleling
   `bridge_intercept_config`. The originally-designed component wrappers (`.pool()`/`.executor()`/
   wrapper EVM) do **not** compile against upstream op-reth — see
   [[upstream-component-type-pinning]]. **Do not** re-attempt the wrapper approach.

3. **Live hook (only thing currently wired).** FR-2/3a L2-normal drop in
   `crates/builder/src/flashblocks/context.rs::execute_best_transactions` (≈`:653-673`), immediately
   after the bridge check: on a `BlacklistEvaluator::evaluate(...)` hit → `record_exec_revert(&hit)`
   + `best_txs.mark_invalid(...)` + `continue` (state not committed). Empty snapshot = fail-open
   no-op. This is the sequencer/out-block face; the out-block client is the only interception point
   for normal L2 txs.

4. **Block-boundary list semantics (cross-client).** Snapshot read once at block head from parent
   state; not block-internal live state. `add(0xAAA)` in block N takes effect from N+1.

5. **Fail-open is consensus-safe.** Any view/decode failure / unmapped chain / mirror-with-no-code →
   empty list → full no-op. Both clients read the same parent state, so failure is deterministic.

## Remaining (not yet wired — consensus-critical, needs build-capable host + integration tests)

- **FR-4 live snapshot population**: per-block mirror read via a read-only EVM over parent state
  (`new_simulation_state` pattern) + `StateProvider` threaded into the builder ctx. Until wired the
  snapshot is empty and the L2 gate is fail-open. Reading via the *building* EVM risks consensus
  divergence — do not.
- **FR-2/3b deposit included-as-reverted** in `execute_sequencer_transactions` (see deposit-loop
  note in [[module-builder]]); **check①/③** require mounting `XLayerRevmInspector` on the builder EVM.
- **FR-1 ingress / FR-5 follower & flashblocks faces**: blocked by upstream type-pinning; would
  require a custom `OpAddOns` stack / engine-validator-builder cascade.

## Constraints Honored

- `[Rule] Extend, Never Fork Upstream`: 0 upstream lines changed; new crates + composition only.
- `[Rule] No Panics at Runtime`: Result/Option, fail-open, no `.unwrap()` in prod paths.
- Cross-client constants single-sourced in `crates/blacklist`; node crate references them.
