---
name: adversarial-check
description: Adversarially compare two commits/tags to derive code execution paths that could consensus-fork (state root mismatch, block hash divergence, gas divergence). Takes two git refs as input, diffs them, and hunts edge cases across a consensus-surface checklist.
---

# Skill: adversarial-check

Given two git refs (commit hashes and/or tags), derive the diff between them and adversarially search for execution paths where the two versions could produce **different consensus outputs** — state root, block hash, receipts root, gas used, or accept/reject decisions. The goal is to find the edge case that forks the chain *before* it ships.

TRIGGER when: user runs `/adversarial-check <ref1> <ref2>`, or asks to "check consensus safety between two commits/tags/versions", "will this diff fork the chain", "adversarial check".
DO NOT TRIGGER when: user asks for a general code review (use `pr-review`) or a security audit.

## Hard constraints: static analysis only — NEVER build or execute

This skill is a pure static-reasoning exercise over git history. Building this workspace takes enormous CPU/time and adds nothing the diff can't tell you.

- **NEVER run** `cargo build`, `cargo check`, `cargo clippy`, `cargo test`, `cargo nextest`, `cargo install`, `cargo run`, `just build*`/`just check`/`just test`, `docker build`, or any command that compiles code or executes the node/tests. This applies to every sub-agent spawned by this skill — repeat the prohibition verbatim in their prompts.
- **NEVER modify the working tree** (no checkout, no submodule update, no cargo metadata/tree, which may touch the lockfile or network). Read code exclusively via `git log`, `git diff REF_OLD REF_NEW -- <path>`, `git show <ref>:<path>`, and `git -C <submodule> …` equivalents.
- **Stay on the current branch — never read other branches.** All analysis is confined to commits reachable from the current branch's `HEAD` (enforced by the ref validation in Inputs). Never enumerate, resolve, or read commits from any other branch: no `git log <other-branch>`, no `git show <other-branch>:<path>`, no `git diff` against a ref outside the current branch's history, no `git branch -a`/`git for-each-ref` sweeps to discover other branches, and no fetching other branches. If a trail of evidence appears to lead to a commit not on the current branch, record it as an open question — do not follow it. Repeat this prohibition verbatim in every sub-agent prompt, alongside the build/execute prohibition.
- The only permitted evidence is: the diff hunks, file contents at the two refs, commit messages, and manifest/lockfile contents at the two refs — all from the current branch's history.
- Where only a build could settle a question (does it compile, which rev does cargo actually resolve, does a test pass), record it as an **open question** or a **verification suggestion** in the report — for humans/CI to run later — never execute it yourself.

## Inputs

Two git refs are **required**: `REF_OLD` and `REF_NEW` (commit hashes, tags, or branch names).

- If invoked as `/adversarial-check <ref1> <ref2>`, use them as `REF_OLD` and `REF_NEW` (older/currently-deployed first, newer/candidate second).
- If fewer than two refs are given, ask the user for the missing ref(s). Do not guess.
- Validate both refs before anything else:
  ```bash
  git rev-parse --verify --quiet <ref>^{commit}
  ```
  If a ref is unknown, try `git fetch --tags` once, then report failure to the user.
- **Both refs must lie on the current branch.** After resolving, verify each ref is an ancestor of (or equal to) the current branch's `HEAD`:
  ```bash
  git merge-base --is-ancestor <ref>^{commit} HEAD
  ```
  If either check fails, the ref belongs to another branch (or is ahead of the checkout) — **abort and report which ref is outside the current branch**; do not fetch, resolve, or analyze commits from other branches, and do not fall back to a nearest merge-base. Ref names that are other branches' heads are rejected by this same check, not special-cased.

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

**Locally-diffable dependency drift — diff it, don't defer it.** Before declaring a bumped dependency unverifiable, check whether its two revisions are readable from here:

- **Submodule gitlink moved** (`deps/optimism` etc.): the objects usually already exist in the submodule's local git dir. Read them with `git -C deps/<sub> log --oneline OLD_SHA..NEW_SHA` (commit messages alone often name the behavior change) and `git -C deps/<sub> diff OLD_SHA NEW_SHA -- <consensus paths>`, applying this same checklist to the consensus-relevant hunks. If an object is missing, one bounded `git -C deps/<sub> fetch origin <sha>` attempt is permitted — objects only; still never `submodule update`, `checkout`, or anything that touches any working tree.
- **Git-rev pin moved in Cargo.toml/Cargo.lock** (e.g. `okx/reth` rev A → rev B): check for a local clone of that repo (workspace sibling directories, cargo git checkouts) holding both revs and diff there the same way.
- **crates.io version bump**: genuinely not diffable here — report old→new and recommend the dependency-side check.

