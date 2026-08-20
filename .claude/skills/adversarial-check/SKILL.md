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

Overall guidelines, applying to every dimension:

- For every affected execution path, enumerate and review every reachable Rust `enum` variant and every conditional branch (`if`/`else`, `match` arms, guards, early returns, and `?` error propagation). Map each branch to a concrete triggering input and expected observable result; do not validate only the nominal success path or assume an untested arm is semantically equivalent.
- Do not limit the review to comparing REF_OLD with REF_NEW. Also examine whether the execution paths introduced or modified for the new feature can alter the consensus semantics of existing features. In most cases, these interactions can be identified by tracing every affected execution path in REF_NEW.

| # | Boundary | Key content to probe | Typical assertion (must be identical across refs) |
|---|---|---|---|
| 1 | **Txpool admission & candidate selection** | Validators, subpools, ordering/replacement rules, gasless admission, candidate iterators | Accept/reject, rejection class, subpool, replacement result, candidate tx set |
| 2 | **Consensus pre-execution validation** | Header/payload checks, tx env validation, balance/nonce/gas guards, error → Engine API mapping | Tx/block acceptance decision; no state or counter commits on invalid input |
| 3 | **EVM execution semantics & control flow** | Opcodes, precompiles, env fields, frames, error propagation, fork gating | Execution status, return/revert data, logs, observable opcode values, halt category |
| 4 | **State transition, commit & rollback** | Journal checkpoints, commit timing, exclusion paths, block finalization | Per-tx state diffs, post-state, **state root**; empty delta on failure/exclusion |
| 5 | **Gas & fee accounting** | Gas counters, refunds, fee formulas, vaults, L1 data / operator fees | `gasUsed`, cumulative gas, refunds, effective price, all balance deltas |
| 6 | **Receipts, header fields & block commitments** | Receipt construction, header assembly, encoding, roots/hashes | Receipts, bloom, header fields, tx/receipt/state roots, **block hash** |
| 7 | **Payload build/import & Flashblock path equivalence** | Default builder, `no_tx_pool`, Flashblock build/replay, engine import, backfill | Same inputs ⇒ identical included set, results, post-state, roots, block hash on every path |

Detailed probes per dimension (non-exhaustive — invent more from the actual diff):

#### Dimension 1: Txpool admission and candidate selection

Key entry points include:

- `deps/optimism/rust/op-reth/crates/txpool/src/validator.rs`
- `deps/optimism/rust/op-reth/crates/txpool/src/xlayer_gasless.rs`
- `deps/optimism/rust/op-reth/crates/txpool/src/pool.rs`
- `best_transactions` / `execute_best_transactions` in `deps/optimism/rust/op-reth/crates/payload/src/builder.rs`

When the diff changes a txpool validator, subpool, ordering, replacement rule, or iterator error path, check:

- Does the same transaction change from accepted to rejected, or move among `pending`, `basefee`, `queued`, or other subpools? Cover `nonce < state_nonce`, `==`, and `>`, plus consecutive nonces and nonce gaps for one sender.
- Did a balance boundary change? Cover balance sufficient only for `value`, only for the maximum L2 fee, for the L2 fee plus L1 data fee, exactly sufficient, and short by 1 wei. Confirm that gasless, normal, and deposit transactions do not incorrectly share one balance rule.
- Did a fee-cap boundary change? Cover `max_fee_per_gas = 0`, `basefee - 1`, `basefee`, and `basefee + 1`, plus priority-fee, blob-fee, operator-fee, and L1-fee checks where applicable.
- Do gasless admission and the block executor use the same contract address, state/header, and `(to, input, gas_limit)`? Cover whitelist allow and deny, allowance gas exactly at the cap and off by one, contract-call errors, and an unavailable latest header.
- After a new head, base-fee change, or reorg, are existing transactions reclassified or revalidated? Can a previously accepted gasless transaction execute as non-gasless, or vice versa?
- When one transaction from a sender fails, does the iterator skip only that transaction, skip all later nonces from that sender, or terminate the whole candidate iteration? Did a change among `mark_invalid`, `skip`, `continue`, and `break` alter the candidate set?
- Did replacement rules or ordering keys change? Cover the same sender and nonce, equal tips, gasless mock tips, and percentile boundaries. Determine whether this is an intended producer-policy change or an allegedly equivalent implementation that changed the candidate set.
- Does a `NoTxPool` payload still ignore the txpool completely? Ensure forced or payload-provided transactions cannot be affected accidentally by txpool filters, priority, or gasless mock prices.

