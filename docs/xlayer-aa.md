# XLayerAA — Implementation Notes

## Milestone status (P2 close-out)

As of 2026-04-22, P2b is feature-complete on the primitives layer but
intentionally stops short of "node wiring" (original P2b.2d). The M5
mempool work is what actually threads `XLayerPrimitives` through the
top-level node builder.

| Sub-milestone | Status | What's in |
|---|---|---|
| P2b.1 | done | `XLayerAAEnvelope` newtype + `XLayerTxEnvelope = Extended<OpTxEnvelope, XLayerAAEnvelope>` + wire→exec `FromRecoveredTx` |
| P2b.2a | done | `InMemorySize` + `TxHashRef` + `SignerRecoverable` on both envelope types |
| P2b.2b | done | `SignedTransaction` impl (gated on `serde`); side-fix: sever `xlayer-revm` default-feature cascade that forced serde/reth-codec on standalone builds |
| P2b.2c | done | `XLayerPrimitives: NodePrimitives` with `SignedTx = XLayerTxEnvelope`; naive `Compact` impl (length-prefixed 2718 bytes, behind `reth-codec` feature); `RlpBincode` marker (behind `serde-bincode-compat` feature) |
| P2b.2d | deferred to M5 | full node wiring — requires forking `OpEngineTypes` + pool builder + payload builder because reth's `NodeTypes::Payload: PayloadTypes<BuiltPayload: BuiltPayload<Primitives = Self::Primitives>>` bound pins the engine-types' `Primitives` slot to ours, which cascades through `reth-optimism-node::OpEngineTypes` (bounded on `NodePrimitives<Block = OpBlock>`, not our `XLayerBlock`). That's M5-scope mempool-and-payload surgery, not a primitives-layer fix. |

`main.rs` still ties `bin/node` to `OpNode`. That's intentional: the
current node is wire-compatible with stock 0x00/0x01/0x02/0x04/0x7E
traffic and doesn't yet accept 0x7B at the pool boundary. M5 is what
swaps the pool to `XLayerAATransactionPool` and replaces the payload
builder so `XLayerPrimitives` becomes reachable end-to-end. Until
then, `XLayerPrimitives` is a declarative target kept compile-green
by the two gate tests in `crates/builder/src/primitives.rs`.

## Mistakes

Running log of mistakes made during XLayerAA implementation, so we don't repeat them. Append new entries at the top.

### 2026-04-22 — The plan said "add `OpTxEnvelope::Eip8130` variant to op-alloy"; the right move is `Extended<OpTxEnvelope, XLayerAAEnvelope>` with zero submodule patches

**What the plan said.** M1 prescribed directly patching `OpTxEnvelope` in the `op-alloy` submodule — adding an `Eip8130` variant and cascading through `OpTxType`, `OpTypedTransaction`, `OpPooledTransaction`, every match arm, the receipt envelope, op-reth primitives' `Compact` / `InMemorySize` codecs, serde tagged-enum plumbing, etc. The scoping exercise before P2 put that work at 600–1000 lines of submodule diff across ~15 files, plus non-trivial rustc-ICE risk (shared with the earlier `OpHardforks` default-method misadventure — see the next-older entry below).

**What's actually there.** `alloy_consensus::Extended<BuiltIn, Other>` exists specifically for this use case. Its own crate doc calls it out:

> This is intended to be used to extend existing presets, for example the ethereum or opstack transaction types and receipts.

It provides blanket impls for every trait the downstream consumers expect:

```rust
impl<B: Transaction, T: Transaction>   Transaction   for Extended<B, T>   // alloy-consensus
impl<B: Typed2718,  T: Typed2718>      Typed2718     for Extended<B, T>   // alloy-eips
impl<B: Encodable2718, T: Encodable2718> Encodable2718 for Extended<B, T>
impl<B: Decodable2718 + IsTyped2718, T: Decodable2718> Decodable2718 for Extended<B, T>
impl<B: Encodable, T: Encodable>       Encodable     for Extended<B, T>   // alloy-rlp
impl<B: Decodable, T: Decodable>       Decodable     for Extended<B, T>
impl<B: OpTransaction, T: OpTransaction> OpTransaction for Extended<B, T> // op-alloy
```

