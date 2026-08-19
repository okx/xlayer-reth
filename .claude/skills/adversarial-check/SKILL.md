---
name: adversarial-check
description: Adversarially compare two commits/tags to derive code execution paths that could consensus-fork (state root mismatch, block hash divergence, gas divergence). Takes two git refs as input, diffs them, and hunts edge cases across a consensus-surface checklist.
---

# Skill: adversarial-check

Given two git refs (commit hashes and/or tags), derive the diff between them and adversarially search for execution paths where the two versions could produce **different consensus outputs** — state root, block hash, receipts root, gas used, or accept/reject decisions. The goal is to find the edge case that forks the chain *before* it ships.

TRIGGER when: user runs `/adversarial-check <ref1> <ref2>`, or asks to "check consensus safety between two commits/tags/versions", "will this diff fork the chain", "adversarial check".
DO NOT TRIGGER when: user asks for a general code review (use `pr-review`) or a security audit.

## Inputs

Two git refs are **required**: `REF_OLD` and `REF_NEW` (commit hashes, tags, or branch names).

- If invoked as `/adversarial-check <ref1> <ref2>`, use them as `REF_OLD` and `REF_NEW` (older/currently-deployed first, newer/candidate second).
- If fewer than two refs are given, ask the user for the missing ref(s). Do not guess.
- Validate both refs before anything else:
  ```bash
  git rev-parse --verify --quiet <ref>^{commit}
  ```
  If a ref is unknown, try `git fetch --tags` once, then report failure to the user.

## Instructions

### 1. Establish the diff scope

```bash
# Commit-level context (what landed between the refs)
git log --oneline --no-merges REF_OLD..REF_NEW

# Full change surface
git diff --stat REF_OLD REF_NEW

# File list for classification
git diff --name-status REF_OLD REF_NEW
```

Also capture **dependency drift** — for an execution client, dependency bumps are a primary consensus-fork source:

```bash
git diff REF_OLD REF_NEW -- Cargo.toml Cargo.lock '**/Cargo.toml'
git diff REF_OLD REF_NEW -- .gitmodules deps/
```

Flag any version change to `revm`, `reth`/`reth-*`, `alloy-*`, `op-alloy-*`, `op-revm`, or the `optimism` submodule as **in-scope even though the Rust diff may look empty** — the consensus change lives in the dependency. When a consensus-critical dependency is bumped, list its old→new versions in the report and recommend running this same checklist against the dependency's own diff.

### 2. Classify changed files onto the consensus surface

Sort every changed file into one or more checklist dimensions (Section 3). Files that touch none of them (docs, CI, metrics naming, log messages, RPC read-only formatting) go to a **"declared non-consensus"** list — still shown in the report so the classification itself can be challenged.

Treat as consensus-relevant by default:
- EVM/execution: `revm`, evm config, precompiles, `execute`/`executor`, receipt builder
- Chainspec/hardforks: `crates/chainspec/`, fork activation timestamps/heights, genesis
- Payload building & validation: payload builder, block assembly, tx pool → block ordering
- Engine/consensus: newPayload/FCU handling, block validation, header checks
- Gas/fees: L1 data fee, base fee, operator/vault fee logic, gas refunds
- Derivation-adjacent state: anything altering what op-node sees via Engine API
- DB/trie: state root computation, storage formats, pruning that affects historical execution
- Sync paths: live sync vs backfill vs replay executing the same block differently

### 3. Adversarial checklist (base — extend as the diff demands)

For each dimension, ask: **"construct an input (tx, block, timing, history) for which REF_OLD and REF_NEW give different answers in the 'typical assertion' column."** Every hit is a finding.

| # | Boundary | Key content to probe | Typical assertion (must be identical across refs) |
|---|---|---|---|
| 1 | **Admission verification** | Block header fields, transaction validity, payload attributes, system/deposit transaction legality | Accept/reject decision AND error type consistency |
| 2 | **EVM semantics** (revm) | Opcodes, precompiles, revert paths, exceptions, fork activation boundaries | Return value, logs, exception kind consistency |
| 3 | **State writes** | Nonce, balance, code, storage, self-destruct | Post-state and **state root** consistency |
| 4 | **Gas and fees** (optimism/op-reth) | Gas accounting, refunds, L1 data fee, base fee, fee vaults | Gas used and balance changes consistency |
| 5 | **Block output** | Transaction ordering, receipts, bloom, header fields | **Block hash** and **receipts root** consistency |
| 6 | **Derivation and rollback** | L1 origin, head, channel, batch handling | Head and derivation results consistent before/after reorg |
| 7 | **Multipath implementation** | Block build vs import, serial vs parallel execution, live sync vs backfill vs replay vs proof generation | ALL consensus-key outputs identical on every path |

Adversarial probes to apply per dimension (non-exhaustive — invent more from the actual diff):