Only after these fail may a dependency bump be filed as an open question; say in the report which of the two revisions was unreadable and why.

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

1. **Admission**: A tx/payload valid under REF_OLD but rejected under REF_NEW (or vice versa)? Changed error variants that alter engine-API accept/reject? Deposit/system tx checks tightened or loosened? Boundary values (max gas limit, empty payload, zero-gas tx, malformed but previously tolerated encodings)? Opcode-legality changes in payload content — init-code/deployed-code rules (`0xEF` prefix per EIP-3541, init-code size per EIP-3860, code-size cap) or fork-gated opcode sets accepting a deployment on one ref and rejecting it on the other?
2. **EVM semantics**: Precompile behavior at edge inputs (empty input, max length, invalid points)? Opcode gas or output changes? Unusual opcodes in the tx payload — invalid/undefined opcodes, truncated `PUSH`, jump-dest analysis, fork-gated opcodes — handled identically (see the unusual-opcode trigger sweep in Section 4)? Fork-activation off-by-one — behavior AT the activation timestamp/block vs one before/after? Revert data propagation changes?
3. **State writes**: Ordering of writes changed? Self-destruct + re-create in same tx/block? Touched-but-empty account handling? Cache vs DB read divergence (e.g. raw-cache eviction, stale reads)? Balance changes moved to a different point in the tx lifecycle?
4. **Gas/fees**: Rounding or overflow behavior in fee math? L1 data fee computed from different input (pre/post compression, different cost function)? Refund cap edges? Fee vault address or crediting-order changes? Base fee computation at block-gas boundary values?
5. **Block output**: Tx ordering rules changed (pool priority, sender nonce grouping, gasless/RSC filters admitting different sets)? Receipt field or bloom construction changes? Header field defaults (extraData, withdrawalsRoot, requestsHash) at fork boundaries?
6. **Derivation/rollback**: Same L1 data deriving a different L2 block? Reorg handling leaving different head or stale state? Restart-after-crash replay reaching a different state than the live path did?
7. **Multipath**: Block **built** by REF_NEW sequencer vs **imported** by REF_OLD replica — identical outputs? Parallel execution scheduling exposing order-dependence? Replay/backfill using a code path the live path doesn't share? Any state root computed by two different implementations in the same binary?

**Cross-version pairing matters**: the fleet upgrades gradually. Always evaluate the mixed topology — REF_NEW sequencer + REF_OLD replicas, and the reverse. A change that is self-consistent within one binary still forks the network if build (new) and import (old) disagree.

### 4. Build an abstract state/logic tree from the diff — read hunks, not just names

For every file classified in Section 2, read the actual diff (`git diff REF_OLD REF_NEW -- <path>`) and, where the hunk is ambiguous, read surrounding context at both refs (`git show REF_OLD:<path>`, `git show REF_NEW:<path>`). **Never compile or execute anything** — all reasoning is done on this abstraction:

For each consensus-relevant hunk, model the changed function as an abstract tree and compare the two versions node by node:

```
input domain (tx fields, block/header fields, timestamps, config flags, prior state)
  └─ guard/branch conditions (in evaluation order)
       └─ state transitions taken on each branch (writes: nonce/balance/storage/code;
          accumulators: gas, fees, logs; early returns / errors)
            └─ consensus outputs produced (state root input set, receipts, gas used,
               accept/reject + error variant, header fields)
```

Derive findings by structural comparison of the REF_OLD tree vs the REF_NEW tree:

- **Branch set changed**: a guard added/removed/reordered → find the input region that lands in different branches across refs.
- **Same branch, different transition**: identical condition but the write/accumulation differs (order, width, rounding, saturation) → find the value range where results differ.
- **Input domain changed**: a field/flag/default now feeds the decision → the fleet-wide value of that input becomes a fork switch.
- **Output mapping changed**: same post-state but different externalization (receipt fields, error variant, header default) → check which consensus assertion consumes it.