And the EIP-2718 dispatch in [`Decodable2718 for Extended`](../../.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/alloy-consensus-1.7.3/src/extended.rs#L219) already does the right thing:

```rust
if B::is_type(ty) { B::typed_decode(ty, buf).map(Self::BuiltIn) }
else              { T::typed_decode(ty, buf).map(Self::Other)   }
```

Because `OpTxType::is_type(0x7B)` returns `false` (0x7B isn't in the OP variant set), **a raw 0x7B-prefixed blob automatically routes to `Self::Other`** — no envelope patching required.

**Fix.** Define a single type alias in `crates/xlayer-consensus/src/envelope.rs`:

```rust
pub type XLayerTxEnvelope = Extended<OpTxEnvelope, XLayerAAEnvelope>;
```

where `XLayerAAEnvelope` is a local newtype over `Sealed<TxEip8130>` (the orphan rule blocks impl'ing foreign `OpTransaction` on foreign `Sealed<T>` — see the next-newer entry for the `Signed<>` vs `Sealed<>` diagnosis). All other traits on `Sealed<TxEip8130>` are forwarded through the newtype with boilerplate `self.0.x()` delegation — no new behaviour, no submodule changes.

Five guard tests (`raw_0x7b_decodes_as_other`, `standard_eip1559_decodes_as_builtin`, `envelope_encode_decode_is_stable`, `xlayer_aa_envelope_is_not_deposit`, `extended_delegates_is_deposit_both_sides`) pin the routing invariants so a future attempt to add 0x7B to `OpTxType` (which would silently flip dispatch to `BuiltIn` and invalidate our AA handler) trips the test.

**Lesson.**

- When a plan says "patch this upstream enum to add a variant," always check whether the upstream author already built an extension mechanism. `Extended` exists **exactly** because downstream L2s kept copy-pasting envelope variants. A five-minute upstream-trait search saves ~1000 lines of submodule diff.
- The extension mechanism's idiomatic consumer — the L2 plumbing layer — is upstream's ergonomic sweet spot. Downstream-only newtypes for inner types are orphan-rule friction (worked around here with a ~30-line forwarder), but the top-level `Extended<B, T>` has no such friction for anything that takes `B` or `T` individually.
- For wire-level types, the trait bar is surprisingly narrow: `Transaction + Typed2718 + IsTyped2718 + Encodable2718 + Decodable2718 + Encodable + Decodable + OpTransaction`. If your type already has most of those (as `TxEip8130` did) the remaining work is a handful of one-liners, not an architectural rewrite.

### 2026-04-22 — The original plan prescribed `Eip8130(Signed<TxEip8130>)` for the envelope variant, but the correct wrapper is `Sealed<TxEip8130>`

**What the plan said.** [docs/xlayer-aa.md](xlayer-aa.md) (the M1 section) prescribed:

> `deps/optimism/rust/op-alloy/crates/consensus/src/transaction/envelope.rs` — add **`Eip8130(Signed<TxEip8130>)`** variant to `OpTxEnvelope`

and the corresponding extensions to `OpTypedTransaction`, `OpPooledTransaction`, `OpTxType`. `Signed<T>` is the canonical wrapper used by Ethereum tx types (`Signed<TxEip1559>`, `Signed<TxEip7702>`, …), so it was the natural first guess.

**Why it's wrong for XLayerAA.** EIP-8130 bodies carry **embedded authentication** in their `sender_auth` / `payer_auth` blobs, not an external ECDSA signature. A `TxEip8130` **is** its signed form. Wrapping it in `Signed<TxEip8130>` would:

1. Require `TxEip8130: SignableTransaction<Signature>` — forcing a `signature_hash() -> B256` impl that doesn't fit the domain-separated sender/payer hash model. Any concrete impl would have to arbitrarily pick one of the two domains (or return a dummy), misleading downstream code that reads it.
2. Carry a redundant `Signature` alongside `sender_auth` — two sources of truth, guaranteed to drift.
3. Force a pointless `ecrecover`-style sender computation on the envelope when the real recovery logic lives in the AA handler's authorizer phase.

**What's actually correct.** `Sealed<TxEip8130>`. [`OpTxEnvelope::Deposit(Sealed<TxDeposit>)`](../deps/optimism/rust/op-alloy/crates/consensus/src/transaction/envelope.rs) is the existing precedent: Deposit txs also aren't externally-signed (they're CL-synthesized), and they use `Sealed` — a wrapper that just pairs the body with its pre-computed hash. The [upstream `TransactionEnvelope` derive macro](../../../home/po/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/alloy-tx-macros-1.7.3/src/parse.rs) explicitly supports both `Signed<T>` and `Sealed<T>` in variant positions (see the `inner_type()` helper it uses to extract the inner tx type).

All the traits the macro-generated envelope impls need are covered by blanket impls on `Sealed<T>`:

```
impl<T: Encodable2718> Encodable2718 for Sealed<T>    // alloy-eips
impl<T: Decodable2718 + Sealable> Decodable2718 for Sealed<T>
impl<T: Typed2718>    Typed2718    for Sealed<T>
impl<T: Transaction>  Transaction  for Sealed<T>      // alloy-consensus
```

`TxEip8130` already implements the RHS of each. **No new trait impls needed on the tx itself.**

**Fix.** P1 scope shrinks to:

1. Add a regression-guard roundtrip test — `sealed_wrapper_round_trip` in `crates/xlayer-consensus/src/aa/tx.rs` — that constructs `Sealed<TxEip8130>`, encodes via `Encodable2718`, decodes back, and asserts equality of both the inner tx and the seal hash. If the envelope landing in M1b/P2 ever breaks, this test fires first.
2. Drop the `SignableTransaction`-related trait-impl work the plan had prescribed for P1 — it's wholly unnecessary.
3. Correct downstream plan language: `Eip8130(Signed<TxEip8130>)` → `Eip8130(Sealed<TxEip8130>)` in every reference.

**Lesson.**

- When porting a plan that calls for "add this variant to the tx envelope", inspect the **existing non-`Signed<>` variants** first. `OpTxEnvelope::Deposit(Sealed<TxDeposit>)` was already a living precedent for a non-externally-signed wire type — directly analogous to ours.
- Spec diagrams ("signed tx", "signed body") often overload "signed" to mean "authenticated", not "wrapped in `Signed<T>`". Inside alloy's type system those are different categories — `Signed<T>` strictly means a body + **external** ECDSA signature. Check the trait shape of the wrapper before copying its name.
- Short roundtrip test up front is cheap and pins the right wrapper choice in code, not just prose — the test fails loudly if a future refactor tries to switch wrappers without updating the rest of the plumbing.

### 2026-04-22 — Shipped XLayerAA upgrade-tx bundle with hardcoded `common.FromHex` constants, ignoring the newer NUT-bundle pattern OP has been migrating to since Karst

**What I did.** When writing op-node's [`xlayer_aa_upgrade_transactions.go`](../deps/optimism/op-node/rollup/derive/xlayer_aa_upgrade_transactions.go), I followed the older per-fork style used by Ecotone / Fjord / Granite / Holocene / Isthmus / Jovian — see e.g. [`jovian_upgrade_transactions.go`](../deps/optimism/op-node/rollup/derive/jovian_upgrade_transactions.go) as the canonical example: a single Go file with 7 `common.FromHex("0x<huge hex>")` constants inline, a handwritten `XLayerAANetworkUpgradeTransactions()` function building each `DepositTx` from scratch, and `attributes.go` dispatch that ignores returned gas. The file was 275 lines, ~40KB of which was inline hex.

**What's actually current.** OP introduced a generic "Network Upgrade Transactions" (NUT) bundle format in [`upgrade_transaction.go`](../deps/optimism/op-node/rollup/derive/upgrade_transaction.go) starting with Karst:

- Bytecode lives in a standalone `<fork>_nut_bundle.json` file with a stable schema (`{metadata, transactions[]}`), loaded via `//go:embed`.
- A single `readNUTBundle(forkName, reader)` + `bundle.toDepositTransactions()` + `bundle.totalGas()` pipeline replaces all per-fork boilerplate.
- The dispatcher in `attributes.go` accumulates per-bundle gas into `upgradeGas` and adds it to the activation block's gas limit — so the ~9.5M-gas install set doesn't crowd out normal user txs in a 30M block.

Skimming the directory before writing would have surfaced the newer pattern:

```
ecotone_upgrade_transactions.go:   3× common.FromHex  (pre-NUT)
fjord_upgrade_transactions.go:     1× common.FromHex  (pre-NUT)
interop_upgrade_transactions.go:   2× common.FromHex  (pre-NUT)
isthmus_upgrade_transactions.go:   4× common.FromHex  (pre-NUT)
jovian_upgrade_transactions.go:    2× common.FromHex  (pre-NUT)
upgrade_transaction.go:            1× //go:embed     ← Karst+ NUT bundle
```

**Why it's wrong.**

- **Maintenance cost.** Every Solidity / compiler-setting change produces a multi-KB diff in the Go source (hex strings), making bytecode review impossible. JSON bytecode diffs are still opaque but at least the surrounding tx metadata (`from`, `gasLimit`, `intent`) is readable.
- **Lost gas budget.** Without the `upgradeGas +=` thread, the 7 ~9.5M-gas deploy txs have to squeeze into the activation block alongside normal user txs and the system tx — at the worst moment for the chain (fork activation, usually under load).
- **Out of step with upstream direction.** OP explicitly added a TODO in `attributes.go:184` — `// TODO(#19239): migrate Interop to NUT bundle` — signaling existing pre-NUT forks are going to be migrated, not preserved. Shipping a new pre-NUT fork adds to the migration backlog.

**Fix.** Migrate XLayerAA to the NUT bundle pattern:

1. `contracts/eip8130/script/extract_runtime.py` writes `xlayer_aa_nut_bundle.json` directly into the op-node submodule (`deps/optimism/op-node/rollup/derive/`) alongside `karst_nut_bundle.json`. Per-contract metadata (deployer, gas, intent, constructor-arg-ness) lives in a `CONTRACTS` dict at the top of the script. For the 3 contracts with `(address accountConfiguration)` constructor arg, the script pre-appends the abi-encoded AC address to `data` so there's no post-processing in Go.
2. `xlayer_aa_upgrade_transactions.go` shrinks from 275 lines → 112: `//go:embed xlayer_aa_nut_bundle.json` + a 10-line wrapper that calls the shared `readNUTBundle(forks.Name("xlayer_aa"), ...)` + `bundle.toDepositTransactions()` + `bundle.totalGas()`. Using `forks.Name("xlayer_aa")` as a free-string rather than adding XLayerAA to the upstream `forks.All` enum keeps the fork ladder untouched — XLayerAA remains XLayer-local.
3. `attributes.go` dispatch now captures both return values: `upgradeTxs, xlayerAAGas, err := XLayerAANetworkUpgradeTransactions()` + `upgradeGas += xlayerAAGas`. The `var upgradeGas uint64` declaration is hoisted from just-before-Karst to just-after-`upgradeTxs` so XLayerAA (dispatched before Karst) can also contribute.
4. Test updated to assert per-tx gas limits and total gas match the sum.

**Lesson.**

- Before cloning an existing pattern from a codebase, grep for variants. `grep -l "common.FromHex\|go:embed" *upgrade*.go` in the derive/ directory would have surfaced the split immediately. The existence of a newer parallel pattern usually signals the old one is on its way out.
- "Many files do it this way" isn't evidence that it's current. Check recency by timestamp or by whether the upstream has an open migration TODO (`grep TODO\(#`) pointing away from it.

### 2026-04-22 — Assumed Canyon's `ensure_create2_deployer` was the standard OP-upgrade pattern, and that Base's `Deploy.s.sol` was a production rollout recipe

**What I assumed.** When planning XLayerAA predeploy activation (M3 C4), I took Canyon's [`ensure_create2_deployer`](../deps/optimism/rust/alloy-op-evm/src/block/canyon.rs) as the canonical template for "install predeploys at a fork's activation block" — i.e. an EL-side `apply_pre_execution_changes` hook that checks `is_X_active_at_timestamp(ts) && !is_X_active_at_timestamp(ts - 2)` and force-writes bytecode. I also assumed Base's [`script/Deploy.s.sol`](https://github.com/base/eip-8130/blob/main/script/Deploy.s.sol) (which uses CREATE2 via Nick's factory at `0x4e59b448...` with salt=0) described how Base itself would install the 7 EIP-8130 predeploys on their live L2. The plan threaded both assumptions: vendor Base's Solidity source → forge build → `include_bytes!` runtime bytecode → install via an EL hook at activation.

**What's actually true.**

1. **Canyon is the exception, not the template.** Grepping `alloy-op-evm` for `ensure_*` in `apply_pre_execution_changes` returns exactly one function: `ensure_create2_deployer`. All other OP fork upgrades — Ecotone, Fjord, Granite, Holocene, Isthmus, Interop, Jovian — install their contracts via **CL-synthesized upgrade deposit transactions** emitted by op-node in `rollup/derive/<fork>_upgrade_transactions.go`. The EL receives these as ordinary deposit txs and executes them via the normal transaction path; no EL hook involved. Canyon is special because the CREATE2 deployer is a well-known Ethereum-mainnet contract deployed via a Nick-style keyless signature that can't be reproduced on L2 as a regular deposit tx — so Canyon falls back to an irregular state transition. **No other OP fork has this property.**
2. **The OP-standard recipe for deploying a brand-new contract at a fork.** Pick a fresh, human-readable deployer address per contract (e.g. `L1BlockJovianDeployerAddress = 0x4210000000000000000000000000000000000006`), compute the predeploy address as `CREATE(deployer, nonce=0)`, and have op-node emit a `DepositTx { from: deployer, to: nil, data: creationCode, ... }` in the activation boundary block. Every deployer is a fresh account (nonce guaranteed 0) so the address is fully deterministic across chains. See [deps/optimism/op-node/rollup/derive/jovian_upgrade_transactions.go:18-48](../deps/optimism/op-node/rollup/derive/jovian_upgrade_transactions.go#L18) for the cleanest recent example.
3. **Base's `Deploy.s.sol` is a local Foundry broadcast script**, not a production rollout mechanism. It wraps deployments in `vm.startBroadcast()` (i.e. signs txs from a funded EOA in a dev wallet) and assumes Nick's factory already exists at the target chain. It says nothing about how Base would actually install these at a fork boundary on Base mainnet — because **Base has not actually activated EIP-8130 on any live chain yet**. The repo is tagged "Reference implementation"; there is no production deployer.
4. **The "match Base's CREATE2 addresses byte-for-byte" constraint was empty.** It assumed Base had committed to a specific set of addresses on production, which would be broken by any divergence. Since Base has no production deployment, the constraint protected nothing.

**Why it's wrong.**

- It inflated an irregular-state-transition hack (Canyon) into a general-purpose pattern, and inherited all of Canyon's pain (the rustc-ICE C4 blocker from the previous entry) unnecessarily.
- It treated a Foundry dev-deployment script as a production spec, and built the XLayer plan around aligning with an address set that no live chain actually uses.
- It optimized for a hypothetical cross-chain tooling compatibility that doesn't exist yet and may never exist in the assumed form (Base could easily pick a different mechanism when they do ship).

**Fix.** Switch XLayerAA to the **standard OP-upgrade pattern**, matching Ecotone/Isthmus/Jovian:

1. **Drop CREATE2 + Nick's factory + salt=0 for predeploys.** Allocate 7 fresh deployer addresses (e.g. `0x4210000000000000000000000000000000000010..0016`), compute the 7 predeploy addresses as `CREATE(deployer_i, 0)`. Addresses will differ from Base but remain deterministic and chain-portable.
2. **Move predeploy installation to op-node.** Author `XLayerAANetworkUpgradeTransactions()` in `deps/optimism/op-node/rollup/derive/xlayer_aa_upgrade_transactions.go`, emitting 7 `DepositTx`s (one per predeploy) at the `XLayerAA` fork activation boundary block. Pattern follows `jovian_upgrade_transactions.go` exactly.
3. **EL becomes trivial.** Delete `AA_PREDEPLOYS` as an activation-hook input (no bytecode needs to be embedded in the EL). Keep `XLayerHardfork::XLayerAA` + `is_xlayer_aa_active_at_timestamp` — those are still needed for AA-tx-type validation, not for predeploy injection. The C4 rustc-ICE problem **disappears entirely** because there is no EL hook to write.
4. **Devnet: block 1, not block 0.** Set `genesis.timestamp = 0`, `XLAYER_DEVNET_XLAYER_AA_TIMESTAMP = 1`. op-node's activation-boundary logic (`isActive(blockTs) && !isActive(parentTs)`) can't fire inside genesis (no parent, CL doesn't synthesize txs for block 0), so the 7 upgrade deposit txs land in block 1. Users can send AA txs from block 2. Genesis-alloc was considered and rejected — it would create a devnet-only third code path on top of CL upgrade tx (used by testnet/mainnet) and add maintenance burden for a 2-second convenience.

**Lesson.**

- When a pattern appears only once in a large codebase, treat it as a historical exception until proven otherwise. `git log` on `canyon.rs` would have revealed that the irregular-state-transition trick was a one-off forced by a keyless-deployment constraint unique to the CREATE2 deployer. grep all `apply_pre_execution_changes` implementations before assuming any one of them is representative.
- When a reference implementation is labeled "reference" and has no production deployment, don't treat its auxiliary scripts (Foundry, hardhat, docker-compose) as production contracts. Check whether the upstream has actually shipped before cloning their deployment mechanism.
- "Byte-for-byte address compatibility" is a load-bearing constraint only when there's a concrete chain that would be broken by divergence. Without that, it's speculative alignment and can justify disproportionate engineering effort. Verify the counterparty chain has actually committed to addresses before letting the constraint shape architecture.

### 2026-04-22 — Adding a default method to `OpHardforks` in the op-reth submodule triggers a rustc ICE

**What I tried.** For M3 C4 (XLayerAA predeploy activation hook), I needed `alloy-op-evm::block::apply_pre_execution_changes` to detect XLayerAA activation without pulling `xlayer-chainspec` upward. Plan: add one default method on [`OpHardforks`](../deps/optimism/rust/alloy-op-hardforks/src/lib.rs):

```rust
fn named_fork_activation(&self, _name: &str) -> ForkCondition {
    ForkCondition::Never
}
```

…and override it on `OpChainSpec` to iterate the `ChainHardforks` list by `Hardfork::name()`. A separate `xlayer_aa.rs` module would then call `spec.named_fork_activation("XLayerAA").active_at_timestamp(ts)`.

**What happened.** With just that default-method addition applied (before any downstream impls existed), `cargo test -p xlayer-chainspec --lib` panicked with a rustc internal error during build of the unrelated `reth-optimism-node` crate — a spurious `Unpin` trait-obligation failure on `reth_basic_payload_builder::BasicPayloadJob<_, OpPayloadBuilder<..., OpEvmConfig, (), _>>`. `cargo check --workspace` passed cleanly; the ICE only surfaced when the test harness built the full async-future type graph. Stashing the trait patch made the ICE disappear immediately.

**Why it's wrong.** Adding a default method to a trait decorated with `#[auto_impl::auto_impl(&, Arc)]` forces `auto_impl` to regenerate blanket impls for `&T` and `Arc<T>`. When those blanket impls interact with a downstream trait-obligation stack that includes deeply nested generic types (`BasicPayloadJob` wraps `OpPayloadBuilder` wraps `OpTransactionValidator` wraps `BlockchainProvider` wraps `NodeTypesWithDBAdapter` wraps `OpNode` + `OpEvmConfig` + `OpPayloadBuilderAttributes<OpTxEnvelope>` …), rustc's solver can hit an internal state where `Unpin` auto-trait inference loops or over-recurses.

**Fix.** Revert the trait-method approach. Two realistic paths for C4:

1. **Block-executor wrapper inside xlayer-reth** (no op-reth patch): write a `XLayerOpBlockExecutorFactory` that delegates to `OpBlockExecutorFactory::create_executor` and wraps the returned `OpBlockExecutor` in a newtype whose `apply_pre_execution_changes` calls our installer first. Invasive (many `BlockExecutor` methods to delegate) but purely additive.
2. **Required method on a new op-reth trait** (not a default on `OpHardforks`): define a fresh `NamedForkLookup` trait in alloy-op-hardforks with no default impls, parameter-bound on `OpBlockExecutor`'s `Spec`. This avoids the auto_impl blanket-regeneration path that appears to trigger the ICE.

**Interim impact.** `AA_PREDEPLOYS` exists as a declarative table in `crates/chainspec/src/xlayer_aa_predeploys.rs` but nothing calls it at activation. Devnet (XLayerAA ts=0) therefore runs with empty code at the 7 predeploy addresses — any AA tx that depends on `AccountConfiguration` / verifier contract calls will revert at execution. Testnet / mainnet are at `u64::MAX` placeholders so unaffected. A dedicated C4 follow-up picks one of the two paths above.

**Lesson.** Default methods on traits decorated with `auto_impl::auto_impl` should be treated as high-risk when the trait is deep in the `op-reth` dep graph. Test with `cargo test --workspace --no-run` (not just `cargo check`) to surface solver-heavy obligations before committing such patches.

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
