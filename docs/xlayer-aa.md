# XLayerAA — Implementation Notes

## Mistakes

Running log of mistakes made during XLayerAA implementation, so we don't repeat them. Append new entries at the top.

### 2026-04-22 — E4a's "swap EvmConfig inside payload body" trick broke on the executor path

E4a flipped the default builder to `XLayerEvmConfig` via a narrow trick: keep the outer `PayloadServiceBuilder` type param as upstream `OpEvmConfig` (for uniform enum dispatch) and ignore the `_: OpEvmConfig` argument inside the impl body, constructing `XLayerEvmConfig` locally. That worked because nothing downstream inspected the `PayloadServiceBuilder`'s EVM type param — payload builders are self-contained.

The moment we tried to do the same for the executor side (peer block import / engine `newPayload` / state sync), the trick collapsed. reth's [`NodeComponentsBuilder`](https://github.com/paradigmxyz/reth/blob/main/crates/node/builder/src/components/builder.rs) bound is:

```rust
PoolB: PoolBuilder<Node, ExecB::EVM, ...>,
PayloadB: PayloadServiceBuilder<Node, PoolB::Pool, ExecB::EVM>,
```

The **pool and payload builders carry `ExecB::EVM` as a type parameter**. Swapping the executor to produce `OpEvmConfig<..., XLayerAAEvmFactory<...>>` means every downstream component inherits the new EVM type and must impl its trait for it. The cascade we hit:

1. `XLayerPayloadServiceBuilder` / `DefaultBuilderServiceBuilder` / `FlashblocksServiceBuilder` impls had to flip from `PayloadServiceBuilder<_, _, OpEvmConfig>` to `PayloadServiceBuilder<_, _, XLayerEvmConfig>`.
2. `op-reth`'s `ConfigureEngineEvm for OpEvmConfig<ChainSpec, N, R>` (engine API `newPayload`) was pinned to the default `EvmFactory = OpEvmFactory<OpTx>` — had to patch the upstream impl (in our `deps/optimism` submodule) to accept an arbitrary `EvmF` generic. Body is EvmFactory-agnostic, so purely a bound-widening.
3. `OpTransactionRequest: TryIntoTxEnv<_>` was only impl'd for upstream `OpTransaction<TxEnv>` — had to add a delegating impl in `xlayer-revm` under a new `rpc` feature (off by default so no_std prover consumers stay lean).
4. `bin/node/src/main.rs` relied on `op_node.add_ons()`, which returns `Self::AddOns` with `NodeAdapter<_, DefaultComponents>` — i.e. with the upstream `OpExecutorBuilder`'s EVM baked in. Had to replace with `op_node.add_ons_builder::<Optimism>().build::<_, PVB, EB, EVB>()` so `N` is generic and Rust infers it as our XLayer-flavored NodeAdapter.
5. `bin/node/src/main.rs:211`'s `FlashblockSequenceValidator::new(OpEvmConfig::optimism(...), ...)` had to become `xlayer_evm_config(...)` — `set_flashblocks`'s signature requires `FlashblockSequenceValidator<_, Evm, _, _>` to share `Evm` with `XLayerEngineValidator<_, Evm, _, _, _>`, which is bound to `Node::Evm`.
6. `.with_components(op_node.components().payload(pb).executor(xb))` — **order matters**: `.executor(...)` must fire before `.payload(...)`, because each method-chain step type-checks the builder it receives against the current `ExecB::EVM`. Swapping executor last means the payload bound is checked against the stale upstream `OpEvmFactory<OpTx>` EVM.

**Why it's wrong to assume "narrow fix".** When I first scoped the executor task, I thought it was `XLayerExecutorBuilder` + one line in `main.rs`. The real shape is: reth couples pool / payload / executor / add-ons / engine validator / RPC (`RpcConvert`, `TryIntoTxEnv`) through the same EVM type parameter, so flipping the executor's EVM forces every consumer to align. "Narrow" is a local illusion.

**Fix:** do all six bullets in one commit. No way to split cleanly — each on its own causes the chain to fail type-checking.