Walk each checklist dimension (Section 3) against the merged tree and ask the standard question: *is there an input for which the two trees emit different consensus outputs?* Every such input region is a candidate finding for Section 5; the tree path IS the trigger derivation.

**Opcode observability analysis** — for every hunk that touches the EVM environment, execution context, or gas accounting, enumerate which EVM opcodes could *observe* the change and derive triggers as "a tx executing opcode X reads a different value across refs" (each is a candidate state-root/receipts finding, since observed values flow into storage, logs, and control flow):

- Block env mutation (`BlockEnv`/`CfgEnv` fields set, zeroed, or restored around a tx): `BASEFEE` (0x48), `COINBASE` (0x41), `TIMESTAMP` (0x42), `NUMBER` (0x43), `PREVRANDAO` (0x44), `GASLIMIT` (0x45), `CHAINID` (0x46), `BLOBBASEFEE` (0x4A), `BLOCKHASH` (0x40)
- Tx env / fee handling changes (fee skips, price overrides, sponsor/gasless paths): `GASPRICE` (0x3A), `ORIGIN` (0x32), `CALLER` (0x33)
- Balance-crediting order or fee-vault changes: `BALANCE` (0x31), `SELFBALANCE` (0x47) — a contract reading its own or a vault's balance mid-tx sees the difference
- Gas accounting, refund, warming/access-list changes: `GAS` (0x5A) — any change to when/how much gas is charged is observable by a contract that branches on remaining gas; also dynamic-cost opcodes `SLOAD`/`SSTORE`/`*CALL`/`EXTCODE*` under warming rule changes
- Code/state introspection after write-ordering changes: `EXTCODESIZE`/`EXTCODEHASH`/`EXTCODECOPY` (0x3B/0x3F/0x3C), `SELFDESTRUCT` (0xFF) re-create semantics
- New/changed precompiles or opcode gas tables behind a fork gate: list the affected opcodes/precompile addresses and mark findings fork-conditional on that activation

A mitigation that "only" tweaks the environment a tx runs under (e.g. zeroing base fee to bypass a fee check) is a consensus change if ANY opcode can read it — say which opcode, and the trigger is a tx using it.

**Scoped-toggle restoration analysis** — for every hunk that mutates shared execution state on a per-tx or per-call scope (a `CfgEnv`/`BlockEnv` field toggled around one tx, a validation flag like `disable_base_fee`, a precompile set or gas table swapped in, a cache primed or bypassed), verify the set→restore pairing structurally, then hunt the leak:

- **Every exit path restores**: success, revert, invalid-tx skip, and the error/`?` early-return paths — a toggle restored only on the happy path leaks on the first failing tx. Trace each `return`/`?` between set and restore in both trees.
- **Same-block leakage**: if the toggle is NOT restored before the next tx executes, the next tx in the block runs under the relaxed/mutated rule — e.g. a base-fee validation bypass leaking lets an underpriced non-exempt tx into the block. The trigger is a two-tx sequence in one block: one tx that engages the toggle (or errors inside it), followed by one that is only valid/invalid depending on the leaked state. Derive it explicitly.
- **Cross-ref scope mismatch**: one ref scoping the mutation per-tx and the other per-block (or not mutating at all) diverges block content and validation for the *follow-up* txs, even when the toggled tx itself executes identically — check the txs after the trigger, not just the trigger.
- **Observability double-check**: a correctly-restored toggle can still be observable *during* the tx (see the opcode observability analysis above); a validation-only flag that no opcode reads and that restores on every path is the only clean outcome. State which of the two you verified.

Each leak is a finding whose trigger is a same-block tx sequence — cheap for an adversary or even normal traffic to produce, so default likelihood to critical unless the toggle provably cannot be engaged by user txs.

**Unusual-opcode trigger sweep** — when constructing trigger inputs, do not stop at mainstream opcodes; adversarial payloads live in the rarely-executed corners. For every hunk touching the interpreter, opcode tables, code analysis/decoding, contract creation, or call handling, ask whether an *unusual* opcode sequence in the tx payload (calldata-driven code path, init code, or deployed bytecode) behaves differently across refs:

- **Invalid/undefined opcodes**: `INVALID` (0xFE) vs genuinely unassigned opcodes — same exception kind, same gas consumption (all-remaining vs charged) across refs? A change here diverges gas used and receipts.
- **Malformed bytecode edges**: `PUSHn` truncated at end of code, jump-destination analysis changes (`JUMPDEST` validity inside push data), code ending mid-instruction — accepted/decoded identically?
- **Deployment legality**: `0xEF`-prefixed code rejection (EIP-3541), max init-code size (EIP-3860), max deployed-code size — one ref rejecting/reverting a create that the other accepts is a direct state-root divergence.
- **Re-create / redeploy semantics**: `CREATE2` to a previously self-destructed or existing address, `SELFDESTRUCT` + re-create in the same tx/block, nonce/code-hash checks on collision.
- **Newer/fork-gated opcodes**: `PUSH0` (0x5F), `TLOAD`/`TSTORE` (0x5C/0x5D), `MCOPY` (0x5E), blob opcodes — available and identically priced at the same fork boundary on both refs? An opcode legal on one ref and invalid on the other is fork-certain once triggered.
- **Call-shape edges**: `DELEGATECALL`/`STATICCALL`/`CALLCODE` into precompile addresses, calls at max depth (1024), zero-gas calls, `RETURNDATACOPY` beyond return-data size — exception kind and gas identical?

Each hit is a finding whose trigger is simply "a tx whose payload executes this opcode sequence" — cheap for an adversary to craft, so default the likelihood to **critical (reachable by any user tx)** unless a fork gate or admission filter provably blocks it.

While building the trees, specifically hunt for:

- Behavior changes hidden as "refactors" (reordered match arms, changed default branches, `saturating_*` ↔ `checked_*` ↔ raw arithmetic swaps, integer type/width changes, float anywhere near consensus)
- Conditionals gated on config/env/CLI flags — a flag defaulting differently across the fleet is a consensus fork switch
- New early-returns or error paths that skip state writes previously applied (or vice versa)
- `HashMap`/`HashSet` iteration feeding anything ordered (tx selection, trie input, receipts)
- Time (`SystemTime`, `Instant`), randomness, or node-local state (pool contents, caches, peers) influencing block content or validation
- Feature flags / `#[cfg]` differences that make two builds of the same ref behave differently

### 5. For each candidate divergence, derive the concrete forking path

A finding is only reportable with a concrete scenario:

1. **Location**: the relevant file(s) and line number(s) — `path/to/file.rs:123` (or `:123-145` for a range), one entry per ref when the line numbers differ across refs (`REF_OLD path:LL / REF_NEW path:LL`). Line numbers come from the diff hunks / `git show <ref>:<path>` — cite the lines of the changed guard/transition, not the whole function. Every finding MUST carry at least one `file:line` reference.
2. **Trigger input**: the specific tx/block/timing/history that reaches the changed code
3. **Path under REF_OLD**: what executes, what output results
4. **Path under REF_NEW**: what executes, what output results
5. **Diverging assertion**: which consensus output differs (state root / block hash / receipts root / gas used / accept-reject)
6. **Topology**: which node pairing forks (new-seq vs old-replica, old-seq vs new-replica, both)
7. **Likelihood**: reachable by any user tx (critical), only by sequencer policy (high), only at fork boundary or via crafted input (medium), theoretical (low)

If a finding spans many files (e.g. a renamed flag consumed everywhere), cite the defining/deciding site(s) with line numbers and summarize the rest as "and N other call sites" — do not pad the finding with every occurrence.

If a suspected divergence cannot be traced to a concrete trigger, keep it as an **open question**, not a finding.

### 6. Verification suggestions (for humans/CI to run — never executed by this skill)

These go into the report as recommendations only; per the hard constraints above, this skill never builds or runs anything itself. For each finding (and for dependency bumps), suggest the cheapest decisive check, e.g.:

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

**Severity when the code lives in a dependency**: do not automatically cap a finding at fork-plausible just because the executing code sits in a bumped dependency rather than in this repo. When the divergence itself is pinned by in-repo evidence — the diff's own doc comments stating the mechanism (e.g. "zeroes the base fee for this tx"), or a behavioral test asserted on only one ref — rate the severity from that evidence: the divergence is established, and the unverified part is only its resolution, not its existence. Reserve the fork-plausible downgrade for cases where the *existence* of the behavioral difference is itself unconfirmed.