Typical assertions: identical txpool accept/reject decisions, permanent/temporary rejection class, subpool, replacement result, and candidate transaction set. Unless ordering is an explicit protocol rule, do not require blocks built under different txpool policies to have the same block hash.

#### Dimension 2: Consensus pre-execution validation

Key entry points include:

- `validate_block_gas` and `execute_transaction_without_commit` in `deps/optimism/rust/alloy-op-evm/src/block/mod.rs`
- `validate_env` and `validate_against_state_and_deduct_caller` in `deps/optimism/rust/op-revm/src/handler.rs`
- Payload/header validation and the mapping of errors to Engine API `newPayload` results

When the diff changes a pre-execution guard, transaction error, `?`, or early return, check:

- Do normal, gasless, deposit, system-deposit, and post-execution transactions enter the correct branches? Are deposit/system-transaction rules accidentally relaxed or tightened before or after a fork?
- Are baseline checks for `enveloped_tx`, transaction type, chain ID, signature, sender code, nonce, init-code size, and deployed-code size skipped, duplicated, or reordered?
- Which parts must the sender's balance cover: `value`, maximum execution fee, L1 data fee, and operator fee? If gasless waives fees, does it still require `balance >= value`?
- Does block-gas validation use the declared gas limit, actual EVM gas, canonical gas, or gas after refund? Cover remaining block gas exactly equal to and one below the required value, the Regolith deposit exception, and several consecutive transactions.
- Do producer and verifier use the same counter definition and comparison boundary for DA footprint, blob gas, post-execution payload index/refund, and other block-level limits?
- When the gasless allowance system call returns allow/deny, exceeds its gas cap, reverts, halts, or encounters a DB error, how is the transaction classified? Are its journal, warm accesses, logs, and temporary context fully discarded before the real transaction executes?
- Does a new error variant actually change block validity, or only diagnostics? Follow it to the final Engine API result: `VALID`, `INVALID`, `SYNCING`, or `ACCEPTED`.
- After every validation early return, do nonce, balance, journal, warm set, receipt count, cumulative gas, DA counters, and canonical head remain unchanged?

Typical assertions: identical transaction/block acceptance decisions; invalid inputs commit no state or counters; when callers branch on error type, compare a stable error category and final payload status rather than the error string.

#### Dimension 3: EVM execution semantics and control flow

Key entry points include:

- `OpEvm::transact_raw` in `deps/optimism/rust/alloy-op-evm/src/lib.rs`
- The handler lifecycle in `deps/optimism/rust/op-revm/src/handler.rs`
- Diffs to revm opcode tables, precompiles, frames, and the interpreter

When the diff changes `BlockEnv`, `TxEnv`, `CfgEnv`, an opcode, a precompile, or error propagation, check:

- Which opcodes can observe each modified environment field? At minimum map `block.basefee -> BASEFEE` and effective gas-price/transaction fee fields to `GASPRICE`; also check whether adjacent fields such as `NUMBER`, `TIMESTAMP`, `COINBASE`, `PREVRANDAO`, `GASLIMIT`, `BLOBBASEFEE`, and `CHAINID` are overwritten or restored together.
- Is a temporary context change used only to bypass validation, or is it visible during contract execution? For example, temporarily setting `block.basefee` to zero changes fee validation, `BASEFEE`, and potentially fee calculations that depend on base fee.
- Is the same behavior preserved for top-level calls, nested `CALL`, `STATICCALL`, `DELEGATECALL`, `CREATE`, `CREATE2`, and precompile paths?
- Does a new or removed `return`, `?`, `map_err`, or guard bypass cleanup, context restoration, refund handling, beneficiary rewards, or execution-result normalization? Test revert, halt, OOG, invalid transaction, DB error, and inspector/hook errors, not only success.
- Are temporary fields restored on every exit? Within one block, execute `special tx -> normal tx` and `special tx revert/OOG -> normal tx`; make the later transaction read the affected opcode and persist the value to storage.
- Did propagation of revert data, return data, logs, or halt reasons change? Can an inner-frame revert be converted incorrectly into success/halt, or leave logs/state that should have rolled back?
- Is fork/spec selection off by one? Exercise the same opcode or precompile immediately before activation, in the first active block, and after activation.
- Can changes to Rust integer conversions, `checked_*`, `saturating_*`, defaults, or `unwrap_or_default` convert an exception into truncation/zero, or vice versa? Exercise zero, maximum, and off-by-one values.