**Lesson:** for any node-level trait that threads `Self::EVM` (or `Self::Primitives`, or similar "wide" associated type) through a components chain, a minimal local swap at one end propagates through the whole graph. Before scoping, grep for every place that carries the EVM type as a generic — that's the actual blast radius.

### 2026-04-21 — `XLayerEvmConfig` pinned to `OpChainSpec` without a wrapper escape hatch

Initial E4a wired `XLayerEvmConfig` as:

```rust
pub type XLayerEvmConfig = OpEvmConfig<
    OpChainSpec,                            // ← hard-coded
    OpPrimitives,
    OpRethReceiptBuilder,
    XLayerAAEvmFactory<XLayerAATransaction<TxEnv>>,
>;
```

XLayer's hardforks currently live as module-level `const`s in `crates/chainspec/src/lib.rs` (e.g. `XLAYER_MAINNET_JOVIAN_TIMESTAMP`) and everything types through as `Arc<OpChainSpec>`. That works *today* because every XLayer fork still maps onto an `OpHardfork` variant. It stops working the moment we add an XLayer-exclusive fork (first candidate: `XLayerHardfork::XLayerAA` at M3) — `OpHardfork` has no variant for it, and the `OpChainSpec`-typed context can't answer `is_xlayer_aa_active_at_timestamp(ts)`.

**Why leaving it as-is is a trap:** the cost of the wrapper grows with every additional downstream crate that inherits the `Arc<OpChainSpec>` type (pool validator, RPC filler, legacy-rpc). Retrofitting later means chasing every transitive type param.

**Fix (deferred to M3, TODO in place):** introduce `XLayerChainSpec` as a newtype wrapping `Arc<OpChainSpec>` + XLayer-specific fork metadata, impl `OpHardforks` + `EthChainSpec<Header = Header>` (the only trait bounds `OpEvmConfig` requires), and flip the alias to `OpEvmConfig<XLayerChainSpec, …>`. Follow tempo's `TempoChainSpec` / `TempoHardforks` split.

**Why not now:** we only have one XLayer fork in flight (AA), so introducing the wrapper before it has a second user would add indirection without payoff. The TODO comment on the type alias records the plan so whoever lands M3 doesn't have to re-derive it.

**Lesson:** "deferred but documented" beats both "premature abstraction" and "discover it when the ceiling caves in". A forward-pointing TODO with a concrete migration sketch is cheap to write now and expensive to reconstruct later.

### 2026-04-21 — Intrinsic gas constants hard-coded in `build_aa_parts` with no fork binding

First cut of `build_aa_parts` read `AA_BASE_COST`, `EOA_AUTH_GAS`, `NONCE_KEY_COLD_GAS` directly from module-level `const`s and took no `OpSpecId`:

```rust
pub fn build_aa_parts(tx: &TxEip8130) -> Result<BuiltAaParts, BuildError> {
    let verification_gas = if tx.is_eoa() { EOA_AUTH_GAS } else { 0 };
    let nonce_cost = if tx.nonce_key == NONCE_KEY_MAX { 0 } else { NONCE_KEY_COLD_GAS };
    let aa_intrinsic_gas = AA_BASE_COST.saturating_add(verification_gas).saturating_add(nonce_cost);
    // ...
}
```

**Why it's wrong:** AA pricing is *going* to change — verifier costs get re-benched, new native algorithms land, storage-layout tweaks invalidate old SSTORE assumptions. Without a fork-bound schedule the first revision becomes a codebase-wide `const` rename + call-site churn, and downstream callers (pool validator, RPC filler, handler) can silently drift from consensus gas. Base and Tempo both fixed this up front — Base via `VerifierGasCosts::BASE_V1`, Tempo via `spec.is_t1b()` branches inside `key_auth_gas`.

**Fix:** introduce [`XLayerAAGasSchedule`](../crates/xlayer-consensus/src/aa/gas_schedule.rs) — a pure fee table with one `pub const` per fork (`XLAYER_AA` today) and a `for_spec(OpSpecId)` resolver. Threaded `OpSpecId` through `build_aa_parts(tx, spec)`; callers (handler tx-env conversion, pool validator, RPC filler) all already have `ctx.cfg().spec()`.

