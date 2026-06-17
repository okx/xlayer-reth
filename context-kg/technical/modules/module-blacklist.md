# Module: xlayer_blacklist

**Path**: `crates/blacklist/` — single crate, two layers (`rules` + `runtime`)
**Purpose**: Chain-level "emergency freeze" blacklist interception (XLOP-1100). Addresses on the
list cannot have asset transfers (native ETH, ERC20/721/1155, proxy multi-hop, parameterized
transferFrom/permit, selfdestruct, L1 forced deposit) succeed on L2. op-reth must produce a
state-root- and field-identical-receipt result with op-geth for the same block, or it forks.

> **Status (xl/blacklist_v1 — wired, compiles per-crate + full node).** Reduced cross-client to a
> **two-check** execution gate (Transfer logs + native-ETH balance); **no check① (committed CALL
> touch)** and **no ingress mempool filter** — the execution gate is the sole interception point.
> - FR-4 block-head snapshot read: builder `execute_pre_steps` (sequencer) + the follower
>   executor's `apply_pre_execution_changes`, both populate the shared `ArcSwap` snapshot.
> - FR-2 sequencer face: two checks (log + balance) in `flashblocks/context.rs` +
>   `default/builder.rs`; balance reconstructed from the post-state diff; **no revm inspector is
>   mounted** (check① removed → zero EVM-inspection overhead).
> - FR-3 deposit included-as-reverted: sequencer (`execute_sequencer_transactions`) + follower
>   (deps/optimism `OpBlockExecutor` deposit hook) both apply the **same** plan computed by the
>   shared `rules::deposit::apply_included_as_reverted` (single source of truth) → byte-identical
>   post-state.
> - FR-5 decision B (extended): deposits AND normal L2 txs are judged on log + balance only
>   (check① dropped cross-client). **op-geth must drop check① + ingress to match** — this is a
>   cross-client contract change (see PRD / [[project_blacklist-alignment-spec]]).
>
> Controlled submodule fork (deps/optimism, ~50 lines, no public assoc-type change): the
> `DepositBlacklistHook` trait (returns the revert plan) + an `OpBlockExecutor` deposit-hook field
> + an `OpEvmConfig` optional ctx field. Produces the SAME `OpEvmConfig` type — see
> [[upstream-component-type-pinning]]. Remaining: shared adversarial e2e vectors before mainnet
> enablement; 1952/196 mirror addresses are `TODO(XLOP-1071)` placeholders (fail-open no-op).

## Structure (`crates/blacklist/src/` — 3 files)

| File | Layer | Contents |
|------|-------|----------|
| `lib.rs` | entry | `mod rules; mod runtime;` + stable re-exports |
| `rules.rs` | cross-client pure logic (zero reth/revm coupling) | inner mods: `abi` (`getBlacklist` via alloy `sol!`) · `snapshot` (block-head paged read + fail-open) · `inspector` (balance candidates) · `eval` (two checks **log > balance**) · `deposit` (included-as-reverted field rules + exempt senders) · `mirror` (`chain_id → address`) · `metrics` |
| `runtime.rs` | node/revm/reth adapters | `BlacklistRuntimeCtx` (chain dispatch + ArcSwap snapshot + metrics) + inner mods: `balance` (fee-delta reconstruction) · `deposit_apply` (sender state delta) · `view` (50M-gas mirror staticcall) · `follower_hook` (`DepositBlacklistHook` impl — computes the plan via the shared core) · `executor_builder` (installs the hook, same `OpEvmConfig` type) |

## Key Design Decisions

1. **Two-check gate (log + balance).** check① (committed CALL touch) was removed cross-client: a
   real asset transfer always trips the Transfer-event or ETH-balance check, and dropping check①
   removes the only thing that forced a revm inspector onto the build path (performance) and the
   one OOG/halt-frame edge case that diverged from op-geth.

2. **Single crate, builder-field threading — NOT component wrapping.** `blacklist_ctx:
   Option<BlacklistRuntimeCtx>` is a field threaded `payload.rs → service.rs → builder.rs →
   context.rs` (set via `with_blacklist_ctx`), paralleling `bridge_intercept_config`. Wrapping
   pool/executor/EVM types does **not** compile against upstream op-reth — see
   [[upstream-component-type-pinning]]. The follower/executor faces are reached via a controlled
   submodule fork (optional fields + a deposit hook), never a wrapper.

3. **Deposit disposition is single-sourced.** `rules::deposit::apply_included_as_reverted` computes
   the full revert plan (status=0, gasUsed=gasLimit, DepositNonce=N, account nonce=N+1, keep-mint,
   Canyon version). Both the sequencer (`context.rs`) and the follower (submodule hook →
   `runtime::follower_hook`) apply that one plan; the submodule executor only fills the values, it
   recomputes nothing. This removes the prior two-copies divergence risk.

4. **Block-boundary list semantics (cross-client).** Snapshot read once at block head from parent
   state; not block-internal live state. `add(0xAAA)` in block N takes effect from N+1.

5. **Fail-open is consensus-safe.** Any view/decode failure / unmapped chain / mirror-with-no-code →
   empty list → full no-op. Both clients read the same parent state, so failure is deterministic.

6. **No ingress mempool filter.** A committed hit on a normal L2 tx is dropped on the build path
   (`mark_invalid` + physical pool eviction); deposits go included-as-reverted on all paths. There
   is no admission-time filter — the execution gate is the sole, sufficient interception point.

## Constraints Honored

- `[Rule] Extend, Never Fork Upstream`: the only upstream change is the controlled deps/optimism
  fork (deposit hook + optional fields, default no-op, no public assoc-type change).
- `[Rule] No Panics at Runtime`: Result/Option, fail-open, no `.unwrap()` in prod paths.
- Cross-client constants single-sourced in `rules`; the `runtime` adapters reference them.