Typical assertions: identical execution status, return/revert data, logs, observable opcode values, halt category, and complete EVM context after the transaction.

#### Dimension 4: State transition, commit, and rollback

Focus on journal checkpoints, `ResultAndState`, `commit_transaction`, block finalization, system/predeploy calls, and paths that execute a candidate but exclude it from the block.

When the diff changes account mutation, commit timing, or rollback, check:

- When does the caller nonce increment for calls and creates, success/revert/halt, and deposit/gasless/normal transactions? A validation failure or excluded candidate must not leave a nonce change.
- Did the order of value transfer, deposit minting, fee deduction/reimbursement, and beneficiary/vault crediting change? After an intermediate failure, only protocol-specified persistent effects may remain.
- Did creation/deletion semantics change for code, storage, transient storage, self-destructed accounts, created accounts, or touched-empty accounts? Cover create/self-destruct/re-create in the same transaction and the same block.
- Can a gasless whitelist/system call, simulation, or execute-then-exclude candidate leave state, logs, access warming, or cached reads? Can the next transaction observe phantom state or warming?
- If post-processing fails after `execute_transaction_without_commit` succeeds, has any state patch already been applied? Can it later be committed during a retry, the next transaction, or block finalization?
- Did the ordering of receipt/counter updates and DB commit change? What happens if a receipt is pushed but state commit fails, or state commits before receipt construction fails?
- Can failure in block finalization, a post-block balance increment, or a system-contract update leave half of the block state committed?
- Execute `tx1 -> tx2`, `tx1 revert -> tx2`, and `tx1 excluded -> tx2`; compare the nonce, balance, code, storage, and warm/cold state observed by `tx2`.

Typical assertions: identical per-transaction account/storage diffs, final post-state, and `stateRoot`; an empty state delta for failure or exclusion paths; identical pre-state observed by the next transaction in the sequence.

#### Dimension 5: Gas and fee accounting

When the diff changes a gas counter, refund, fee formula, vault, or any `+/-`, `checked_*`, or `saturating_*` operation, first enumerate every gas quantity present in the code: declared gas limit, intrinsic gas, EVM gas used, canonical gas used, refunded gas, cumulative block gas, DA/blob gas, and state/reservoir gas. Then check:

- Which gas quantity feeds each limit and receipt field, and is it the same for producer and verifier? In particular, ensure canonical gas after refund is not used accidentally to limit actual computation.
- Which counters are incremented, decremented, or cleared on success, revert, halt/OOG, failed create, deposits before/after Regolith, and gasless execution? Does a new early return skip any update?
- Is refund non-negative, capped by the relevant gas used, applied exactly once, and limited by the correct fork's refund cap? Cover `0`, exactly at the cap, cap ± 1, and refund greater than gas used.
- Did the inputs to effective gas price, base fee, or priority fee change? Exercise `max_fee = basefee`, `basefee ± 1`, zero priority fee, and the priority-fee limit.
- Are sender precharge, unused-gas reimbursement, beneficiary reward, base-fee vault, L1-fee vault, and operator-fee vault balance changes conserved without charging or refunding the same gas twice?
- Does gasless waive only the fees specified by the protocol while preserving gas consumption, refund, and cumulative-gas semantics? It must not set `gasUsed` to zero merely because fees are waived, nor reward a beneficiary/vault with unpaid fees.
- Did the ordering of deposit mint/value and fee deduction change? Did loading behavior for L1-fee metadata change when the metadata is absent?
- Are the encoded transaction, compressed size, gas basis, and rounding used for L1-data/operator fees unchanged? Check large multiplication, division, rounding direction, and overflow paths.
- Does a post-execution/SDM refund update both receipt gas and sender/beneficiary/vault balances? Updating only receipt gas directly produces a state-root divergence.