**Lesson:** any consensus-critical table (gas, slot layouts, feature flags) must be fork-indexed **from the first implementation** — retrofit is always more painful than paying the extra argument up front. Fork naming matches the XLayerAA hardfork (`XLAYER_AA`, `XLAYER_AA_V2`, …), **not** the underlying OP fork (Jovian, etc.) — OP forks don't carry AA pricing changes and coupling the two would mean bumping the schedule on every OP upgrade.

### 2026-04-21 — `crates/xlayer-revm` initially required `std`, blocking fault-proof reuse

The first port pulled in `std` through several avenues:

- `std::sync::atomic::AtomicBool` for the `ACCOUNT_CONFIG_DEPLOYED` cache (unavailable under `alloc` — `sync::atomic` lives in `core`);
- `std::collections::HashMap` for the authorizer-chain pending-owner overlay (`alloc` has `BTreeMap`, not `HashMap`);
- `alloy-sol-types` with default features (defaults to `std`);
- `format!` / `vec!` / `.to_string()` in error paths without `#[macro_use] extern crate alloc` (macros don't come through the `extern crate alloc as std` rename alone).

**Why it's wrong:** XLayerAA execution logic needs to cross into **fault-proof** / **ZK** prover programs (kona, op-program, custom provers), which build for MIPS / RISC-V targets with `no_std`. A handler that requires `std` cannot be embedded in those environments — the chain's validity-prover path is dead on arrival.

**Fix:**

- `core::sync::atomic::{AtomicBool, Ordering}` (not `std::sync::atomic`).
- `alloc::collections::BTreeMap` for the pending-owner overlay; access patterns (insert / get / iterate in order) map cleanly, cardinality is bounded (≤ `MAX_ACCOUNT_CHANGES_PER_TX`), and it drops one transitive dep (`hashbrown`).
- `alloy-sol-types = { default-features = false }` + gate `alloy-sol-types/std` inside our crate's `std` feature.
- `#[macro_use] extern crate alloc as std;` at lib.rs root so `format!` / `vec!` / `write!` macros work; add explicit `use std::string::ToString;` at call sites that need `.to_string()`.

Smoke-test: `cargo build -p xlayer-revm --no-default-features` + `cargo build -p xlayer-revm` + `cargo test -p xlayer-revm --lib` all green.

**Lesson:** any execution-critical crate (handler, precompiles, config-layout helpers) must build `--no-default-features` from day one — retrofitting later means chasing every transitive dep and every stray `format!`. Rules of thumb:

- Prefer `core::` paths over `std::` / `alloc::` where both exist (atomics, `cmp`, `mem`, `num`, `str`, `result`).
- Collection choice: if cardinality is small and order-agnostic, `BTreeMap` / `BTreeSet` beat `HashMap` / `HashSet` for no_std reach (alloc ships them, `hashbrown` is another dep).
- Third-party deps: always check `default-features` — most alloy / revm crates default to `std` and need explicit gating.
- Macro surface: `#[macro_use] extern crate alloc;` at lib.rs once, rather than `alloc::format!(...)` at every call site.

### 2026-04-21 — No structural guard against multiple create entries in `XLayerAAParts`

EIP-8130 allows at most one create entry per transaction. `XLayerAAParts`
encodes delegation as `Option<Address>` (type-enforced "at most one"),
but create entries flow into `code_placements: Vec<XLayerAACodePlacement>`
with no size bound. If the evm-compat parts constructor regressed to
emit two placements, the handler would silently process both.

**Fix:** add a defensive `code_placements.len() > 1` check in `validate_env`
returning an `Err` (not `assert!`), and document the invariant on the
field itself. The delegation field got a matching doc note that makes
the type-enforced guarantee explicit.

**Lesson:** type-level invariants (`Option<T>`, newtype wrappers,
`[T; N]`) are always preferable to runtime checks; when a spec constraint
doesn't map to a type (bounded-cardinality `Vec`), document the invariant
where the field is declared AND enforce it once in the outermost
validation stage — don't rely on downstream code reading it correctly.

### 2026-04-21 — AA branch bypassed `chain_id` check

Upstream op-revm's `validate_env` delegates the EIP-155 `chain_id` check to `revm-handler`'s mainnet `validate_env`:

```rust
// op-revm handler.rs
fn validate_env(&self, evm: &mut Self::Evm) -> ... {
    if tx_type == DEPOSIT_TRANSACTION_TYPE { ... return Ok(()); }
    if tx.enveloped_tx().is_none() { return Err(MissingEnvelopedTx); }
    self.mainnet.validate_env(evm)  // ← does chain_id here
}
```

The XLayerAA `validate_env` returns early before the `self.op.validate_env(evm)` delegation, so mainnet never runs for AA txs and the `chain_id != cfg.chain_id` check was silently skipped. A cross-chain AA tx could pass stateless validation.

**Fix:** inline the same chain_id check at the end of the AA branch, gated by `ctx.cfg().tx_chain_id_check()`. Return `InvalidTransaction::InvalidChainId` on mismatch and `MissingChainId` when absent (AA is a custom tx type, not legacy).

**Lesson:** any new tx type whose `validate_env` branches out of the delegation chain inherits a debt: every universal check upstream enforces (`chain_id`, `gas_limit_cap`, etc.) must be re-checked locally. Don't assume "OP handled it" — verify by reading the upstream path.

### 2026-04-21 — Lock check only ran for config changes, not for delegation entries; and ran after gas deduction

EIP-8130 Block execution, step 1: *"If `account_changes` contains config change or delegation entries, read lock state for `from`. Reject transaction if account is locked."*

Two issues with the original placement:

1. Lock check was embedded inside `validate_config_change_preconditions` (called from `execution()`), so it fired **only when `sequence_updates` or `config_writes` were present**. A tx that carried a delegation entry on a locked account would skip the check entirely and the delegation would apply.
2. Even for config changes, the check happened inside `execution()`, after gas deduction in `validate_against_state_and_deduct_caller`. A locked account would pay for the rejection before being rejected.

**Fix:** extract `check_account_lock` into helpers and call it at the top of `validate_against_state_and_deduct_caller` (before gas deduction) whenever the tx carries a delegation entry OR config writes OR sequence updates. Remove the now-redundant check from `validate_config_change_preconditions`.

**Lesson:** when the spec says "step 1", do it in step 1 — pushing a validation deeper into execution can still be correct for *rejection* but alters *side effects* (here, gas deduction). And cross-reference: a check gated by predicate A must also cover predicate B if the spec lists both (`config change or delegation`).

### 2026-04-21 — `nonce_free_hash.unwrap_or_default()` collapses all `None` txs onto one replay slot

EIP-8130 does not mandate a specific replay-protection mechanism for nonce-free mode beyond the expiry window, but our implementation uses a ring buffer keyed by `nonce_free_hash`. The state-validation stage originally wrote:

```rust
let nf_hash = parts.nonce_free_hash.unwrap_or_default();
```

**Why it's wrong:** if `nonce_free_hash` arrives as `None`, every such tx shares the zero-hash slot — a single seen entry blocks all of them, or worse (depending on ordering) lets them all pass. Silently defaulting to zero is never the right behavior for a replay key.

**Fix:** reject `None` up front in `validate_env` for nonce-free txs, and use `.ok_or_else(...)?` (not `.unwrap`) at the state-validation site as belt-and-suspenders.

**Lesson:** `unwrap_or_default()` on identity-bearing fields (hashes, nonces, addresses) is almost always wrong. Default values collide; collisions on replay keys are catastrophic.

### 2026-04-21 — Missing structural checks for `nonce_key == NONCE_KEY_MAX`

EIP-8130: *"When `nonce_key == NONCE_KEY_MAX`, the protocol does not read or increment nonce state. `nonce_sequence` MUST be `0`. Replay protection relies on `expiry`, which MUST be non-zero."*

The handler's `validate_env` had neither check:

- `expiry == 0` was not rejected up front; it was only caught indirectly in `validate_against_state_and_deduct_caller` via `expiry <= now` (which happens to be true when `expiry == 0` since `now > 0`). This conflates a structural error with a time-window error and relies on coincidence.
- `nonce_sequence != 0` was never checked at all — the nonce-free branch in the state-validation stage does not read `tx.nonce()`, so a nonce-free tx with `nonce_sequence = 42` would be accepted and included, violating a spec MUST.

**Fix:** in `validate_env`, when `parts.nonce_key == NONCE_KEY_MAX`, explicitly return `Err(...)` if `parts.expiry == 0` or `tx.nonce() != 0`. `Err`, not `assert!` — a malformed tx must be rejected at the tx level, not crash the node.

**Lesson:** spec MUSTs on structural fields belong in `validate_env` (the stateless validation stage), not indirectly in state-dependent stages. And when translating MUSTs into Rust: always `return Err(...)`, never `assert!` / `panic!` / `unreachable!` — those would take down the entire node process on a single malformed tx.

### 2026-04-21 — Phase execution loop did not skip remaining phases on failure

EIP-8130 Block execution: *"if any call in a phase reverts, all state changes for that phase are discarded and remaining phases are skipped."*

The initial port reverted the failing phase's checkpoint but let the outer `for phase in &parts.call_phases` loop continue, so subsequent phases still executed:

```rust
for phase in &parts.call_phases {
    // ...inner call loop sets phase_ok = false on revert and breaks...
    if phase_ok {
        accumulated_refunds += phase_refunds;
    } else {
        evm.ctx().journal_mut().checkpoint_revert(checkpoint);
    }
    phase_results.push(...);
    // ⚠️ no break — next phase runs anyway
}
```

**Why it's wrong:** Sponsor-gated flows rely on this: phase 0 performs a sponsor-paid setup call, phase 1 does the user's actual action. If phase 1 reverts, phase 2..N must NOT run — they may depend on phase 1's writes or repeat its side effects. Continuing silently violates the atomicity contract the spec gives to verifier contracts.

**Fix:** after pushing the failed phase's result, `break` out of the outer loop.

**Lesson:** EIP-8130 phase semantics are "atomic per phase, sequential across phases, halt on first failure". When implementing a phased execution loop, the halt-on-failure needs an explicit `break` — revert-but-continue is not equivalent.

### 2026-04-21 — `thread_local!` as handler → precompile bridge when the data is derivable from `tx`

Initial port cargo-culted base-revm's pattern: a thread-local `XLayerAATxContext` that the handler populated in `validate_against_state_and_deduct_caller` and the TxContext precompile read in `run_tx_context_precompile` for `getMaxCost()` / `getGasLimit()`.

```rust
thread_local! { static XLAYERAA_TX_CONTEXT: RefCell<Option<XLayerAATxContext>> = ... }

// handler:
set_xlayeraa_tx_context(XLayerAATxContext::new(&parts, execution_gas_limit, known_intrinsic, max_fee));

// precompile:
let max_cost = get_xlayeraa_tx_context().map_or(U256::ZERO, |ctx| ctx.max_cost);
```

**Why it's wrong:** I defended the thread-local as necessary because the handler computes `max_cost` and `gas_limit` and the precompile can't reach handler state. But those values are just deterministic formulas over `tx.xlayeraa_parts()` and `tx.max_fee_per_gas()`:

```
gas_limit = tx.gas_limit - parts.aa_intrinsic_gas
max_cost  = (gas_limit + parts.aa_intrinsic_gas - parts.payer_intrinsic_gas
             + parts.custom_verifier_gas_cap) × tx.max_fee_per_gas
```

All inputs are inclusion-time immutable data available to the precompile through `context.tx()`. There is no dynamic handler state involved. Tempo's AA design avoids this entirely — derived values live on the tx env and are computed on demand.

**Fix:** delete the thread-local, the `XLayerAATxContext` struct, and the `set_*` / `clear_*` / `get_*` functions. Compute on demand in the precompile via two small helpers (`aa_execution_gas_limit`, `aa_max_cost`).

**Lesson for future "handler → precompile" plumbing:** before reaching for a thread-local / transient-storage bridge, ask whether the value is already a pure function of `tx + parts`. If yes, compute it at the call site — no global state, no clear-before-every-tx invariant to maintain, no cross-tx pollution risk.

### 2026-04-21 — `U256::from_limbs([N, 0, 0, 0])` for small-integer constants

```rust
pub const NONCE_BASE_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);
pub const EXPIRING_SEEN_BASE_SLOT: U256 = U256::from_limbs([2, 0, 0, 0]);
```

**Why it's wrong:** same readability problem as byte-array addresses. `from_limbs` exposes alloy's internal little-endian limb representation to every reader; nobody wants to decode that.

**Fix:** use the `uint!` macro (re-exported from `revm::primitives::uint`).

```rust
pub const NONCE_BASE_SLOT: U256 = uint!(1_U256);
pub const EXPIRING_SEEN_BASE_SLOT: U256 = uint!(2_U256);
```

### 2026-04-21 — Hand-rolled ABI encoding / selectors instead of `sol!`

The first port of the precompile layer hand-wrote `abi.encode` for every return type:

```rust
pub fn selector(sig: &[u8]) -> [u8; 4] { let h = keccak256(sig); [h[0], h[1], h[2], h[3]] }
pub fn encode_address(a: Address) -> Bytes { /* left-pad 20 bytes to 32 */ }
pub fn encode_calls_abi(phases: &[Vec<XLayerAACall>]) -> Bytes { /* 60-line head/tail offset math */ }

// dispatch:
if selector_bytes == selector(b"getNonce(address,uint256)") { /* manually slice 4+12..4+32 */ }
```

And the storage-slot helpers were written byte-by-byte into fixed buffers:

```rust
pub fn aa_nonce_slot(account: Address, nonce_key: U256) -> U256 {
    let inner = { let mut buf = [0u8; 64]; buf[12..32].copy_from_slice(account.as_slice()); /* ... */ };
    // etc.
}
```

**Why it's wrong:**

- every encoder is a re-derivation of the Solidity ABI spec, which is exactly what `alloy-sol-types` already generates from a `sol!` block — bugs in offset math, padding, or selector case are uniquely painful because the EVM just sees "wrong bytes";
- manual dispatch via `selector(b"getNonce(...)")` re-hashes on every precompile call, and loses type information about arguments;
- storage-slot helpers are Solidity mapping layout (`keccak256(key ‖ slot)`), which `SolValue::abi_encode((key, slot))` expresses in one line.

**Fix:** declare the interface once with `sol!`, then use `SolCall::SELECTOR`, `SolCall::abi_decode`, `SolCall::abi_encode_returns`, and `SolValue::abi_encode` for keys.

```rust
sol! {
    struct CallTuple { address target; bytes data; }
    interface INonceManager {
        function getNonce(address account, uint256 nonceKey) external view returns (uint256);
    }
    interface ITxContext {
        function getCalls() external view returns (CallTuple[][] memory);
        // ...
    }
}

// dispatch:
match selector {
    ITxContext::getSenderCall::SELECTOR => ITxContext::getSenderCall::abi_encode_returns(&sender),
    // ...
}

// slot helpers:
pub fn aa_nonce_slot(account: Address, nonce_key: U256) -> U256 {
    let inner = keccak256((account, NONCE_BASE_SLOT).abi_encode());
    U256::from_be_bytes(keccak256((nonce_key, inner).abi_encode()).0)
}
```

The `sol!` block becomes the single source of truth; encoder/decoder/selector all derive from it.

### 2026-04-21 — `Address::new([byte, ...])` instead of hex `address!("0x...")`

When porting base-revm's handler and precompile addresses, I used the byte-array form:

```rust
pub const NONCE_MANAGER_ADDRESS: Address =
    Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xaa, 0x02]);
```

**Why it's wrong:** byte arrays are unreadable at a glance — you have to count nibbles to see the actual address. The ecosystem (alloy, reth, op-revm) uniformly uses the `address!("0x...")` macro for constant addresses, which is a checksummed hex literal that survives review.

**Fix:** always use `address!("0x...")` for constant addresses.

```rust
pub const NONCE_MANAGER_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa02");
```