1. **Admission**: A tx/payload valid under REF_OLD but rejected under REF_NEW (or vice versa)? Changed error variants that alter engine-API accept/reject? Deposit/system tx checks tightened or loosened? Boundary values (max gas limit, empty payload, zero-gas tx, malformed but previously tolerated encodings)?
2. **EVM semantics**: Precompile behavior at edge inputs (empty input, max length, invalid points)? Opcode gas or output changes? Fork-activation off-by-one — behavior AT the activation timestamp/block vs one before/after? Revert data propagation changes?
3. **State writes**: Ordering of writes changed? Self-destruct + re-create in same tx/block? Touched-but-empty account handling? Cache vs DB read divergence (e.g. raw-cache eviction, stale reads)? Balance changes moved to a different point in the tx lifecycle?
4. **Gas/fees**: Rounding or overflow behavior in fee math? L1 data fee computed from different input (pre/post compression, different cost function)? Refund cap edges? Fee vault address or crediting-order changes? Base fee computation at block-gas boundary values?
5. **Block output**: Tx ordering rules changed (pool priority, sender nonce grouping, gasless/RSC filters admitting different sets)? Receipt field or bloom construction changes? Header field defaults (extraData, withdrawalsRoot, requestsHash) at fork boundaries?
6. **Derivation/rollback**: Same L1 data deriving a different L2 block? Reorg handling leaving different head or stale state? Restart-after-crash replay reaching a different state than the live path did?
7. **Multipath**: Block **built** by REF_NEW sequencer vs **imported** by REF_OLD replica — identical outputs? Parallel execution scheduling exposing order-dependence? Replay/backfill using a code path the live path doesn't share? Any state root computed by two different implementations in the same binary?

**Cross-version pairing matters**: the fleet upgrades gradually. Always evaluate the mixed topology — REF_NEW sequencer + REF_OLD replicas, and the reverse. A change that is self-consistent within one binary still forks the network if build (new) and import (old) disagree.

### 4. Read the diff hunks, not just names

For every file classified in Section 2, read the actual diff (`git diff REF_OLD REF_NEW -- <path>`) and, where the hunk is ambiguous, read surrounding context at both refs (`git show REF_OLD:<path>`, `git show REF_NEW:<path>`). Specifically hunt for:

- Behavior changes hidden as "refactors" (reordered match arms, changed default branches, `saturating_*` ↔ `checked_*` ↔ raw arithmetic swaps, integer type/width changes, float anywhere near consensus)
- Conditionals gated on config/env/CLI flags — a flag defaulting differently across the fleet is a consensus fork switch
- New early-returns or error paths that skip state writes previously applied (or vice versa)
- `HashMap`/`HashSet` iteration feeding anything ordered (tx selection, trie input, receipts)
- Time (`SystemTime`, `Instant`), randomness, or node-local state (pool contents, caches, peers) influencing block content or validation
- Feature flags / `#[cfg]` differences that make two builds of the same ref behave differently

### 5. For each candidate divergence, derive the concrete forking path

A finding is only reportable with a concrete scenario:

1. **Trigger input**: the specific tx/block/timing/history that reaches the changed code
2. **Path under REF_OLD**: what executes, what output results
3. **Path under REF_NEW**: what executes, what output results
4. **Diverging assertion**: which consensus output differs (state root / block hash / receipts root / gas used / accept-reject)
5. **Topology**: which node pairing forks (new-seq vs old-replica, old-seq vs new-replica, both)
6. **Likelihood**: reachable by any user tx (critical), only by sequencer policy (high), only at fork boundary or via crafted input (medium), theoretical (low)

If a suspected divergence cannot be traced to a concrete trigger, keep it as an **open question**, not a finding.

### 6. Verification suggestions

For each finding (and for dependency bumps), suggest the cheapest decisive check, e.g.:

- Unit test asserting identical output across the two behaviors at the boundary value
- `replayor` / block replay of a historical range under REF_NEW, comparing state roots against canon
- DevNet mixed-version topology (xlayer-toolkit) with the trigger tx
- Targeted `cargo nextest run` of the affected crate at both refs with the same fixture

### 7. Report

Structure the output as:

**Scope**: `REF_OLD (<resolved sha>) → REF_NEW (<resolved sha>)`, N commits, M files; dependency drift table (crate, old → new).

**Findings** (ordered by severity):
- 🔴 **Fork-certain**: concrete input demonstrably produces different consensus output — must block release
- 🟠 **Fork-plausible**: divergent path exists; trigger constructed but not yet proven end-to-end — needs the suggested verification before release
- 🟡 **Fork-conditional**: diverges only under specific config/topology/fork-boundary timing — document the constraint and verify
- ⚪ **Open question**: suspicious change, no trigger derived — list what information would resolve it

Each finding uses the 6-field template from Section 5.

**Checklist coverage table**: all 7 dimensions × (files touched, findings, verdict `CLEAN`/`FINDINGS`/`NOT-TOUCHED`).

**Declared non-consensus**: the file list excluded in Section 2, one line each on why.

**Verdict**: `NO CONSENSUS RISK FOUND` / `RELEASE BLOCKED (fork-certain findings)` / `VERIFY BEFORE RELEASE (fork-plausible/conditional findings)`.

## Notes

- Effort scales with diff size: for large diffs (>50 consensus-relevant files or a major dependency bump), fan out one sub-analysis per checklist dimension and merge, rather than skimming everything in one pass.
- The absence of findings is a claim too — only output `NO CONSENSUS RISK FOUND` after every consensus-relevant hunk has actually been read, and say so explicitly if anything was skipped.