Each finding uses the 7-field template from Section 5 — the **Location** field (file path + line number(s)) is mandatory; a finding without a `file:line` reference is not reportable.

**Key files to look at**: a table of the files behind the findings — `file` | `line(s)` | `finding(s) it supports` | `one-line reason`. If more than 10 files are implicated across all findings, list only the **top 10**, ranked by finding severity then by likelihood, and close the table with one line: "N other files implicated — see individual findings." Fork-certain findings' files always make the cut.

**Checklist coverage table**: all 7 dimensions × (files touched, findings, verdict `CLEAN`/`FINDINGS`/`NOT-TOUCHED`).

**Declared non-consensus**: the file list excluded in Section 2, one line each on why.

**Verdict**: `NO CONSENSUS RISK FOUND` / `RELEASE BLOCKED (fork-certain findings)` / `VERIFY BEFORE RELEASE (fork-plausible/conditional findings)`.

## Fan-out protocol (large diffs)

For large diffs (>50 consensus-relevant files or a major dependency bump), fan out sub-agents — but shard by **bounded file batches, never by checklist dimension**. A per-dimension agent inherits an unbounded scope, overflows its context ingesting diffs, and dies silently, losing all its work.

1. **Plan batches from sizes.** Run `git diff --stat REF_OLD REF_NEW` (and the submodule equivalent) and partition the consensus-relevant files into batches of **≤10 files AND ≤1,500 changed lines** each. Every batch gets ALL 7 checklist dimensions applied to it.
2. **Bounded reads inside each agent.** Diff one file at a time (`git diff REF_OLD REF_NEW -- <file>`), never a directory. For any file with >800 changed lines, first list hunk locations (`git diff -U0 REF_OLD REF_NEW -- <file> | rg '^@@'`), then load only the relevant hunks/functions via targeted `git show`/`sed` ranges — never the whole diff at once.
3. **Persist incrementally.** Give each agent a scratchpad file path (`<scratchpad>/adversarial-check/<batch-id>.md`). After EACH file it analyzes, the agent must append: file name, verdict (clean/finding/non-consensus + one line why), and any finding in the Section 5 template — including the mandatory **Location** field with `file:line` references taken from the diff hunks, so the orchestrator never has to re-derive line numbers at merge time. The final agent message is just a pointer to this file plus a summary — the file, not the message, is the source of truth.
4. **Report and terminate on completion — never idle.** Each agent's prompt must state, verbatim in spirit: "When your batch is done, write the complete report (findings + coverage + non-consensus list + anything unverifiable) as your FINAL message and END YOUR TURN IMMEDIATELY. Your task is over at that point: do not wait for further instructions, do not ask what to do next, do not poll, do not schedule wakeups or background work, and do not stay alive 'in case' — an agent that has emitted its final report and is still running is a bug." The orchestrator treats an idle/available notification from a sub-agent as "finished": immediately harvest its report (final message or scratchpad file) and shut it down (TaskStop) — never leave a finished agent running.
5. **Supervise with a 10-minute watchdog.** After spawning, the orchestrator tracks each agent's progress via its scratchpad file. If an agent's scratchpad has not grown for **10 minutes**, treat it as stalled regardless of its reported status: harvest whatever the scratchpad contains, forcibly stop the agent (TaskStop), and — if files from its batch remain uncovered — spawn a fresh agent for ONLY the remaining files. Never wait indefinitely on an agent, never ping a stalled agent to "continue", and never re-spawn the original unbounded scope. The run is over when every batch is harvested and every agent is stopped; the orchestrator must not end with sub-agents still alive.
6. **Cap concurrency** at 4 agents; queue remaining batches.
7. **Merge from the scratchpad files**, then reconcile cross-batch interactions (e.g. a flag defined in one batch, consumed in another) in the orchestrator before writing the report.

## Notes

- The absence of findings is a claim too — only output `NO CONSENSUS RISK FOUND` after every consensus-relevant hunk has actually been read, and say so explicitly if anything was skipped.
- Dependency drift is only "outside the repo" after the locally-diffable checks in Section 1 fail (submodule objects, local clones of git-pinned deps). What remains genuinely unreadable (crates.io bumps, git revs with no local objects): report old→new versions and recommend running this checklist against the dependency's own diff — do not silently mark it clean.
