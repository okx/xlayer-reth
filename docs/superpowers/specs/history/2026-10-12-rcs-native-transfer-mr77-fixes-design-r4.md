# MR77 RCS Filter — Nested CREATE Endowment Fix + Top-level native Transfer Unification — Design (revision r4)

> Delegated technical review target. Reviewer: building `starport-rcs` (via Terran Bus). This document is mirrored
> verbatim to the Lark review doc https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac (the SAME doc,
> updated in place across r1→r2→r3→r4). The Lark publication record (full URL / token / revision / content hash) is in
> `docs/superpowers/lark-review-doc.md`.

- **Status:** Draft for delegated technical review (building `starport-rcs`, via Terran Bus)
- **Revision:** r4 — 2026-10-12 (supersedes r3). r3 was design-APPROVED (PASS `starport-rcs-design-review-r3-pass-20261012`,
  Bus recvxP9lTBdjfE), but the r3 *implementation* at `fb43bf28` FAILED the Stage-5.0 delegated MR acceptance
  (CHANGES_REQUESTED `starport-rcs-mr77-fb43bf28-changes-20261012`, Bus recvxPLNSdwONs, + address-scope clarification
  `starport-rcs-mr77-fb43bf28-address-scope-20261012`, Bus recvxPLWsBiiGd). r4 revises the design to answer the five P2
  items from that review (see **§13 r4**). r1 and r2 were CHANGES_REQUESTED.
- **Unit:** viking-nativetransfer-starport-rcs  **Building/Reviewer:** starport-rcs
- **Jira:** XLOP-1228 (Task) under Epic XLOP-1217 — https://okcoin.atlassian.net/browse/XLOP-1228
- **Product repo / branch / baseline:** https://gitlab.okg.com/github/xlayer-reth · `xl/xlop-1217/0928-1135` · baseline `714e12934aca97eda1ff344d7d1fdc344baff03f`
- **Current MR head reviewed by Stage 5.0 (impl under revision):** `fb43bf28c7f93c0be4620ff204b63c9176b2f47c`
- **MR (reused):** https://gitlab.okg.com/github/xlayer-reth/-/merge_requests/77
- **Requirement source (Player-selected current basis):** https://okg-block.sg.larksuite.com/docx/HVoWdmiGcoPL4hxMJwglbObAgRh
- **Pinned revm (from Cargo.lock @ baseline):** revm-context-interface 19.0.3 · revm-handler 20.0.3 · revm-inspector 21.0.3 · revm-interpreter 37.0.3
- **Pinned op-revm submodule (hardfork basis):** `0dca4b893f3d85dfa02209af9ff031822c4e3043` — `rust/op-revm/src/spec.rs:43` maps `ISTHMUS | JOVIAN => PRAGUE` (⇒ EIP-7702 is ACTIVE under JOVIAN; see §13 r4 item 2)

## 0. Provenance and labeling (read first)

This is a newly authored, clearly-labeled revised spec for the two Player-approved MR77 fixes. Its requirement basis is
the Player-selected document https://okg-block.sg.larksuite.com/docx/HVoWdmiGcoPL4hxMJwglbObAgRh ("MR77 rcs-filter
内部原生转账观测 — 待修复问题", last modified 2026-10-12). Per the Player's explicit decision
(player-starport-mr77-provided-doc-current-basis-20261012), that document is the current requirement input; this spec
does NOT claim to be, or to have recovered, the historical "R12" spec
(`docs/superpowers/specs/2026-09-28-rcs-native-transfer-observation-design.md`), which is NOT present on this branch at
this SHA (GitLab get_file_contents returns File-not-found at 714e1293). Where this spec restates
previously-approved MR77 behavior, the source is the MR77 description plus the verbatim on-disk source at 714e1293,
both cited inline. New decisions introduced by this revision are flagged **[NEW DECISION]**.

### 0.1 What changed across r1 → r2 → r3 (responses to the building's CHANGES_REQUESTED)

r1 (`...-design-review-r1-20261012`) → CHANGES_REQUESTED (`starport-rcs-design-review-r1-changes-20261012`, Bus
recvxOPezyfmOF). r2 (`...-design-review-r2-20261012`) → CHANGES_REQUESTED (`starport-rcs-design-review-r2-changes-20261012`,
Bus recvxP4MqPZ68s): the reviewer **accepted all three r1-item fixes** (D1/D2 declassification, D3/D6 scoping, D7
layering) and raised one new required change plus two text corrections. Full item-by-item responses in **§13**.

**r2 → r3 material change (new in r3):**
- **r2 relied on `create_end`'s `!outcome.result.is_ok()` as the sole creation-failure signal (r2 §D7/§8.6).** The
  reviewer showed this leaks a virtual endowment on an `is_ok` early-return that produced no contract: pinned
  revm-handler 20.0.3 `frame.rs:272-283/:301-305` builds a `CreateOutcome{address:None}` via
  `return_error(InstructionResult::Return)` when the caller nonce can't bump (e.g. `u64::MAX`), and revm-interpreter
  37.0.3 `instruction_result.rs:163-166/219-222` classify `Return` as `is_ok`. **r3 adds D7a:** the creation
  observation's final validity is gated on the real `CreateOutcome.address` (`None` ⇒ revoke the frame's observation;
  `observed_count` stays sticky), with a success-path address-consistency assertion, and a dedicated real-OpEvm test
  (§8 item 6a). See **§4 D7a**, **§5**, **§8**.
- **§3 timing root-cause reframed** as the *requirement document's* hypothesis to be RED-verified (the pinned inspector
  dispatches `frame_start` before `frame_init` uniformly; the exact hook/checkpoint ordering is not independently
  proven here, and the r3 fix does not depend on it).
- **§9 anti-goal scoped**: "one movement ⇒ not two actions" applies within the same `audit_type` (different
  audit_types each carrying an action is normal matcher behavior; consistent with D6/§13).

**r1 → r2 summary (retained):**