Typical assertions: exact equality of per-transaction `gasUsed`, cumulative gas, block gas used, refund, effective gas price, and balance deltas for the sender, beneficiary, and every fee vault. Also assert fee-conservation relations instead of comparing only final balances.

#### Dimension 6: Receipts, header fields, and block commitments

This dimension covers consensus outputs after transaction order has been fixed. Do not review txpool priority or ordering policy here.

When the diff changes receipt construction, header assembly, encoding, a root, or a hash, check:

- Did receipt type, status/post-state, cumulative gas, logs or log order, or logs bloom change? Are OP/X Layer extension fields such as deposit nonce, deposit receipt version, and operator fee present only for the correct fork and transaction type?
- Are receipts constructed correctly for success, revert/halt, deposit, gasless, and post-execution transactions? An excluded or validation-failed candidate must not produce a receipt.
- Are `None`, `Some(0)`, an empty-list root, and an absent field kept distinct? At fork boundaries, do withdrawals root, requests hash, blob/DA fields, `extraData`, and similar fields use the correct presence and encoding?
- Does header `gasUsed` use canonical gas or raw EVM gas as required? Do base fee, gas limit, timestamp, and L1-origin-derived fields come from the same payload attributes?
- Is the transaction root computed from the exact ordered transaction bytes finally included in the block? Is the receipt root computed using the correct typed-receipt encoding? Does the state root correspond to that same committed state?
- Is the block hash computed from the newly derived header fields rather than echoing a hash/root supplied in the input block?
- Can a post-execution error make the producer drop a transaction while the importer rejects the block, or make the two paths choose different fallback values for a receipt/header field?

Typical assertions: identical final ordered transaction bytes, complete receipts, logs bloom, consensus header fields, transaction root, receipt root, state root, and block hash. Report the earliest differing field before reporting its derived root/hash.

#### Dimension 7: Payload build, import, and Flashblock path equivalence

For xlayer-reth, compare the following paths; op-rbuilder is out of scope:

- Optimism default payload builder: `deps/optimism/rust/op-reth/crates/payload/src/builder.rs`
- The `no_tx_pool` payload-attributes path
- X Layer local Flashblock build: `crates/builder/src/flashblocks/builder.rs`
- External/cached Flashblock execution and replay: `crates/builder/src/flashblocks/handler.rs` and related cache/replay paths
- Engine API import and canonical block execution of the final payload
- Backfill, historical replay, or restart/resume paths where relevant

Fix the same parent state, payload attributes, and ordered transaction list to remove txpool-selection noise, then check:

- Are pre-execution system/deposit transactions, payload-provided transactions, builder transactions, and normal transactions inserted at the same positions? Can `no_tx_pool`, replay, or first/last-Flashblock conditions omit, duplicate, or reorder them?
- Do the default builder, NoTxPool, and Flashblock paths use the same block-executor configuration, including chain spec, gasless contract, post-execution mode, DA configuration, and receipt builder?
- When one transaction fails execution, does each path exclude only that transaction, skip later transactions from the same sender, continue to the next transaction, stop the current Flashblock, or invalidate the whole payload? Can these choices produce a different included set or pre-state?
- Does segmented Flashblock execution produce the same final state, receipts, and counters as executing the same ordered list at once? Segment boundaries must not reset cumulative gas, DA footprint, warm state, post-execution entries, or builder-transaction state.
- If partial replay of an external cached Flashblock fails, does the path resume at the failed item, fall back to a fresh build, or retain the successful prefix? If an error is logged and ignored, can old and new versions retain different prefix state?
- Do P2P-received Flashblock execution, local Flashblock building, and final canonical import apply the same gasless, fee, and validation semantics to a transaction?
- Cancellation, timeout, and fallback may select a different valid payload, but can they leave state/cache from a cancelled build that contaminates the next build?
- Under a fork supported by both versions, is a candidate-built payload accepted by the old importer, and an old-built payload accepted by the candidate importer, with identical recomputed commitments?

Typical assertions: given the same parent, attributes, and ordered transaction list, all paths produce the same included transaction set, per-transaction results, receipts, gas/fee deltas, post-state, roots, and block hash. If the input candidate sets differ, attribute that first to dimension 1 rather than misclassifying it as an execution-path inconsistency.

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