- **r1 classified frames by `caller == tx.origin` (r1 §D1).** The reviewer correctly rejected this: under EIP-7702
  delegated-EOA execution and `DELEGATECALL` (which inherits the caller), `caller == origin` can be true for a *nested*
  frame, so identity equivalence is NOT a substitute for frame ancestry. **r2 removes the top-level/internal
  classification entirely** and instead observes the real native value of *every* frame through the single funnel (the
  reviewer's lead recommendation: "根/内部都观察真实value时不做无必要分类"). The top-level tx.value is captured by the
  root frame's own `call`/`create` hook — not by a separately-synthesized event — so there is no identity assumption and
  no top/internal double-count. See **§4 D1/D2**.
- **r1 overstated budget/action invariance (r1 §D3/§D6).** r2 restates these precisely against the real matcher code
  (binding count is a product across a rule's events accumulated across rules; action merge is per `(audit_type,
  log_index)`), and scopes every "no duplicate action" claim. See **§4 D3/D6** and **§6 (item 2)**.
- **r1 merged the rollback cases (r1 §D7/§8).** r2 layers root-success-vs-child-revert vs whole-tx-failure explicitly
  and adds the matching layered tests. See **§4 D7** and **§8**.
- **Line references corrected** to the baseline: `screen_tx` = `handle.rs:273`, `screen_existing` = `handle.rs:463`.

### 0.2 What changed in r4 (responses to the Stage-5.0 MR CHANGES_REQUESTED — five P2 items)

r3's design was approved, and the r3 *implementation* at `fb43bf28` is directionally correct (depth-gate removal,
`CreateOutcome.address` rollback, deps/optimism pointer unchanged, test-35 blob unchanged — all confirmed by the
reviewer). r4 does **not** change the approved D1–D9 + D7a mechanism; it closes five implementation-acceptance gaps the
building found at `fb43bf28`, each grounded below in the real `fb43bf28` source and each turned into an explicit
acceptance-criteria + test-ID table in **§13 r4** so Stages 2.0/3.0 can implement and Stage 5.0 can verify item by item:

1. **Capture-window isolation of the new `create_recipients` state** — the r3 impl pushes/pops `create_recipients` and
   derives the created address + runs `debug_assert_eq!` *ungated*, so inactive/gasless paths now allocate and mutate
   capture-private state, breaking the strict inactive no-op (D-ISO in §4, §5, §8 T1a–T1d).
2. **7702/DELEGATECALL acceptance basis corrected** — JOVIAN maps to PRAGUE (op-revm `spec.rs:43`), so EIP-7702 is
   ACTIVE; the "JOVIAN does not activate 7702" waiver is wrong. The design now *requires* a 7702-delegated-origin test
   and a real native CALL/CREATE inside a DELEGATECALL chain (not an empty DELEGATECALL) — or exact run-config/deny-path
   evidence (§4 D1, §8 T2a–T2c, §13 r4 item 2).
3. **Original-NT2 + rule-end end-to-end coverage** — the fix must be proven on the real *screening* path (through
   `screen_tx` + a `transfer.*` rule that denies both top-level and internal), RED→GREEN, not only on the capture
   stream; the pre-existing `internal_create_endowment_captured_through_composite` (comment-only change this round) is
   not valid RED→GREEN; r3 §8.7/§8 combinations are mapped to named tests (§8 T3a–T3f, §13 r4 item 3).
4. **Full D7a evidence** — add internal-CREATE2 nonce-overflow, prove a revoked observation still consumes the native
   budget (`limit+1` still denies), and assert the EXACT real created address for successful root CREATE and internal
   CREATE/CREATE2 (standard txs have no top-level CREATE2 — do not fabricate one). `debug_assert_eq!` is debug-only and
   does not substitute a release-behavior acceptance test (§4 D7a, §8 T4a–T4e, §13 r4 item 4).
5. **Per-SHA evidence contract** — pipeline 14799173 ran setup/format/clippy + 3 security jobs only (no product-test
   job), so 146/146 + context 12/12 are unverified claims. The design and the plan now REQUIRE a readable per-SHA
   `cargo nextest` log with per-test-ID results for the new head SHA, and an explicit retained-vs-re-tested mark for
   test-35 M1/M2/M3 (§8.E evidence contract, §13 r4 item 5).

## 1. Intent and why

The RCS Filter (xlayer-reth's risk-control rule engine) screens real-user candidate transactions during block-building
simulation. MR77 added observation of internal native-token (OKB) transfers as virtual
`Transfer(address,address,uint256)` logs (native-asset address `0xEeee…EEeE`), injected only into the screening input,
interleaved in execution order with real logs; receipts/RPC/bloom/indexer/consensus are unchanged.

Testing after MR77 showed NT1 (internal CALL), NT3 (SELFDESTRUCT) and NT4 (positive-control allow) pass, but:

- **Problem 1 (bug):** NT2 — an internal CREATE carrying a non-zero endowment — is NOT denied even though chain state
  proves the CREATE happened (probe nonce 1→2, created-contract balance = 1 ETH, receipt success).
- **Problem 2 (design unification):** top-level tx.value and internal transfers use two different rule-input surfaces,
  so a single RCS rule cannot uniformly cover both.

Both must be fixed. The why: risk-control rules must reliably see every native-value movement a candidate transaction
performs — top-level and contract-internal alike — through one matching surface, with no gap and no double-count, so a
single `transfer.*` rule can screen all native OKB flows before the builder commits state.

## 2. Background — the MR77 capture architecture (preserved, cited)

Current approved MR77 behavior that this revision preserves except where a decision below changes it. Source: MR77
description (AC1–AC8) + verbatim source at 714e1293.

- **Where it runs:** the RCS Filter real-user candidate simulation in
  `crates/builder/src/flashblocks/context.rs::execute_best_transactions`. An explicitly-enabled inspector collects
  observations only between `start_capture` and `finish_capture`. Filter-disabled, gasless pre-check, sequencer, cached
  flashblocks, builder self-generated txs and `xlayer_auditTransactions` collect nothing (AC1).
- **Inspector:** `crates/builder/src/flashblocks/rcs_capture.rs::RcsInspector` — the inner inspector of
  `alloy_op_evm::OpEvm`'s composite wrapper. One ordered queue of real + virtual logs with per-frame checkpoints, a
  single non-rolling-back native-transfer counter, and a sticky budget overflow.
  - `NATIVE_ASSET_ADDRESS = 0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE` (rcs_capture.rs:25); matched by address value.
  - `native_transfer_log(from,to,value)` builds the virtual Transfer log: topic0 =
    `keccak256("Transfer(address,address,uint256)")`, topic1/2 = left-padded from/to, data = 32-byte value
    (rcs_capture.rs:45-49).
  - **Single funnel** `observe_native_transfer(from,to,value,kind)` (rcs_capture.rs:180-200): inactive gate → drop
    zero/self → `observed_count += 1` → sticky overflow at `limit+1` (not appended) → else push a `Virtual` log. All of
    CALL/CREATE/SELFDESTRUCT go through this one funnel so counting/filtering/overflow never diverge.
  - **Hooks** (`impl Inspector for RcsInspector`, rcs_capture.rs:375-445): `log` records a real log (:380);
    `call`/`create` push a frame checkpoint on entry and feed the decision (:384,:405); `call_end`/`create_end` pop the
    checkpoint and truncate on revert/halt (:401,:431); `selfdestruct` observes beneficiary transfer, counted even in
    the root frame (:440). The `call`/`create` hooks are invoked for **every** frame **including the outermost
    (transaction-level) frame** — which is precisely why MR77 needed an explicit *top-level exclusion* (`depth > 0`) to
    keep the top-level value out; r2 removes that exclusion (§4 D1/D2).
  - `finish_capture` (rcs_capture.rs:295-360) returns `CaptureOutcome::Complete(stream)` only when the RealLog
    cross-check passes (the real-log subsequence of the queue equals `result.logs()` for count/content/order); otherwise
    `LimitExceeded` or `InvariantViolation{RealLogMismatch}` — both fail closed. Every exit path does a uniform full
    state clear (R9 item 4).
- **Screening input:** `crates/builder/src/flashblocks/context.rs:986-1001` — on `CaptureOutcome::Complete(logs)` it
  calls `filter.screen_tx(ScreenInput{ tx_hash, origin: tx.signer(), tx_to: tx.to(), nonce, value: tx.value(),
  block_height, logs })`; every non-Complete outcome maps to a fail-closed `Screen::Deny` via `fail_closed_screen`
  (never falls back to raw logs).
- **Filter entry points (baseline line refs, corrected per r1 review):** `FilterHandle::screen_tx` =
  `crates/rcs-filter/src/handle.rs:273`; the existing-tx path `screen_existing` = `handle.rs:463`. `ScreenInput` is
  defined at `handle.rs:82` (fields `tx_hash, origin, tx_to: Option<Address>, nonce, value: U256, block_height,
  logs: &[Log]`).
- **Rule bindings:** `crates/rcs-filter/src/matching.rs:230-265` `build_bindings` exposes tx-level vars
  `contract_address` (= tx_to, or null for creation), `origin`, `value` (= `ScreenInput.value`), `nonce`; plus
  per-event `<name>.<param>` / `<name>.address` decoded from the logs (real + virtual).
- **Budgets (AC5):** two independent options `max_native_transfers_per_tx` and `max_event_bindings_per_tx`, both default
  10000, both `>= 1` (0 is a startup error even when the Filter is disabled).
- **Fail-closed loop-level test 35** (AC4/AC6, R10→R12): proves the real candidate loop takes `Screen::Deny` on a
  runtime RealLogMismatch with no commit / no pending-buffer / no partial action, via `#[cfg(test)]` seams
  (`LAST_SCREEN_DECISION`, `FORCE_REALLOG_MISMATCH`), mutation-resistant (M1/M2/M3). This safety evidence must be
  retained; this revision must not weaken it.

## 3. Problems — root causes (code-grounded)

### Problem 1 — nested CREATE endowment missed (bug; NT2)

`record_create_observation` (rcs_capture.rs:225-240) and the `create` hook (rcs_capture.rs:405-429) both gate on
`depth > 0` using `context.journal().depth()`:

```text
// create hook (rcs_capture.rs:410-412)
let depth = context.journal().depth();
let value = inputs.value();
if depth > 0 && !value.is_zero() { /* …compute created_address, record_create_observation… */ }
```

The **empirically observed** fact is the asymmetry: NT1 (nested CALL) passes while NT2 (nested CREATE) fails, so
`journal().depth()` as read inside the `create` hook does not reliably distinguish a nested CREATE from a top-level one.
The requirement document explains this as a hook/checkpoint **timing** skew (the `create` hook firing before the new
frame's journal checkpoint while the `call` hook fires after). That timing explanation is treated here as the
requirement doc's **hypothesis to be confirmed RED-first in the implementation stage**, not as an independently proven
fact: in the pinned revm the inspector dispatch path is `frame_start` before `frame_init` (revm-inspector 21.0.3
`traits.rs:104-111`; `handler.rs:179-188` dispatches `call`/`create` together) with the CALL checkpoint in
revm-handler 20.0.3 `frame.rs:179-180`, so the exact ordering must be pinned down by the RED reproduction. Crucially,
**the r3 fix (D1) does not depend on the precise timing mechanism** — removing the depth-based decision entirely and
observing every frame's real native value is correct whatever the exact hook ordering turns out to be; the RED NT2
reproduction (§8 item 1) both proves the current gap and pins the mechanism.

### Problem 2 — top-level tx.value not unified (design)

Top-level tx.value is matched only via the tx-level `value` binding (from `ScreenInput.value`; matching.rs:241), while
internal transfers are matched via `transfer.*` event bindings decoded from virtual logs. Two surfaces ⇒ a rule cannot
cover top-level + internal with one `transfer.*` condition. The MR77 design (AC2) deliberately emitted no virtual log
for top-level tx.value; this revision changes that.

## 4. Resolved design decisions

**Core insight (r2):** both problems are resolved by the *same* minimal change — **stop special-casing the top-level
frame and stop classifying frames at all; observe the real native value of every frame through the single funnel.**
The top-level value is simply the root frame's own transfer; nested-create endowment is simply a non-top frame's
transfer. Removing the `depth`-based top-level exclusion fixes NT2 (nested CREATE is no longer dropped) and unifies the
top-level value (the root frame now emits a `transfer.*` log). No `caller`/`origin` identity test and no hook-time
`journal().depth()` are used for the decision.

### D1 — Remove top-level/internal classification; observe every frame's real native value [NEW DECISION; replaces r1 §D1 `caller==origin`]

A CALL/CREATE/SELFDESTRUCT frame contributes an observation iff it moves real native value, decided **only** by the
value the hook already carries — never by frame identity and never by `journal().depth()`:

- `call` hook: observe iff `inputs.transfer_value()` is `Some(v)` with `v != 0`. `transfer_value()` is `Some` only for a
  real `CallValue::Transfer`, so `DELEGATECALL`/`STATICCALL` (apparent value → `None`) are never observed, and
  `CALLCODE` (caller == target) is dropped by the funnel's self-transfer filter. `from = inputs.transfer_from()`,
  `to = inputs.transfer_to()`.
- `create` hook: observe iff `inputs.value() != 0`. `from = inputs.caller()`, `to = created_address` (D4).
- `selfdestruct`: unchanged (observes the beneficiary transfer; funnel filters zero/self).

**Why this is correct and reliable (reviewer item 1):** every observation corresponds to an *actual* native-value
movement performed by *that* frame. There is nothing to classify: the top-level frame's value is the top-level
transfer; a nested frame's value is a nested transfer. This is immune to the two failure modes the reviewer cited:
- **EIP-7702 delegated EOA** (revm-handler 20.0.3 `pre_execution.rs` allows a delegated EOA to initiate a tx;
  `execution.rs` resolves the delegated code): `tx.origin` is irrelevant here because we never compare against it; the
  root frame's own `call`/`create` value is observed regardless of whether the origin carries delegated code, and
  nested frames it spawns are observed by their own hooks.
- **`DELEGATECALL` inherits the caller** (revm-interpreter 37.0.3 `instructions/contract.rs`): a nested `DELEGATECALL`
  frame can have `caller == origin`, which would have fooled r1's identity test — but `DELEGATECALL` carries apparent
  value (`transfer_value()` → `None`) so it is correctly never observed, and any real `CALL`/`CREATE` it performs is
  observed on its own real value. No ancestry/identity reasoning is required at all.

**Why the root frame is captured (the one real dependency, proven by test, not assumed):** revm invokes the inspector
`call`/`create` hooks for the outermost frame too — this is exactly why MR77 had to add a `depth > 0` *exclusion* to
keep the top-level out, and why `create_top_level_generates_no_event` exists as an r1 unit test. r2 removes that
exclusion. This dependency is made falsifiable by the mandatory real-OpEvm tests in §8 (top-level CALL/CREATE emit a
transfer; NT2 nested CREATE denied) rather than asserted. The in-capture frame bookkeeping that still matters —
`push_checkpoint`/`pop_checkpoint` balance for rollback — is audited explicitly in D7/§8.

**Removed code gates:** delete the `depth > 0` gate in the `create` hook (rcs_capture.rs:412) and in
`record_create_observation` (rcs_capture.rs:232), and the `depth > 0` gate in `record_call_observation`
(rcs_capture.rs:215). The `depth` parameter is dropped from both decision helpers (they become value-only decisions),
so the hook-timing of `journal().depth()` can no longer affect the result. (Rejected alternative of keeping a
frame-ancestry marker instead of removing classification: §6 R-B.)

### D2 — Top-level native transfer is the root frame's own observation [NEW DECISION; replaces r1 synthesis]

Because classification is removed (D1), the top-level non-zero tx.value is captured by the **root frame's own
`call`/`create` hook** — there is no separately synthesized event:

- Top-level CALL (`tx.to = Some(addr)`): the root `call` hook observes `Transfer(origin, tx.to, tx.value)` because
  `transfer_from() == origin`, `transfer_to() == tx.to`, `transfer_value() == Some(tx.value)` for the outermost call.
- Top-level CREATE (`tx.to = None`): the root `create` hook observes `Transfer(origin, created_address, tx.value)`
  (D4).

This removes the r1 double-count risk entirely: each native movement is observed exactly once, by the single frame that
performs it (the reviewer's "no duplicate top/internal occurrences"). Values that genuinely pass through several frames
(e.g. origin→proxy→impl each forwarding value) produce several *distinct* transfers, which is correct and consistent
with the no-dedup rule (D8/AC3).

### D3 — ScreenInput.value: RETAIN + deprecate (do not remove in this change) [confirmed retained by reviewer; claim re-scoped]

Keep `ScreenInput.value` and the tx-level `value`/`contract_address`/`origin`/`nonce` bindings; mark `value` as
"retained for backward compatibility; prefer `transfer.*` for new rules". **Scoped claim (reviewer item 2):** retaining
`value` only guarantees the tx-level `value`/`contract_address` bindings are *computed* unchanged. It does **not** mean
a legacy rule's *overall observed result* is unchanged, because D1/D2 change the **log set**: the `transfer.*` logs now
include the top-level transfer and nested-CREATE endowments that were absent before. A rule that references `transfer.*`
(directly or via an alias) is re-evaluated against this superset and may legitimately match more. This behavior change is
the point of Problem 2; it is called out here so the RCS side can review rule impact.

- Rationale for retain-not-remove: removing `value` is a breaking rule-schema change requiring coordinated
  RCS-service-side rule migration (AC7, a non-code precondition owned by the RCS service team) and is NOT required to
  achieve the unification goal (rules can now use `transfer.*` for everything). Retaining it is non-breaking and
  reversible; a later revision can deprecate/remove `value` once deployed rules migrate.

### D4 — Top-level contract creation without tx.to [confirmed retained by reviewer]

For a top-level CREATE (deployment tx, `tx.to == None`) with non-zero value, the endowment recipient is the created
contract address, which the `create` hook already derives (nonce-derived for CREATE, salt/init-code for CREATE2;
rcs_capture.rs:414-425). Emit `Transfer(origin, created_address, tx.value)`. Do NOT substitute the zero address or null
(rejected: misleading `transfer.to`; §6 R-D). The tx-level `contract_address` binding remains null for creations
(unchanged). The address-derivation must be covered by a precise `CreateOutcome`/nonce-address test (§8 item 2).

### D5 — Ordering

Hooks fire in execution order; the root frame's hook fires before any inner frame or log, so the top-level transfer is
the first entry of the ordered stream handed to `screen_tx`: `[top-level transfer, …real + internal-virtual logs in
execution order]`. The RealLog cross-check is unaffected (it ignores virtual logs).

### D6 — Budget counting [confirmed uniform by reviewer; matcher-budget wording corrected]

The top-level transfer flows through `observe_native_transfer` and therefore counts against
`max_native_transfers_per_tx` like any other observation (uniform single counter; it is at most +1 and deterministic).

**Matcher-binding budget, stated precisely (reviewer item 2):** per `matching.rs:128-139` the complete-binding count
for a rule is the **product** of its per-event candidate counts (`checked_binding_count`), and these products are
**accumulated across all rules** into `complete_binding_count`, which fails closed (`Deny`) once it exceeds
`max_event_bindings_per_tx`. Adding the top-level transfer increases the candidate count of the `transfer`-shaped event
by one wherever such a log is now present; for a rule declaring the same transfer alias twice, its contribution grows
from `N²` to `(N+1)²`. The design does not change the matcher; it must be shown (§8 item 7) that realistic rule sets
stay within the default budget and that the `limit+1` boundary still denies. Both budgets keep default 10000 and `>= 1`.

### D7 — Rollback / early-failure / parent-child, layered [reviewer item 3]

Every CALL/CREATE frame (including the root) gets `push_checkpoint` on entry; `pop_checkpoint(reverted)`
(rcs_capture.rs:261-272) **truncates only the current frame's checkpoint tail** when that frame reverted/halted, and is
otherwise a balanced pop. `observed_count` never rolls back (sticky). The layered consequences — each asserted
separately in §8 — are:

1. **Root succeeds, a child reverts, parent continues:** keep the root's transfer `V`; drop only the reverted child
   frame's transfer `W`. (Not "nothing survives".)
2. **An internal parent frame reverts:** its own + descendants' events are truncated, but events of *outer* (ancestor)
   frames survive.
3. **The root frame / whole tx fails:** the root checkpoint truncates everything, so no top-level or internal transfer
   survives.
4. Zero-value and self-transfers emit nothing (funnel filter), independent of success.
5. CREATE/CREATE2 that fails early must leak neither a created-address nor an event.

The top-level transfer obeys exactly these rules via the root checkpoint — it is not emitted for a reverted/halted
top-level execution.

### D7a — Creation completion validity: gate on `CreateOutcome.address`, not only `is_ok` [NEW DECISION r3; resolves r2 CHANGES]

The `create` hook pushes the virtual endowment on frame entry (D1), *before* the frame outcome is known. r2 relied on
`create_end` truncating via `pop_checkpoint(!outcome.result.is_ok())` (rcs_capture.rs:431-437) to roll back a failed
creation. That is insufficient: in the pinned revm, a CREATE can finish with **no contract created yet still be
`is_ok`** —

- revm-handler 20.0.3 `frame.rs:272-283` `return_error` constructs a `CreateOutcome` with `address = None`; `:301-305`
  invokes `return_error(InstructionResult::Return)` when the caller nonce cannot be bumped (e.g. `nonce == u64::MAX`);
- revm-interpreter 37.0.3 `instruction_result.rs:163-166, 219-222` classify `InstructionResult::Return` as `is_ok`.

So the already-pushed virtual endowment would be **retained** even though no creation/transfer occurred, and the RealLog
cross-check cannot catch it (it ignores virtual logs). (Checked read-only against the baseline Cargo.lock crate
checksums; the exact handler path is to be confirmed by the §8 item 6a RED test, not assumed.)

**Decision:** a CREATE/CREATE2 observation's *final* validity is gated on the real `CreateOutcome.address`:

1. At `create_end`, treat the frame as not-created when `outcome.address.is_none()` **regardless of `is_ok`** — i.e.
   the create frame's checkpoint is truncated (its virtual endowment revoked) when the outcome produced no address, in
   addition to the existing revert/halt truncation. Concretely the "reverted" signal for a CREATE frame becomes
   `!outcome.result.is_ok() || outcome.address.is_none()`.
2. `observed_count` still never rolls back (sticky) — a revoked observation does not decrement the budget counter.
3. On a successful creation, assert the recipient the hook recorded (the pre-derived `created_address`) equals the
   actual `outcome.address`; a mismatch is a bug and must fail the test (and, defensively, must not surface a transfer
   to a wrong address).

This keeps "no leak on CREATE early-failure" true for the `is_ok` early-return path the reviewer identified, not only
for the revert/halt path. It is within the already-agreed CREATE early-failure no-leak scope (no new Jira/Player
approval). If the active OP execution path already guarantees no such leak, the implementation must prove it with the
exact covering source + a real regression result rather than inferring success from `is_ok` (§8 item 6a).

### D-ISO — The new create-recipient bookkeeping obeys the inactive no-op invariant [NEW DECISION r4; resolves Stage-5.0 item 1]

D7a requires tracking the per-create pre-derived recipient so `create_end` can assert address consistency. The r3
implementation at `fb43bf28` added a `create_recipients: Vec<Option<Address>>` and drove it **directly from the hooks,
not through the active gate**:

- `create` (fb43bf28 `rcs_capture.rs:400-426`) always runs `self.create_recipients.push(Some(created_address))` (or
  `push(None)` for a zero endowment) and, for a non-zero endowment, derives `created_address` by reading
  `context.journal_ref()` — **before** any `if !self.active` check;
- `create_end` (`:428-450`) always runs `self.create_recipients.pop()` and the `debug_assert_eq!` (`:442`).

Every *other* capture mutation is gated — `observe_native_transfer` (`:193`), `record_real_log` (`:237`),
`push_checkpoint` (`:244`), `pop_checkpoint` (`:255`) each start with `if !self.active { return; }`. So the new
`create_recipients` path is the sole exception: on the inactive/gasless pre-check, sequencer, cached-flashblocks,
builder-self and `xlayer_auditTransactions` paths, a `create` frame now allocates/pushes onto a capture-private Vec and
reads the journal to derive an address, violating the strict inactive no-op the module header asserts
(`rcs_capture.rs:367-370`) and AC1 / r3 §2.

**Decision:** the create-recipient bookkeeping is capture-window-scoped exactly like all other capture state.

1. The `create` hook performs **no** `create_recipients` mutation and **no** created-address derivation while
   `!self.active`. Confine the push + derivation inside the active gate (e.g. an early `if !self.active { return None; }`
   at the top of `create`, mirroring the helper pattern, or route the push through an active-gated helper). On the
   inactive path a `create` frame does zero allocation and zero capture-private mutation — a true no-op.
2. The `create_end` hook performs **no** `create_recipients.pop()` and runs no `debug_assert_eq!` while inactive. Push
   and pop stay balanced: because the push is active-gated, the pop must use the identical gate so the Vec never
   underflows and never carries a stale entry into the next window.
3. `start_capture`/`finish_capture`/`abort_capture` already `clear()` `create_recipients` (fb43bf28 `:283,:350,:360`);
   keep that so no entry leaks across windows. The clean-state assertion must *include* `create_recipients` (see §8 T1c).
4. The zero-endowment case must not push a placeholder on the inactive path at all; within an active window the chosen
   representation (whether zero endowments push a `None` sentinel or are skipped) is an implementation detail left to the
   builder, provided push/pop stay balanced across success/revert/early-failure/abort and consecutive txs (§8 T1a–T1d).

This is strictly a *scoping* correction to D7a's bookkeeping; it does not change observable transfer behavior inside an
active window. It stays within the approved CREATE-early-failure/no-leak + no-op scope — no new Jira/Player approval.

### D8 — No dedup; zero/self filtered (unchanged)

Identical `(from,to,value)` transfers are NOT deduplicated (AC3) — unchanged. Zero-amount and self-transfers never
qualify (funnel `value.is_zero() || from == to`) — unchanged; a top-level `tx.value == 0` or self-send emits no
top-level transfer.

### D9 — Receipt/RPC/consensus unchanged (unchanged)

Virtual native Transfer logs (top-level and internal) are injected only into the screening input. They never reach
receipts, RPC logs, bloom, the indexer, or consensus/commit state. No change to this invariant.

## 5. Affected files / interfaces / callers (blast radius)

Grounded in the codegraph index at 714e1293 (relations) + verbatim reads (line numbers):

- `crates/builder/src/flashblocks/rcs_capture.rs` — the only production change site.
  - `create` hook (:405-429): drop the `depth > 0` gate; still derive `created_address` and call the create decision
    whenever `value != 0`.
  - `call` hook (:384-399): drop the top-level `depth > 0` exclusion; observe whenever `transfer_value()` is a non-zero
    `Some`.
  - `record_call_observation` (:208-220) / `record_create_observation` (:225-240): drop the `depth`
    parameter and the `depth > 0` gate; become value-only decisions funnelling into `observe_native_transfer`.
  - `create_end` (:431-437): gate the frame's truncation on `!outcome.result.is_ok() || outcome.address.is_none()`
    (D7a), so an `is_ok` early-return that produced no contract (`address == None`) revokes the frame's virtual
    endowment; add a success-path assertion that the recorded `created_address` equals `outcome.address`.
    `observed_count` stays sticky. **[r4 D-ISO]** the `create_recipients` pop + the consistency assertion are
    active-gated (no `pop`/assert while `!self.active`), balancing the active-gated push in `create`; the success-path
    exact-address check must hold in release builds, not only via `debug_assert_eq!` (§13 r4 item 4, T4c–T4e).
  - `create` hook **[r4 D-ISO]**: the `create_recipients` push and the created-address derivation (journal read) are
    confined to the active window — a `create` frame on the inactive/gasless path does zero capture-private mutation
    and zero derivation (mirrors the `if !self.active { return; }` guard used by every other capture mutation). See §4
    D-ISO and §13 r4 item 1 (T1a–T1d).
  - `record_create_observation` has 4 internal callers (incl. unit tests, which this revision's test plan updates);
    hooks are invoked by OpEvm; all within this crate. The `RcsCaptureControl` seam (:286) and `finish_capture`
    cross-check (:295) are unchanged, so rcs-filter gains no new dependency and the RealLog invariant is preserved.
- `crates/builder/src/flashblocks/context.rs` — **no structural change**: `screen_tx` already consumes the full
  `Complete(logs)` stream (:986-1001); the top-level transfer arrives inside that stream (D2/D5). `ScreenInput.value`
  kept (D3). The test-35 seams (`LAST_SCREEN_DECISION`, `FORCE_REALLOG_MISMATCH`) remain intact.
- `crates/rcs-filter/src/handle.rs`, `matching.rs` — **no code change** (D3 retains `value`); the top-level transfer is
  matched via existing `transfer.*` decoding. `screen_tx`=handle.rs:273, `screen_existing`=handle.rs:463,
  `build_bindings`=matching.rs:230 all unchanged. Any new/updated RCS rules are an RCS-service-side (AC7) concern.

Net builder-side change is localized to `rcs_capture.rs` (remove gates / value-only decisions) plus tests. No
upstream-reth modification; extension stays via the inspector/trait seam.

## 6. Rejected / considered alternatives

- **R-A: Keep `journal().depth()`, special-case the CREATE hook timing.** Rejected — fragile, revm-version-dependent,
  and the hook-timing skew is the bug itself.
- **R-A′: Classify by `caller == tx.origin` (r1 §D1).** Rejected per the building's r1 review — unreliable under
  EIP-7702 delegated-EOA execution and `DELEGATECALL` caller inheritance; identity equivalence is not frame ancestry.
- **R-B: Keep a top-level marker but track it via an in-capture frame-ancestry counter (checkpoint-stack depth) instead
  of removing classification.** Considered (it is also reviewer-acceptable). Rejected as unnecessary: once we observe
  every frame's real value (D1), there is no behavior that needs a root marker, and a second depth-like counter is extra
  state with its own balance obligation. Removing classification is simpler and strictly fewer moving parts.
- **R-C: Synthesize the top-level transfer in `context.rs` by prepending to the stream.** Rejected — `context.rs` lacks
  the top-level CREATE's created address (needed for D4) and it would split the single funnel (two emission sites ⇒
  drift risk on counting/ordering/rollback).
- **R-D: Use zero/null recipient for top-level CREATE.** Rejected — misleading `transfer.to` (D4).
- **R-E: Remove `ScreenInput.value` now (hard unification).** Rejected for this change — breaking rule-schema change
  requiring cross-team RCS rule migration (AC7); deferred (D3).
- **R-F: Exempt the top-level transfer from the native budget.** Rejected — a second counting rule risks drift; uniform
  counting is simpler and the +1 is harmless (D6).

## 7. Constraints

- Only the two approved changes + necessary tests/spec. Reuse MR77 and branch `xl/xlop-1217/0928-1135`; preserve others'
  edits and the `deps/optimism` submodule pointer. No merge/deploy/chain action, no extra repo scope, no new Jira.
- Must pass repository-required formatting/lint: `cargo fmt` (scoped per package, never `--all`) and
  `cargo clippy --all-targets --workspace -- -D warnings`.
- Retain the test-35 safety evidence and all existing passing controls; do not weaken fail-closed behavior.
- Virtual logs never enter receipt/RPC/bloom/indexer/consensus (D9).

## 8. Test plan (executed in the implementation stage, not this design stage)

Real production OpEvm hook integration is mandatory (not just unit stubs) — the bug is a hook-timing artifact, and the
root-frame-hook dependency (D1/D2) must be proven, not assumed.

1. **NT2 reproduce → fix** (gating `OpEvm.transact`): internal CREATE with non-zero endowment is denied; add a RED
   reproduction first (current code fails to deny), then prove GREEN after D1. Mirror NT1 (CALL) for symmetry.
2. **CREATE2 + address derivation:** nested CREATE2 endowment captured with the correct `created_address`
   (salt/init-code derived); a precise `CreateOutcome`/nonce-address assertion for CREATE (D4).
3. **Top-level unification:** top-level CALL emits `Transfer(origin, tx.to, value)` **first** in order; top-level CREATE
   emits `Transfer(origin, created_address, value)`; a single `transfer.*` rule denies both top-level and internal. This
   test is what proves the root frame's hook fires (D1/D2) — it fails loudly if it does not.
4. **Identity-independence (reviewer item 1):** a **7702-delegated-origin** transaction whose origin executes a nested
   CALL and a nested CREATE, and a **DELEGATECALL chain** that performs a real CALL/CREATE, each produce the correct
   transfers with no mis-capture and no omission — demonstrating the design does not depend on `caller == origin`. If
   the runtime configuration excludes 7702 on this chain, record the concrete run-config evidence (chainspec/activation)
   instead of assuming an EOA origin.
5. **No duplicate top/internal:** exactly one occurrence per movement; correct from/recipient/value; `value` binding
   still present and consistent (D3); value forwarded origin→proxy→impl yields the expected *distinct* transfers.
6. **Rollback/early-failure, layered (reviewer item 3):** (a) root V success + child W reverts + parent continues → keep
   V, drop W; (b) internal parent revert → outer events survive; (c) whole-tx/root failure → nothing survives;
   (d) CREATE/CREATE2 early failure → no created-address/event leak; (e) cross-tx cleanup — the next capture starts
   clean (`start_capture` reset); `observed_count` stays sticky across (a)–(d).
6a. **CREATE `is_ok` early-return no-leak (reviewer r2 item; D7a):** a non-zero root value V plus an internal
   CREATE/CREATE2 whose caller nonce = `u64::MAX` (handler early-returns `CreateOutcome{address:None}` with an `is_ok`
   `Return`), parent continues and succeeds → the stream keeps V and does NOT contain the non-occurring endowment, even
   though `outcome.result.is_ok()`. Also cover the existing out-of-funds / init-code revert / address-collision cases,
   and the success path asserting the recorded `created_address == outcome.address`. If the active OP path already
   prevents this leak, substitute the exact covering source + a real regression result (do not infer from `is_ok`).
   `observed_count` stays sticky through the revocation.
7. **Budgets fail-closed (reviewer item 2):** legacy `value`-condition rule still fires under the new log set; a rule
   with the Transfer ABI as two aliases shows the `N²→(N+1)²` combination growth; the `max_event_bindings_per_tx` and
   `max_native_transfers_per_tx` boundaries deny at `limit+1`; multiple audit rules of the same and of different
   `audit_type` merge actions per `(audit_type, log_index)` (matching.rs:163-171) with no empty-action Audit
   (matching.rs:156-159). Both budgets default 10000, `>= 1`.
8. **Consistency:** first-screen vs post-approval re-simulation identical; Filter-disabled/inactive/gasless/
   sequencer/cached/self-generated produce zero capture (AC1).
9. **Safety retention:** test 35 (loop-level fail-closed, M1/M2/M3 mutation-resistant) still RUN+PASS; RealLog
   cross-check still fails closed. Record real `cargo test` commands/results — do not reuse the old MR green record as
   this change's acceptance evidence.

### 8.r4 — Named test IDs for the five Stage-5.0 items (authoritative mapping for 2.0/3.0/5.0)

The §8 items above remain in force. The table below is the authoritative, verifiable test-ID set for the five P2 items;
each row is a NEW or STRENGTHENED test for this round. "RED→GREEN" means the test must be shown failing on the pre-fix
code and passing after the fix. Full acceptance criteria per item are in **§13 r4**. Crate paths: capture-stream +
real-OpEvm hook tests live in `crates/builder/src/flashblocks/rcs_capture.rs` `#[cfg(test)]`; screening/matcher
end-to-end tests live in `crates/rcs-filter/src/integration_tests.rs` (the only covering tests for `screen_tx` per the
codegraph blast-radius); gating `execute_best_transactions` loop tests live in `context.rs`.

| ID | Item | What it proves | Kind |
|---|---|---|---|
| T1a | 1 | inactive inspector: a real `create` hook (non-zero + zero endowment) causes NO `create_recipients` mutation, NO journal read, empty stream | real-OpEvm, extends `inactive_inspector_ignores_all_observations` (:507) to drive `create` |
| T1b | 1 | gasless/disabled path (never `start_capture`) + a CREATE tx: zero capture-private mutation; `create_recipients` stays empty | real-OpEvm |
| T1c | 1 | `assert_capture_state_is_clean` (:689) also asserts `create_recipients.is_empty()`; holds after Complete/LimitExceeded/InvariantViolation/abort | unit |
| T1d | 1 | push/pop balance across success, child-revert, CREATE early-failure, `abort_capture`, and two consecutive txs — no underflow, no stale entry | real-OpEvm |
| T2a | 2 | a **7702 delegated-origin** tx (SET_CODE / type-0x04 or delegated EOA) whose origin performs a nested CALL and a nested CREATE → correct transfers, no mis/omission | real-OpEvm, 7702-active spec |
| T2b | 2 | a **DELEGATECALL chain with a real inner CALL and a real inner CREATE carrying value** → the inner transfers are observed on their own real value (the current empty-inner test is retained but does NOT satisfy this) | real-OpEvm |
| T2c | 2 | hardfork-basis evidence: assert the active spec enables 7702 (JOVIAN⇒PRAGUE); if 7702 is deliberately disabled on the target chain, record the exact chainspec/run-config/deny-path instead — NOT a JOVIAN assumption | config evidence / test |
| T3a | 3 | **original NT2** (internal CREATE non-zero endowment) denied through the real `screen_tx` screening path by a `transfer.*` rule — RED on pre-fix, GREEN after; distinct from the capture-stream-only `internal_create_endowment_captured_through_composite` | integration (rcs-filter) |
| T3b | 3 | ONE `transfer.*` rule denies BOTH a top-level transfer and an internal transfer in the same screening run | integration |
| T3c | 3 | legacy `value`-condition rule still fires under the new (superset) log set; a Transfer-ABI rule aliased twice shows `N²→(N+1)²` combination growth; `max_event_bindings_per_tx` / `max_native_transfers_per_tx` deny at `limit+1` | integration |
| T3d | 3 | same-`audit_type` multi-rule merges one action per `(audit_type, log_index)`; different-`audit_type` each carry their own action; no empty-action Audit | integration |
| T3e | 3 | first-screen (`screen_tx`) vs post-approval re-simulation (`screen_existing`) produce identical decisions on the same tx | integration |
| T3f | 3 | state explicitly whether `matching.rs`/`handle.rs` need a real code change: design position is **no** matcher/handle code change (D3/§5); T3a–T3e must prove the end-to-end behavior through the UNCHANGED matcher, since a comment-only `matching.rs` diff + generic matcher unit tests do NOT constitute end-to-end evidence | design assertion + tests |
| T4a | 4 | internal **CREATE2** nonce-overflow (`caller nonce = u64::MAX`) → `CreateOutcome{address:None, is_ok:true}` endowment revoked; only the surviving transfers remain | real-OpEvm |
| T4b | 4 | a revoked observation STILL consumes the native budget: with `max_native_transfers_per_tx = N`, a root transfer + a revoked failed-create at the boundary makes `observed_count = N+1` ⇒ fail-closed `LimitExceeded`/Deny (sticky, non-rolling-back counter) | real-OpEvm |
| T4c | 4 | successful **root CREATE**: assert the EXACT real created address `== outcome.address` (not merely `!= caller/!= zero`) | real-OpEvm |
| T4d | 4 | successful **internal CREATE** and **internal CREATE2**: assert the EXACT real created address `== outcome.address` | real-OpEvm |
| T4e | 4 | release-behavior acceptance (not a `debug_assert`): a wrong pre-derived recipient must not surface a transfer to a wrong address in a release build; keep existing root-V/child-W layered rollback tests | real-OpEvm |

### 8.E — Evidence contract [NEW r4; resolves Stage-5.0 item 5]

Stage 3.0/5.0 MUST produce, for the NEW MR head SHA (not fb43bf28, not the old MR green record):

1. A **readable per-SHA `cargo nextest` log** captured at that exact SHA, with **per-test-ID pass/fail** for every T1a–T4e
   above plus the pre-existing §8 items, attached to the MR/review (e.g. `cargo nextest run -p xlayer-builder` and the
   `crates/rcs-filter` integration tests; record the exact command, SHA, and summary counts). A unit-claimed "146/146"
   with no attached log is NOT acceptance evidence.
2. The required formatting/lint results at that SHA: `cargo +nightly fmt --all --check` (exit 0) and
   `cargo clippy --all-targets --workspace -- -D warnings` (0/0).
3. For **test-35 (M1/M2/M3 mutation-resistant, loop-level fail-closed)**: an explicit **retained-vs-re-tested** mark —
   state which of M1/M2/M3 are byte-identical to baseline source (retained) and which were re-run at the new SHA
   (re-tested), with the re-run result. Retained-without-re-run is permitted only when the covering source blob is
   proven unchanged at the new SHA; otherwise re-run and record.
4. Because the SHA will change when 3.0 re-implements, the review request MUST carry the new SHA and the per-item
   disposition; the pipeline must include an actual product-test job (or an attached local nextest log serves as the
   per-SHA evidence of record).

## 9. Non-goals / anti-goals

- **Non-goals:** merging/deploying MR77; any chain/consensus/receipt/RPC change; RCS-service-side rule authoring or
  migration (AC7); removing `ScreenInput.value` now; new Jira issues or repo scope; touching the `deps/optimism`
  submodule.
- **Anti-goals** (failure even if it "works"): fixing NT2 by any mechanism that re-introduces a depth/timing dependency
  or a `caller`/`origin` identity test; emitting the top-level transfer twice, or producing two actions for one
  movement **within the same `audit_type`** (distinct `audit_type`s each carrying one action for the same log is normal
  matcher behavior — see D6/§13); retaining a virtual endowment for an `is_ok` CREATE early-return that created no
  contract (D7a); letting any virtual log leak into a receipt/consensus; weakening the test-35 fail-closed proof;
  claiming a classifier correct without real OpEvm hook tests (incl. 7702/DELEGATECALL).

## 10. Additional Context — AC1–AC8 mapping (preserved from MR77, with r2 deltas)

- **AC1** (collection only in the enabled RCS simulation path) — preserved unchanged.
- **AC2** — changed by r2: internal CALL/CREATE/CREATE2/SELFDESTRUCT each emit a virtual Transfer (NT2 now included via
  D1), AND the top-level non-zero tx.value now also emits a virtual Transfer as the root frame's own observation
  (D1/D2/D4), reversing the MR77/R9 "top-level emits none". Zero-amount / no-net-movement still emit none.
- **AC3** (ordered interleave; revert/parent-revert/failed-tx leave no stale action; identical transfers not
  deduplicated) — preserved and layered (D7/§8 item 6); top-level transfer obeys the same ordering/rollback (D5/D7/D8).
- **AC4** (RealLog subsequence match; budget/invariant → fail-closed Screen::Deny; test 35) — preserved; budgets now also
  account for the top-level transfer (D6).
- **AC5** (two budgets, default 10000, `>= 1`) — preserved.
- **AC6** (unit + gating-integration coverage incl. test 35) — extended with NT2 reproduce/fix + top-level unification +
  7702/DELEGATECALL + layered-rollback tests (§8).
- **AC7** (RCS-service confirmation of special-address role, Transfer ABI/`transfer` alias, no-tokenId ActionItem,
  virtual non-receipt semantics) — remains a non-code precondition owned by the RCS side; `ScreenInput.value` retention
  (D3) keeps existing rules working during migration, with the re-scoped caveat that the `transfer.*` log set grows.
- **AC8** (clean baseline; submodule untouched; fmt/clippy/targeted tests green) — preserved for the new change.

## 11. Open items for the reviewer (building starport-rcs)

None blocking. For explicit confirmation during review:

- D1/D2 (remove classification; top-level value is the root frame's own observation) — confirm this is the preferred
  resolution of r1 item 1 over a frame-ancestry marker (R-B).
- D3 (retain `ScreenInput.value` + deprecate; `transfer.*` log set grows) — confirm this matches the RCS-service
  migration expectation.
- D6 (top-level counts against `max_native_transfers_per_tx`; matcher binding grows per the product rule) — confirm the
  budget treatment is acceptable.

## 12. Builder-check (self-review)

Read back as a fresh builder: the spec gives one coherent mechanism (observe every frame's real native value, no
classification) that a competent Rust/revm engineer can implement without returning to the author; every new decision is
flagged, grounded in cited source, and paired with its rejected alternative; the test plan is falsifiable (RED-first
NT2; top-level and 7702/DELEGATECALL tests prove the root-hook dependency) and preserves test 35. Residual
implementation freedom (exact fixture construction, whether 7702 is active on this chainspec) is intentionally left to
the builder and marked as such.

## 13. CHANGES_REQUESTED — item-by-item responses (r1 / r2 / r3 / r4)

### 13.r1–r3 — prior-round responses

**Item 1 — root-frame criterion (`caller==origin`) is wrong under 7702/DELEGATECALL.** Accepted. r2 **removes
classification entirely** (D1): every frame's real native value is observed through the one funnel; the top-level value
is the root frame's own observation (D2). No `caller`/`origin` identity test and no hook-time `journal().depth()` remain.
7702-delegated-origin nested CALL/CREATE and DELEGATECALL-chain cases are covered by mandatory real-OpEvm tests (§8 item
4); if 7702 is excluded on this chain, run-config evidence is required rather than an EOA assumption. The remaining
real dependency (the root frame's hook fires) is made falsifiable by §8 items 3–4, and the `push_checkpoint`/
`pop_checkpoint` balance is audited in D7/§8 item 6.

**Item 2 — action/budget boundary statements were inaccurate.** Accepted. D6 now states the matcher budget precisely
(product of per-event candidate counts per rule, accumulated across rules, fail-closed at `limit+1`; `N²→(N+1)²` for a
dual-alias rule — matching.rs:128-139), and scopes the action-merge semantics to `(audit_type, log_index)` with no
empty-action Audit (matching.rs:156-171). D3 is re-scoped: retaining `value` keeps the tx-level bindings computed
unchanged but does NOT keep a legacy rule's overall result unchanged, because the `transfer.*` log set is now a superset.
Tests added: legacy `value` rule + Transfer ABI, same/different `audit_type` multi-rule merge, combination-budget
boundary, first-screen/re-sim consistency (§8 items 5,7,8). No RCS-service expansion.

**Item 3 — rollback statements must be layered.** Accepted. D7 now layers root-success-vs-child-revert (keep V, drop W),
internal-parent-revert (outer survives), and whole-tx/root failure (clear all), grounded in `pop_checkpoint`
(rcs_capture.rs:261-272) truncating only the current frame tail with sticky `observed_count`; §8 item 6 asserts each case
under non-zero root plus CREATE/CREATE2 early-failure no-leak and cross-tx cleanup.

**r2 CHANGES_REQUESTED — response (new in r3).** The reviewer (`starport-rcs-design-review-r2-changes-20261012`, Bus
recvxP4MqPZ68s) accepted all three r1-item fixes and raised:

- **New required change — CREATE `is_ok` early-return leak.** Accepted. r2 gated creation rollback only on
  `!outcome.result.is_ok()`, which misses the pinned-revm path where `return_error(InstructionResult::Return)` yields
  `CreateOutcome{address:None}` and `is_ok == true` (nonce can't bump, e.g. `u64::MAX`). r3 adds **D7a**: gate the
  create frame's final validity on `outcome.address` (`None` ⇒ revoke the observation, `observed_count` sticky) and
  assert address consistency on success; §5 updates `create_end`; §8 item 6a adds the real-OpEvm `u64::MAX`
  early-return no-leak test (or exact covering-source + regression proof). Within the agreed CREATE early-failure
  no-leak scope; no new Jira/Player.
- **§3 timing root-cause.** Accepted. §3 now marks the `create`-before-checkpoint / `call`-after timing as the
  requirement document's hypothesis to be RED-verified (pinned inspector dispatches `frame_start` before `frame_init`;
  `handler.rs:179-188`; CALL checkpoint `frame.rs:179-180`); the empirically-observed NT1-pass/NT2-fail asymmetry
  stands, and the D1 fix does not depend on the exact ordering.
- **§9 scope.** Accepted. The "one movement ⇒ not two actions" anti-goal is scoped to the same `audit_type`; distinct
  `audit_type`s each carrying an action for the same log is normal matcher behavior (D6/§13 consistent).

**Retained items** (per reviewer): `ScreenInput.value` retained (D3); top-level creation recipient = actual
`created_address`, not zero (D4, with precise address test); root event counts toward the native budget (D6); single
funnel, event ordering, receipt/RPC isolation, and test 35 all preserved (D5/D8/D9/§7/§8 item 9). Baseline line
references corrected: `screen_tx`=handle.rs:273, `screen_existing`=handle.rs:463. No additional Player technical approval
is required; task/Jira scope is unchanged.

### 13.r4 — Stage-5.0 MR CHANGES_REQUESTED — item-by-item response (five P2 items)

Source: building `starport-rcs` reply `starport-rcs-mr77-fb43bf28-changes-20261012` (Bus recvxPLNSdwONs) + address-scope
clarification `starport-rcs-mr77-fb43bf28-address-scope-20261012` (Bus recvxPLWsBiiGd), reviewing MR77 @
`fb43bf28c7f93c0be4620ff204b63c9176b2f47c`. All five items are **P2**. Each is grounded in the real `fb43bf28` source
(read via GitLab MCP; line numbers are at `fb43bf28`) and carries explicit acceptance criteria + the §8.r4 test IDs. The
approved D1–D9 + D7a mechanism is unchanged; these are implementation-acceptance corrections, not scope changes.

### Item 1 — Capture-window isolation of `create_recipients` (P2). **Accepted → D-ISO.**

**Finding (grounded):** at `fb43bf28` the `create` hook (`rcs_capture.rs:400-426`) pushes `create_recipients` and
derives the created address + reads the journal **before** any active check, and `create_end` (`:428-450`) pops +
`debug_assert_eq!`s unconditionally. Every other mutation is gated with `if !self.active { return; }`
(`observe_native_transfer:193`, `record_real_log:237`, `push_checkpoint:244`, `pop_checkpoint:255`), so this is the one
path that allocates/mutates capture-private state on inactive/gasless runs — breaking the strict no-op asserted at
`rcs_capture.rs:367-370` and AC1. `assert_capture_state_is_clean` (`:689-697`) does not check `create_recipients`; the
inactive test (`:507-521`) never drives the real `create` hook.

**Design response:** D-ISO (in §4) scopes all create-recipient bookkeeping to the active window and keeps push/pop
balanced; §5 `create_end` bullet updated. **Acceptance criteria:**

| AC | Criterion | Test |
|---|---|---|
| 1.1 | On the inactive path, a real `create` frame (zero and non-zero endowment) performs no `create_recipients` mutation and no journal/address derivation | T1a, T1b |
| 1.2 | `assert_capture_state_is_clean` asserts `create_recipients.is_empty()` and holds on every finish/abort exit | T1c |
| 1.3 | push/pop balanced across success, child-revert, CREATE early-failure, abort, consecutive txs — no underflow / stale entry | T1d |
| 1.4 | observable transfer behavior inside an active window is unchanged vs r3 | existing §8 items 1–3 still GREEN |

### Item 2 — 7702/DELEGATECALL acceptance basis (P2). **Accepted → basis corrected; tests required.**

**Finding (grounded):** the test comment at `fb43bf28 rcs_capture.rs:1940-1942` asserts "`OpSpecId::JOVIAN` … does not
activate EIP-7702" and uses that to waive a full 7702 test. The building verified this contradicts the pinned op-revm
submodule `0dca4b893f3d85dfa02209af9ff031822c4e3043` `rust/op-revm/src/spec.rs:43`, which maps `ISTHMUS | JOVIAN =>
PRAGUE`; PRAGUE enables EIP-7702, so 7702 IS active under JOVIAN. The only DELEGATECALL test
(`delegatecall_carries_no_transfer_event:1944-1968`) uses an **empty** inner account (`account(0, None)`), proving only
that an empty DELEGATECALL emits nothing — it does not exercise a real CALL/CREATE inside delegated execution.

**Design response:** the "JOVIAN-no-7702" waiver is withdrawn; D1's identity-independence claim must be proven, not
assumed. §4 D1 already requires 7702/DELEGATECALL coverage; r4 makes it non-waivable and names the tests.
**Acceptance criteria:**

| AC | Criterion | Test |
|---|---|---|
| 2.1 | Correct hardfork basis recorded: JOVIAN⇒PRAGUE⇒EIP-7702 active (op-revm `spec.rs:43`); remove the contradictory comment | T2c |
| 2.2 | A 7702 delegated-origin tx whose origin runs a nested CALL and a nested CREATE yields the correct transfers (no `caller==origin` dependence) | T2a |
| 2.3 | A DELEGATECALL chain with a real inner CALL and a real inner value-carrying CREATE: the inner transfers are observed on their own real value; the empty-inner test is retained but insufficient | T2b |
| 2.4 | If 7702 is deliberately disabled on the target chain, exact chainspec/run-config/deny-path evidence replaces the assumption (not a JOVIAN claim) | T2c |

### Item 3 — Original-NT2 + rule-end end-to-end coverage (P2). **Accepted.**

**Finding (grounded):** `top_level_create_with_value_emits_transfer:1580` is commented "NT2" but original NT2 is an
*internal* CREATE; `internal_create_endowment_captured_through_composite:1143` pre-exists on baseline with only a comment
change this round and only inspects the **capture stream** (`run_capturing_tx`), not the real screening/matcher path, so
it is not this round's RED→GREEN. `matching.rs` only had a comment change; generic matcher unit tests do not prove the
end-to-end combinations. Per codegraph, `screen_tx` (handle.rs:273) is covered by `crates/rcs-filter/src/integration_tests.rs`.

**Design response:** the fix must be demonstrated on the real screening path, and r3 §8.7/§8 combinations mapped to named
tests. **Acceptance criteria:**

| AC | Criterion | Test |
|---|---|---|
| 3.1 | Original NT2 (internal CREATE endowment) denied through the real `screen_tx` path by a `transfer.*` rule — RED pre-fix, GREEN post-fix | T3a |
| 3.2 | ONE `transfer.*` rule denies both a top-level and an internal transfer in the same run | T3b |
| 3.3 | legacy `value` rule still fires under the superset log set; dual-alias Transfer rule shows `N²→(N+1)²`; both budgets deny at `limit+1` | T3c |
| 3.4 | same/different `audit_type` action-merge per `(audit_type, log_index)`; no empty-action Audit | T3d |
| 3.5 | first-screen vs re-simulation consistency | T3e |
| 3.6 | explicit design statement: no `matching.rs`/`handle.rs` code change is required (D3/§5); if 3.0 finds one is, it is a design deviation to re-review | T3f |

If the original fault genuinely cannot be reproduced on the current baseline, 3.0 must record the exact fixture /
deployment-baseline difference rather than substituting a top-level CREATE for the internal NT2.

### Item 4 — Full D7a evidence (P2) + address-scope clarification. **Accepted.**

**Finding (grounded):** the D7a test `create_is_ok_early_return_no_endowment_leak:1678-1735` covers only internal CREATE
nonce-overflow; it asserts `stream.len()==1` but does not prove the revoked observation still consumed the budget, has no
CREATE2 case, and the create tests assert `created != caller/!= zero` (`:1599-1601`, `:1163-1173`) rather than the exact
address. `debug_assert_eq!(:442)` is debug-only. Per the address-scope clarification (recvxPLWsBiiGd), exact-address
asserts target top-level CREATE and internal CREATE/CREATE2; **standard txs have no top-level CREATE2 — do not fabricate one.**

**Design response:** §4 D7a + §8.r4. **Acceptance criteria:**

| AC | Criterion | Test |
|---|---|---|
| 4.1 | internal CREATE2 nonce-overflow revokes the phantom endowment (address-None guard) | T4a |
| 4.2 | a revoked observation STILL consumes the native budget: root + revoked failed-create at the boundary ⇒ `limit+1` denies (sticky counter) | T4b |
| 4.3 | successful root CREATE asserts the EXACT real created address `== outcome.address` | T4c |
| 4.4 | successful internal CREATE and internal CREATE2 assert the EXACT real created address | T4d |
| 4.5 | release-behavior safety (no `debug_assert`-only reliance); keep existing root-V/child-W rollback tests | T4e |
| 4.6 | no top-level CREATE2 test is fabricated (standard txs have none) | n/a (scope guard) |

### Item 5 — Per-SHA evidence contract (P2). **Accepted → §8.E.**

**Finding (grounded):** GitLab pipeline `14799173` is bound to `fb43bf28` and succeeded, but its 6 jobs are
setup/format/clippy + 3 security checks — there is **no product-test job**, so the "146/146 + context 12/12" claims are
unverified. test-35 M1/M2/M3 were not distinguished retained-vs-re-tested.

**Design response:** §8.E makes the evidence contract binding on Stages 3.0/5.0, and the plan is updated to match.
**Acceptance criteria:**

| AC | Criterion |
|---|---|
| 5.1 | A readable per-SHA `cargo nextest` log at the NEW head SHA with per-test-ID (T1a–T4e + §8 items) pass/fail is attached to the MR/review |
| 5.2 | `cargo +nightly fmt --all --check` exit 0 and `cargo clippy --all-targets --workspace -- -D warnings` 0/0 at that SHA |
| 5.3 | test-35 M1/M2/M3 explicitly marked retained (blob proven unchanged) vs re-tested (re-run result recorded) |
| 5.4 | the review request carries the new SHA + per-item disposition; a product-test job or attached local nextest log is the evidence of record |

**r4 scope note.** All five items are implementation-acceptance corrections within the already-approved two-fix scope
(nested CREATE/CREATE2 endowment capture + unified top-level native Transfer) and the agreed CREATE-early-failure/no-op
boundary. No new Jira, no new identity, no scope expansion, no change to the D1–D9 + D7a mechanism. This remains a
design-stage artifact: no production/test code is modified here; Stages 2.0/3.0 implement and 5.0 verifies against the
tables above. approval_mode = delegated_agent; no human signature claimed.
